use std::sync::Arc;
use tokio::sync::Mutex;
use std::path::PathBuf;
use tracing::{info, error};
use crate::{AppState, AppWindow, i18n};
use crate::store_create::plan::{
    choose_strategy, CapabilityProbe, CleanupPlan, StoreCreateJournal, StoreCreatePhase,
    StoreCreateRequest,
};
use crate::ui::data::refresh_distros_ui;
use super::{sanitize_instance_name, generate_random_suffix};

fn supports_named_install_error(output: &str, error: &str) -> bool {
    let combined = format!("{}\n{}", output, error).to_lowercase();
    combined.contains("--name")
        && (combined.contains("invalid")
            || combined.contains("unknown")
            || combined.contains("option")
            || combined.contains("参数")
            || combined.contains("选项")
            || combined.contains("usage"))
}

pub async fn perform_install(
    ah: slint::Weak<AppWindow>,
    as_ptr: Arc<Mutex<AppState>>,
    source_idx: i32,
    name: String,
    friendly_name: String,
    internal_id: String,
    install_path: String,
    file_path: String,
) {
    info!("perform_install started: source={}, name={}, friendly={}, internal_id={}, path={}", 
          source_idx, name, friendly_name, internal_id, install_path);

    // Guard against UI thread blocks - yield initially
    tokio::task::yield_now().await;

    // 2. Setup initial state and manual operation guard
    let (dashboard, executor, config_manager, distro_snapshot) = {
        let lock_timeout = std::time::Duration::from_millis(3000);
        match tokio::time::timeout(lock_timeout, as_ptr.lock()).await {
            Ok(state) => {
                // Get a snapshot of distros for conflict check (using async to avoid deadlock)
                let distros = state.wsl_dashboard.get_distros().await;
                (Arc::new(state.wsl_dashboard.clone()), state.wsl_dashboard.executor().clone(), state.config_manager.clone(), distros)
            },
            Err(_) => {
                error!("perform_install: Failed to acquire AppState lock within 3s");
                let ah_err = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_err.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_install_status(i18n::t("install.error").into());
                        app_typed.set_terminal_output("Error: System is busy (AppState lock timeout). Please try again.".into());
                    }
                });
                return;
            }
        }
    };
    
    dashboard.increment_manual_operation();
    let dashboard_cleanup = dashboard.clone();
    let _manual_op_guard = scopeguard::guard(dashboard_cleanup, |db| {
        db.decrement_manual_operation();
    });

    info!("perform_install: Initializing UI state...");
    let ah_init = ah.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = ah_init.upgrade() {
            let app_typed: AppWindow = app;
            app_typed.set_is_installing(true);
            app_typed.set_install_status(i18n::t("install.checking").into());
            app_typed.set_install_success(false);
            app_typed.set_terminal_output("".into());
            app_typed.set_name_error("".into());
        }
    });

    // 3. Name validation and conflict detection
    let mut final_name = name.clone();
    if final_name.is_empty() {
        if source_idx == 2 {
            final_name = friendly_name.clone();
        } else if !file_path.is_empty() {
            if let Some(stem) = std::path::Path::new(&file_path).file_stem() {
                final_name = stem.to_string_lossy().to_string();
            }
        }
    }

    if final_name.is_empty() {
        let ah_err = ah.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = ah_err.upgrade() {
                let app_typed: AppWindow = app;
                app_typed.set_name_error(i18n::t("dialog.name_required").into());
                app_typed.set_is_installing(false);
                app_typed.set_install_status(i18n::t("install.error").into());
            }
        });
        return;
    }

    let is_valid_chars = final_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !is_valid_chars || final_name.len() > 25 {
        let ah_err = ah.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = ah_err.upgrade() {
                let app_typed: AppWindow = app;
                app_typed.set_name_error(i18n::t("dialog.install_name_invalid").into());
                app_typed.set_is_installing(false);
                app_typed.set_install_status(i18n::t("install.error").into());
            }
        });
        return;
    }

    let name_exists = distro_snapshot.iter().any(|d| d.name == final_name);

    if name_exists {
        let new_suggested_name = sanitize_instance_name(&generate_random_suffix(&final_name));
        let ah_err = ah.clone();
        let mut distro_location = String::new();
        if let Some(app) = ah_err.upgrade() {
             let app_typed: AppWindow = app;
            distro_location = app_typed.get_distro_location().to_string();
        }
        
        let new_path = std::path::Path::new(&distro_location)
            .join(&new_suggested_name)
            .to_string_lossy()
            .to_string();

        let final_name_clone = final_name.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = ah_err.upgrade() {
                let app_typed: AppWindow = app;
                app_typed.set_new_instance_name(new_suggested_name.into());
                app_typed.set_new_instance_path(new_path.into());
                app_typed.set_name_error(i18n::tr("dialog.install_name_exists", &[final_name_clone]).into());
                app_typed.set_is_installing(false);
                app_typed.set_install_status(i18n::t("install.conflict_error").into());
            }
        });
        return;
    }

    let mut success = false;
    let mut error_msg = String::new();

    // 4. Source-specific installation logic
    match source_idx {
        2 => { // Store Source
            let real_id = if !internal_id.is_empty() {
                internal_id.clone()
            } else {
                friendly_name.clone()
            };

            if real_id.is_empty() {
                let ah_err = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_err.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_install_status(i18n::t("install.unknown_distro").into());
                        app_typed.set_is_installing(false);
                    }
                });
                return;
            }

            let default_distro_location = config_manager.get_settings().distro_location.clone();
            let final_target_path = if install_path.is_empty() {
                PathBuf::from(&default_distro_location)
                    .join(&final_name)
                    .to_string_lossy()
                    .to_string()
            } else {
                install_path.clone()
            };
            let seed_exists = distro_snapshot.iter().any(|d| d.name == real_id);
            let request = StoreCreateRequest::new(
                final_name.clone(),
                final_target_path.clone(),
                real_id.clone(),
            );
            let journal_root = PathBuf::from(config_manager.get_settings().temp_location.clone());

            let ah_status = ah.clone();
            let real_id_clone = real_id.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = ah_status.upgrade() {
                    let app_typed: AppWindow = app;
                    app_typed.set_install_status(i18n::t("install.installing").into());
                    app_typed.set_terminal_output(format!("{}\n", i18n::tr("install.step_1", &[real_id_clone])).into());
                }
            });
            let mut terminal_buffer = format!("{}\n", i18n::tr("install.step_1", &[real_id.clone()]));
            info!("Starting store installation for distribution ID: {}", real_id);

            let use_web_download = executor.detect_fastest_source().await;
            let source_text = if use_web_download { "GitHub" } else { "Microsoft" };

            let direct_plan = choose_strategy(CapabilityProbe::Supported, seed_exists, &real_id, &request);
            let direct_journal = StoreCreateJournal::new(
                uuid::Uuid::new_v4().to_string(),
                request.clone(),
                direct_plan.cleanup.clone(),
                direct_plan.seed_created_by_operation,
            );
            let direct_journal_path = match crate::store_create::save_journal(&journal_root, &direct_journal) {
                Ok(path) => path,
                Err(err) => {
                    let ah_err = ah.clone();
                    let err_text = format!("{}: {}", i18n::t("install.error"), err);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = ah_err.upgrade() {
                            let app_typed: AppWindow = app;
                            app_typed.set_install_status(err_text.into());
                            app_typed.set_is_installing(false);
                        }
                    });
                    return;
                }
            };

            if !final_target_path.is_empty() {
                let _ = std::fs::create_dir_all(&final_target_path);
                let _ = crate::store_create::create_ownership_marker(
                    &final_target_path,
                    &direct_journal.operation_id,
                );
            }

            let mut direct_args_owned = vec![
                "--install".to_string(),
                real_id.clone(),
                "--name".to_string(),
                final_name.clone(),
            ];
            if !final_target_path.is_empty() {
                direct_args_owned.push("--location".to_string());
                direct_args_owned.push(final_target_path.clone());
            }
            direct_args_owned.push("--no-launch".to_string());
            if use_web_download {
                direct_args_owned.push("--web-download".to_string());
            }
            let direct_cmd_str = format!("wsl {}", direct_args_owned.join(" "));
            terminal_buffer.push_str(&format!("Trying direct named install: {}\n", direct_cmd_str));

            let direct_args: Vec<&str> = direct_args_owned.iter().map(|arg| arg.as_str()).collect();
            let direct_result = executor.execute_command_streaming(&direct_args, |_| {}).await;

            if direct_result.success {
                dashboard.refresh_distros().await;
                let distros_final = dashboard.get_distros().await;
                if distros_final.iter().any(|d| d.name == final_name) {
                    success = true;
                    terminal_buffer.push_str(&format!(
                        "Direct Store install succeeded using {} as the download source.\n",
                        source_text
                    ));
                    terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_9")));
                    let _ = crate::store_create::update_journal_phase(
                        &direct_journal_path,
                        StoreCreatePhase::Completed,
                    );
                    let _ = crate::store_create::remove_ownership_marker(
                        &final_target_path,
                        &direct_journal.operation_id,
                    );
                    let _ = crate::store_create::remove_journal(&direct_journal_path);
                } else {
                    error_msg = i18n::tr("install.verify_failed", &[final_name.clone()]);
                    let _ = crate::store_create::update_journal_phase(
                        &direct_journal_path,
                        StoreCreatePhase::Recoverable,
                    );
                }
            } else if supports_named_install_error(
                &direct_result.output,
                direct_result.error.as_deref().unwrap_or(""),
            ) {
                terminal_buffer.push_str(
                    "Current WSL does not support '--name'; switching to compatibility mode.\n",
                );
                let _ = crate::store_create::remove_ownership_marker(
                    &final_target_path,
                    &direct_journal.operation_id,
                );
                let _ = crate::store_create::remove_journal(&direct_journal_path);

                if seed_exists {
                    error_msg = format!(
                        "Current WSL does not support '--name'. Safe automatic fallback is unavailable while '{}' already exists. Please upgrade WSL before creating another Store instance for this distro.",
                        real_id
                    );
                } else {
                    let fallback_plan = choose_strategy(
                        CapabilityProbe::Unsupported,
                        seed_exists,
                        &real_id,
                        &request,
                    );
                    let archive_path = fallback_plan.archive_path.clone().unwrap_or_default();
                    let fallback_journal = StoreCreateJournal::new(
                        uuid::Uuid::new_v4().to_string(),
                        request,
                        fallback_plan.cleanup.clone(),
                        fallback_plan.seed_created_by_operation,
                    );
                    let fallback_journal_path = match crate::store_create::save_journal(&journal_root, &fallback_journal) {
                        Ok(path) => path,
                        Err(err) => {
                            let ah_err = ah.clone();
                            let err_text = format!("{}: {}", i18n::t("install.error"), err);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = ah_err.upgrade() {
                                    let app_typed: AppWindow = app;
                                    app_typed.set_install_status(err_text.into());
                                    app_typed.set_is_installing(false);
                                }
                            });
                            return;
                        }
                    };

                    terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_2")));
                    let mut install_args = vec!["--install", "-d", &real_id, "--no-launch"];
                    if use_web_download {
                        install_args.push("--web-download");
                    }
                    let cmd_str = format!("wsl {}", install_args.join(" "));
                    terminal_buffer.push_str(&format!("{}\n", i18n::tr("install.step_3", &[cmd_str.clone()])));
                    terminal_buffer.push_str(&i18n::tr("install.step_4", &[source_text.to_string()]));

                    let fallback_result = executor.execute_command_streaming(&install_args, |_| {}).await;
                    if fallback_result.success {
                        dashboard.refresh_distros().await;
                        let distros_final = dashboard.get_distros().await;
                        if distros_final.iter().any(|d| d.name == real_id) {
                            let seed_install_path = executor
                                .get_distro_install_location(&real_id)
                                .await
                                .data;
                            if let Some(seed_install_path) = seed_install_path.as_ref() {
                                let _ = crate::store_create::register_owned_path(
                                    &fallback_journal_path,
                                    seed_install_path.clone(),
                                );
                                let _ = crate::store_create::create_ownership_marker(
                                    seed_install_path,
                                    &fallback_journal.operation_id,
                                );
                            }
                            let _ = crate::store_create::update_journal_phase(
                                &fallback_journal_path,
                                StoreCreatePhase::SeedReady,
                            );
                            terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_5")));
                            let _ = crate::store_create::update_journal_phase(
                                &fallback_journal_path,
                                StoreCreatePhase::PromotionPending,
                            );
                            terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_6")));

                            let archive_parent = PathBuf::from(config_manager.get_settings().temp_location.clone());
                            let _ = std::fs::create_dir_all(&archive_parent);
                            let export_res = executor.execute_command(&["--export", &real_id, &archive_path]).await;
                            if export_res.success {
                                terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_7")));
                                let unregister_res = executor.execute_command(&["--unregister", &real_id]).await;
                                if unregister_res.success {
                                    if let Some(seed_install_path) = seed_install_path.as_ref() {
                                        let _ = crate::store_create::remove_ownership_marker(
                                            seed_install_path,
                                            &fallback_journal.operation_id,
                                        );
                                    }
                                    if !final_target_path.is_empty() {
                                        let _ = std::fs::create_dir_all(&final_target_path);
                                        let _ = crate::store_create::create_ownership_marker(
                                            &final_target_path,
                                            &fallback_journal.operation_id,
                                        );
                                    }
                                    let import_res = executor.execute_command(&["--import", &final_name, &final_target_path, &archive_path]).await;
                                    if import_res.success {
                                        success = true;
                                        terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_8")));
                                        terminal_buffer.push_str(&format!("{}\n", i18n::t("install.step_9")));
                                        let _ = crate::store_create::update_journal_phase(
                                            &fallback_journal_path,
                                            StoreCreatePhase::Completed,
                                        );
                                        let _ = std::fs::remove_file(&archive_path);
                                        let _ = crate::store_create::remove_ownership_marker(
                                            &final_target_path,
                                            &fallback_journal.operation_id,
                                        );
                                        let _ = crate::store_create::remove_journal(&fallback_journal_path);
                                    } else {
                                        error_msg = import_res.error.unwrap_or_else(|| i18n::t("install.import_failed_custom"));
                                        let _ = crate::store_create::update_journal_phase(
                                            &fallback_journal_path,
                                            StoreCreatePhase::Recoverable,
                                        );
                                    }
                                } else {
                                    error_msg = unregister_res.error.unwrap_or_else(|| i18n::t("install.import_failed_custom"));
                                    let _ = crate::store_create::update_journal_phase(
                                        &fallback_journal_path,
                                        StoreCreatePhase::Recoverable,
                                    );
                                }
                            } else {
                                error_msg = export_res.error.unwrap_or_else(|| i18n::t("install.import_failed_custom"));
                                let _ = crate::store_create::update_journal_phase(
                                    &fallback_journal_path,
                                    StoreCreatePhase::Recoverable,
                                );
                            }
                        } else {
                            error_msg = i18n::tr("install.verify_failed", &[real_id.clone()]);
                            let _ = crate::store_create::update_journal_phase(
                                &fallback_journal_path,
                                StoreCreatePhase::Recoverable,
                            );
                        }
                    } else {
                        if !fallback_result.output.trim().is_empty() {
                            terminal_buffer.push_str(&format!("\n[WSL Output]\n{}\n", fallback_result.output));
                        }
                        error_msg = fallback_result.error.unwrap_or_else(|| i18n::t("install.install_failed"));
                        let _ = crate::store_create::update_journal_phase(
                            &fallback_journal_path,
                            StoreCreatePhase::Recoverable,
                        );
                    }
                }
            } else {
                if !direct_result.output.trim().is_empty() {
                    terminal_buffer.push_str(&format!("\n[WSL Output]\n{}\n", direct_result.output));
                }
                error_msg = direct_result.error.unwrap_or_else(|| i18n::t("install.install_failed"));
                let _ = crate::store_create::update_journal_phase(
                    &direct_journal_path,
                    StoreCreatePhase::Recoverable,
                );
            }

            let ah_cb = ah.clone();
            let tb_clone = terminal_buffer.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = ah_cb.upgrade() {
                    let app_typed: AppWindow = app;
                    app_typed.set_terminal_output(tb_clone.into());
                }
            });
        },
        0 | 1 => { // RootFS or VHDX Import
            if file_path.is_empty() {
                error_msg = i18n::t("install.select_file");
            } else {
                let mut terminal_buffer = format!("{}\n", i18n::t("install.step_1_3"));
                let ah_cb = ah.clone();
                let tb_clone = terminal_buffer.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_cb.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_terminal_output(tb_clone.into());
                    }
                });
                
                let mut target_path = install_path.clone();
                if target_path.is_empty() {
                    let distro_location = config_manager.get_settings().distro_location.clone();
                    let base = PathBuf::from(&distro_location);
                    target_path = base.join(&final_name).to_string_lossy().to_string();
                }
                
                let tp_clone = target_path.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&tp_clone)).await.unwrap() {
                    let err = format!("Failed to create directory: {}", e);
                    let ah_cb = ah.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = ah_cb.upgrade() {
                            let app_typed: AppWindow = app;
                            app_typed.set_install_success(false);
                            app_typed.set_install_status(format!("{}: {}", i18n::t("install.error"), err).into());
                            app_typed.set_is_installing(false);
                        }
                    });
                    return;
                }

                let ah_cb = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_cb.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_install_status(i18n::t("install.importing").into());
                    }
                });

                let mut import_args = vec!["--import", &final_name, &target_path, &file_path];
                if source_idx == 1 {
                    import_args.push("--vhd");
                }
                
                let cmd_str = format!("wsl {}", import_args.join(" "));
                terminal_buffer.push_str(&i18n::tr("install.step_2_3", &[cmd_str.clone()]));
                let ah_cb = ah.clone();
                let tb_clone = terminal_buffer.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_cb.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_terminal_output(tb_clone.into());
                    }
                });

                let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(100);
                let ah_ui = ah.clone();
                let initial_tb = terminal_buffer.clone();
                let ui_task = tokio::spawn(async move {
                    let mut buffer = initial_tb;
                    let mut dot_count = 0;
                    let mut interval = tokio::time::interval(std::time::Duration::from_millis(800));
                    
                    loop {
                        tokio::select! {
                            msg = rx.recv() => {
                                if msg.is_none() {
                                    break;
                                }
                                // Consume but don't display
                            }
                            _ = interval.tick() => {
                                if !buffer.ends_with('\n') {
                                     dot_count = (dot_count % 3) + 1;
                                     let mut dots = String::new();
                                     for _ in 0..dot_count { dots.push('.'); }
                                     let text_to_set = format!("{}{}", buffer, dots);
                                     
                                     let ah_cb = ah_ui.clone();
                                     let _ = slint::invoke_from_event_loop(move || {
                                         if let Some(app) = ah_cb.upgrade() {
                                             app.set_terminal_output(text_to_set.into());
                                         }
                                     });
                                }
                            }
                        }

                        if buffer.len() > 20_000 {
                            let to_drain = buffer.len() - 10_000;
                            if let Some(pos) = buffer[to_drain..].find('\n') {
                                buffer.drain(..to_drain + pos + 1);
                            } else {
                                buffer.drain(..to_drain);
                            }
                        }
                         // Throttled UI update removed to prevent overwriting dots animation
                    }
                    if !buffer.ends_with('\n') {
                        buffer.push('\n');
                    }
                    let ah_final = ah_ui.clone();
                    let text_to_set = buffer.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = ah_final.upgrade() {
                            let app_typed: AppWindow = app;
                            app_typed.set_terminal_output(text_to_set.into());
                        }
                    });
                    buffer
                });

                let tx_callback = tx.clone();
                let result = executor.execute_command_streaming(&import_args, move |text| {
                    let _ = tx_callback.try_send(text);
                }).await;

                drop(tx);
                terminal_buffer = ui_task.await.unwrap_or(terminal_buffer);

                success = result.success;
                if !success {
                     if !result.output.trim().is_empty() {
                         terminal_buffer.push_str(&format!("\n[WSL Output]\n{}\n", result.output));
                    }
                    error_msg = result.error.unwrap_or_else(|| i18n::t("install.import_failed"));
                } else {
                    terminal_buffer.push_str(&format!("{}\n", i18n::tr("install.step_3_3", &[final_name.clone()])));
                }
                
                let ah_cb = ah.clone();
                let tb_clone = terminal_buffer.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_cb.upgrade() {
                        let app_typed: AppWindow = app;
                        app_typed.set_terminal_output(tb_clone.into());
                    }
                });
            }
        },
        _ => {
            error_msg = i18n::t("install.unknown_source");
        }
    }

    let ah_final = ah.clone();
    let final_name_clone = final_name.clone();
    let error_msg_clone = error_msg.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = ah_final.upgrade() {
            let app_typed: AppWindow = app;
            if success {
                app_typed.set_install_success(true);
                app_typed.set_install_status(i18n::tr("install.created_success", &[final_name_clone]).into());
            } else {
                app_typed.set_install_success(false);
                app_typed.set_install_status(format!("{}: {}", i18n::t("install.error"), error_msg_clone).into());
            }
            app_typed.set_is_installing(false);
        }
    });
    
    if success {
        refresh_distros_ui(ah.clone(), as_ptr.clone()).await;
    }
}
