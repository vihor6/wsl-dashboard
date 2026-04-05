#[path = "../src/store_create/plan.rs"]
mod plan;

use std::path::Path;

use plan::{
    archive_path_for, choose_strategy, journal_path, ownership_marker_path, CapabilityProbe,
    CleanupDecision, RecoveryAction, StoreCreateJournal, StoreCreatePhase, StoreCreateRequest,
    StoreCreateStrategy, JOURNAL_DIR_NAME, OWNERSHIP_MARKER_PREFIX,
};

fn sample_request() -> StoreCreateRequest {
    StoreCreateRequest::new(
        "Ubuntu-24.04-dev",
        r"D:\linux\Ubuntu-24.04-dev",
        "Ubuntu-24.04",
    )
}

#[test]
fn direct_install_requires_supported_probe() {
    let request = StoreCreateRequest::new("Ubuntu-24.04", "", "Ubuntu-24.04");

    let plan = choose_strategy(CapabilityProbe::Supported, false, "Ubuntu-24.04", &request);
    assert_eq!(plan.strategy, StoreCreateStrategy::DirectInstall);
    assert!(plan.seed_created_by_operation);

    let fallback = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    assert_eq!(fallback.strategy, StoreCreateStrategy::FreshSeedPromote);
}

#[test]
fn direct_install_stays_preferred_even_if_same_store_distro_already_exists() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Supported, true, "Ubuntu-24.04", &request);

    assert_eq!(plan.strategy, StoreCreateStrategy::DirectInstall);
    assert_eq!(plan.cleanup.owned_distros, vec![request.target_name.clone()]);
    assert_eq!(plan.cleanup.owned_paths, vec![request.target_path.clone()]);
}

#[test]
fn fresh_seed_promote_tracks_created_seed_and_target() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, true, "Ubuntu-24.04", &request);

    assert_eq!(plan.strategy, StoreCreateStrategy::FreshSeedPromote);
    assert!(plan.seed_created_by_operation);
    assert_eq!(
        plan.cleanup.owned_distros,
        vec!["Ubuntu-24.04".to_string(), request.target_name.clone()]
    );
    assert_eq!(plan.cleanup.owned_paths, vec![request.target_path.clone()]);
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
fn ownership_marker_path_is_scoped_to_operation_and_install_path() {
    let marker = ownership_marker_path(r"D:\linux\Ubuntu-24.04-dev", "ABC123");
    let rendered = marker.to_string_lossy();

    assert!(rendered.contains(OWNERSHIP_MARKER_PREFIX));
    assert!(rendered.ends_with(".marker"));
    assert!(rendered.contains("abc123"));
}

#[test]
fn cleanup_validation_rejects_unowned_distros_and_paths() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let mut journal = StoreCreateJournal::new("recover-op-1234", request.clone(), plan.cleanup, true);
    journal.cleanup.register_owned_path(r"D:\seed-cache\Ubuntu-24.04".to_string());

    assert!(journal.can_cleanup_distro("Ubuntu-24.04", Some(r"D:\seed-cache\Ubuntu-24.04")));
    assert!(journal.can_cleanup_distro("Ubuntu-24.04-dev", Some(request.target_path.as_str())));
    assert!(!journal.can_cleanup_distro("Ubuntu-20.04", Some(r"D:\linux\Ubuntu-20.04")));
    assert!(!journal.can_cleanup_distro("Ubuntu-24.04", Some(r"D:\linux\Ubuntu-20.04")));
    assert!(journal.can_cleanup_path(request.target_path.as_str()));
    assert!(journal.can_cleanup_path(r"D:\seed-cache\Ubuntu-24.04"));
    assert!(!journal.can_cleanup_path(r"D:\windows\system32"));
    assert!(journal.can_cleanup_archive(journal.cleanup.archive_path.as_deref().unwrap_or_default()));
    assert!(!journal.can_cleanup_archive(r"D:\other\archive.tar"));
}

#[test]
fn cleanup_decisions_treat_missing_resources_as_already_absent() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let journal = StoreCreateJournal::new("recover-op-1234", request.clone(), plan.cleanup, true);

    assert_eq!(
        journal.cleanup_distro_decision(&request.target_name, None, false),
        CleanupDecision::AlreadyAbsent
    );
    assert_eq!(
        journal.cleanup_path_decision(request.target_path.as_str(), false, false),
        CleanupDecision::AlreadyAbsent
    );
    assert_eq!(
        journal.cleanup_archive_decision(
            journal.cleanup.archive_path.as_deref().unwrap_or_default(),
            false,
        ),
        CleanupDecision::AlreadyAbsent
    );
}

#[test]
fn cleanup_decisions_require_owned_marked_resources_before_removal() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let mut journal = StoreCreateJournal::new("recover-op-1234", request.clone(), plan.cleanup, true);
    journal.cleanup.register_owned_path(r"D:\seed-cache\Ubuntu-24.04".to_string());

    assert_eq!(
        journal.cleanup_distro_decision(
            "Ubuntu-24.04",
            Some(r"D:\seed-cache\Ubuntu-24.04"),
            true,
        ),
        CleanupDecision::RemoveNow
    );
    assert_eq!(
        journal.cleanup_path_decision(request.target_path.as_str(), true, true),
        CleanupDecision::RemoveNow
    );
    assert_eq!(
        journal.cleanup_path_decision(r"D:\seed-cache\Ubuntu-24.04", true, true),
        CleanupDecision::RemoveNow
    );
    assert_eq!(
        journal.cleanup_distro_decision("Ubuntu-24.04", Some(r"D:\linux\Ubuntu-20.04"), true),
        CleanupDecision::Unsafe
    );
    assert_eq!(
        journal.cleanup_path_decision(request.target_path.as_str(), true, false),
        CleanupDecision::Unsafe
    );
}

#[test]
fn journal_recovery_actions_cover_only_managed_cleanup_and_reopen() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, true, "Ubuntu-24.04", &request);
    let journal = StoreCreateJournal::new("recover-op-1234", request.clone(), plan.cleanup, true);

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
}

#[test]
fn journal_round_trips_phase_cleanup_and_seed_origin() {
    let request = sample_request();
    let plan = choose_strategy(CapabilityProbe::Unknown, false, "Ubuntu-24.04", &request);
    let mut journal = StoreCreateJournal::new("phase-op", request, plan.cleanup, true);
    journal.advance_to(StoreCreatePhase::Recoverable);

    let encoded = serde_json::to_string(&journal).expect("serialize");
    let decoded: StoreCreateJournal = serde_json::from_str(&encoded).expect("deserialize");

    assert_eq!(decoded.phase, StoreCreatePhase::Recoverable);
    assert!(decoded.seed_created_by_operation);
    assert_eq!(decoded.cleanup, journal.cleanup);
    assert_eq!(decoded.created_at, journal.created_at);
}
