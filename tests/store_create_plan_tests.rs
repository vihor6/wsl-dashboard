#[path = "../src/store_create/plan.rs"]
mod plan;

use std::path::Path;

use plan::{
    archive_path_for, choose_strategy, journal_path, staging_distro_name, staging_install_path,
    CapabilityProbe, RecoveryAction, StoreCreateJournal, StoreCreatePhase, StoreCreateRequest,
    StoreCreateStrategy, JOURNAL_DIR_NAME, STAGING_PREFIX,
};

fn sample_request() -> StoreCreateRequest {
    StoreCreateRequest::new(
        "Ubuntu-24.04-dev",
        r"D:\linux\Ubuntu-24.04-dev",
        "Ubuntu-24.04",
    )
}

#[test]
fn direct_install_only_applies_when_real_id_is_free_and_matches_target() {
    let request = StoreCreateRequest::new("Ubuntu-24.04", r"D:\linux\Ubuntu-24.04", "Ubuntu-24.04");

    let plan = choose_strategy(CapabilityProbe::Supported, false, "Ubuntu-24.04", &request);
    assert_eq!(plan.strategy, StoreCreateStrategy::DirectInstall);
    assert!(plan.cleanup.owns_distro("Ubuntu-24.04"));
    assert!(plan.cleanup.owns_path(r"D:\linux\Ubuntu-24.04"));

    let fallback = choose_strategy(CapabilityProbe::Unknown, true, "Ubuntu-24.04", &request);
    assert_eq!(fallback.strategy, StoreCreateStrategy::ExistingSeedPromote);
}

#[test]
fn existing_seed_promote_only_owns_new_instance_and_archive() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, true, "Ubuntu-24.04", &request);

    assert_eq!(plan.strategy, StoreCreateStrategy::ExistingSeedPromote);
    assert_eq!(plan.cleanup.owned_distros, vec![request.target_name.clone()]);
    assert_eq!(plan.cleanup.owned_paths, vec![request.target_path.clone()]);
    assert!(plan.cleanup.archive_path.is_some());
    assert!(!plan.cleanup.owns_distro("Ubuntu-24.04"));
    assert!(!plan.cleanup.owns_path(r"D:\linux\Ubuntu-24.04"));
}

#[test]
fn fresh_staged_promote_tracks_only_journal_owned_residue() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);

    assert_eq!(plan.strategy, StoreCreateStrategy::FreshStagedPromote);
    assert_eq!(plan.cleanup.owned_distros.len(), 2);
    assert!(plan.cleanup.owns_distro("Ubuntu-24.04-dev"));
    assert!(plan.cleanup.owned_distros.iter().any(|item| item.starts_with(STAGING_PREFIX)));
    assert!(plan.cleanup.owned_paths.iter().any(|item| item.contains(STAGING_PREFIX)));
    assert!(plan.cleanup.archive_path.is_some());
}

#[test]
fn journal_path_is_kept_out_of_instances_toml() {
    let path = journal_path(Path::new(r"C:\Users\alice\.wsldashboard\temp"), "op-1234");
    let rendered = path.to_string_lossy();

    assert!(rendered.contains(JOURNAL_DIR_NAME));
    assert!(rendered.ends_with("store-create-op1234.json"));
    assert!(!rendered.ends_with("instances.toml"));
}

#[test]
fn journal_recovery_actions_only_target_owned_residue() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let journal = StoreCreateJournal::new("recover-op-1234", request.clone(), plan.cleanup.clone());

    let actions = journal.recovery_actions();
    assert!(actions.iter().any(|action| matches!(
        action,
        RecoveryAction::RemoveManagedDistro { distro_name } if distro_name == &request.target_name
    )));
    assert!(actions.iter().any(|action| matches!(
        action,
        RecoveryAction::RemoveManagedPath { install_path } if install_path == &request.target_path
    )));
    assert!(actions.iter().any(|action| matches!(
        action,
        RecoveryAction::ReopenAddFlow { request: reopened } if reopened == &request
    )));
    assert!(!actions.iter().any(|action| matches!(
        action,
        RecoveryAction::RemoveManagedDistro { distro_name } if distro_name == "Ubuntu-20.04"
    )));
}

#[test]
fn journal_round_trips_phase_and_cleanup_data() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let mut journal = StoreCreateJournal::new("phase-op", request, plan.cleanup);
    journal.advance_to(StoreCreatePhase::Recoverable);

    let encoded = serde_json::to_string(&journal).expect("serialize");
    let decoded: StoreCreateJournal = serde_json::from_str(&encoded).expect("deserialize");

    assert_eq!(decoded.phase, StoreCreatePhase::Recoverable);
    assert_eq!(decoded.cleanup, journal.cleanup);
}

#[test]
fn generated_staging_metadata_is_opaque() {
    let op = "ABC123";
    let staging_name = staging_distro_name(op);
    let staging_path = staging_install_path(r"D:\linux\Ubuntu-24.04-dev", op);
    let archive_path = archive_path_for(r"D:\linux\Ubuntu-24.04-dev", op);

    assert!(staging_name.starts_with(STAGING_PREFIX));
    assert!(!staging_name.contains("Ubuntu"));
    assert!(staging_path.contains(STAGING_PREFIX));
    assert!(archive_path.ends_with(".tar"));
}
