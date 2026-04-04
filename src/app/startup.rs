use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::Mutex;
use tracing::{info, warn};
use slint::ComponentHandle;
use crate::{AppWindow, AppInfo, AppState};

// Automatically check for updates and expiration at startup
pub fn spawn_check_task(app_handle: slint::Weak<AppWindow>, app_state: Arc<Mutex<AppState>>) {
    let ah = app_handle.clone();
    let app_state_check = app_state.clone();
    
    tokio::spawn(async move {
        let current_v = env!("CARGO_PKG_VERSION");
        // Wait a moment before checking to avoid affecting startup speed
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Read last popup time and check update interval
        let (last_check_time, check_update_days, timezone, is_silent_mode) = {
            let state = app_state_check.lock().await;
            let settings = state.config_manager.get_settings();
            let timezone = state.config_manager.get_config().system.timezone.clone();
            (
                settings.check_time.parse::<i64>().unwrap_or(0),
                settings.check_update as i64,
                timezone,
                state.is_silent_mode
            )
        };
        
        if is_silent_mode {
            info!("Skipping startup checks (silent mode)");
            return;
        }
        
        let now_ms = chrono::Utc::now().timestamp_millis();
        let interval_ms: i64 = check_update_days * 24 * 60 * 60 * 1000;
        let should_check_update = (now_ms - last_check_time) >= interval_ms;

        info!("Check-update: last={}, now={}, interval={}, should_check_update={}", 
               last_check_time, now_ms, interval_ms, should_check_update);
        
        // If not time to check, skip both expiration and update checks
        if !should_check_update {
            info!("Skipping startup checks (interval not reached)");
            return;
        }

        // Check expiration first
        let expire_time_str = env!("APP_EXPIRE_TIME");
        let expire_time: i64 = expire_time_str.parse().unwrap_or(0);
        
        if expire_time > 0 {
            info!("Checking expiration first. App expire time: {}", expire_time);
            let timezone_inner = timezone.clone();
            let now = tokio::task::spawn_blocking(move || crate::utils::time::standard_time(&timezone_inner))
                .await
                .unwrap_or(0);
                
            info!("Current standard time: {}", now);
            
            if now > expire_time {
                let ah_c = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_c.upgrade() {
                        app.set_show_expire_dialog(true);
                    }
                });
                // Update check-update timestamp
                let mut state = app_state_check.lock().await;
                let _ = state.config_manager.update_check_time();
                info!("App expired! Skipping update check.");
                return;
            }
        }

        // If not expired, then check for updates
        let ah_c = ah.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = ah_c.upgrade() {
                app.global::<AppInfo>().set_checking_update(true);
            }
        });

        match crate::app::updater::check_update(current_v, &timezone).await {
            Ok(result) => {
                // Update status in AppInfo regardless of popup (used by About page)
                let has_update = result.has_update;
                let latest_version = result.latest_version.clone();
                
                let ah_c = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_c.upgrade() {
                        app.global::<AppInfo>().set_checking_update(false);
                        if has_update {
                            app.global::<AppInfo>().set_has_update(true);
                            app.global::<AppInfo>().set_latest_version(latest_version.into());
                        }
                    }
                });

                // Only show popup if there's an update (should_check_update is already true here)
                if has_update {
                    let ah_c = ah.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = ah_c.upgrade() {
                            app.set_show_update_dialog(true);
                        }
                    });
                    // Update check-update timestamp
                    let mut state = app_state_check.lock().await;
                    let _ = state.config_manager.update_check_time();
                }
            }
            Err(e) => {
                warn!("Auto check update failed: {}", e);
                let ah_c = ah.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = ah_c.upgrade() {
                        app.global::<AppInfo>().set_checking_update(false);
                    }
                });
            }
        }
    });
}

pub fn spawn_store_create_recovery_check(
    app_handle: slint::Weak<AppWindow>,
    app_state: Arc<Mutex<AppState>>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let (temp_location, is_silent_mode) = {
            let state = app_state.lock().await;
            (
                state.config_manager.get_settings().temp_location.clone(),
                state.is_silent_mode,
            )
        };

        if is_silent_mode {
            return;
        }

        let base_dir = PathBuf::from(temp_location);
        let journal_paths = crate::store_create::list_journals(&base_dir);
        let Some(journal_path) = journal_paths.into_iter().next() else {
            return;
        };

        let Ok(journal) = crate::store_create::load_journal(&journal_path) else {
            warn!(
                "Store-create recovery check found an unreadable journal at {}",
                journal_path.display()
            );
            return;
        };

        let payload = format!("store-create-recovery:{}", journal_path.display());
        let message = format!(
            "Detected an interrupted Store instance creation for '{}'. Existing instances were preserved. You can clean up the unfinished residue and reopen the add flow.",
            journal.request.target_name
        );

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = app_handle.upgrade() {
                app.set_current_message(message.into());
                app.set_current_message_action("Clean up and retry".into());
                app.set_copy_script_content(payload.into());
                app.set_show_message_dialog(true);
            }
        });
    });
}
