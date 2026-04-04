use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const STAGING_PREFIX: &str = "wsl-dashboard-store-stage";
pub const JOURNAL_DIR_NAME: &str = "operations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityProbe {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreCreateStrategy {
    DirectInstall,
    FreshStagedPromote,
    ExistingSeedPromote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCreateRequest {
    pub target_name: String,
    pub target_path: String,
    pub store_id: String,
}

impl StoreCreateRequest {
    pub fn new(
        target_name: impl Into<String>,
        target_path: impl Into<String>,
        store_id: impl Into<String>,
    ) -> Self {
        Self {
            target_name: target_name.into(),
            target_path: target_path.into(),
            store_id: store_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreCreatePhase {
    JournalCreated,
    SeedReady,
    PromotionPending,
    Recoverable,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub owned_distros: Vec<String>,
    pub owned_paths: Vec<String>,
    pub archive_path: Option<String>,
}

impl CleanupPlan {
    pub fn owns_distro(&self, distro_name: &str) -> bool {
        self.owned_distros.iter().any(|owned| owned == distro_name)
    }

    pub fn owns_path(&self, path: &str) -> bool {
        self.owned_paths.iter().any(|owned| owned == path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    RemoveManagedDistro { distro_name: String },
    RemoveManagedPath { install_path: String },
    RemoveManagedArchive { archive_path: String },
    ReopenAddFlow { request: StoreCreateRequest },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCreateJournal {
    pub operation_id: String,
    pub request: StoreCreateRequest,
    pub phase: StoreCreatePhase,
    pub cleanup: CleanupPlan,
}

impl StoreCreateJournal {
    pub fn new(
        operation_id: impl Into<String>,
        request: StoreCreateRequest,
        cleanup: CleanupPlan,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            request,
            phase: StoreCreatePhase::JournalCreated,
            cleanup,
        }
    }

    pub fn advance_to(&mut self, phase: StoreCreatePhase) {
        self.phase = phase;
    }

    pub fn recovery_actions(&self) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        for distro_name in &self.cleanup.owned_distros {
            actions.push(RecoveryAction::RemoveManagedDistro {
                distro_name: distro_name.clone(),
            });
        }

        for install_path in &self.cleanup.owned_paths {
            actions.push(RecoveryAction::RemoveManagedPath {
                install_path: install_path.clone(),
            });
        }

        if let Some(archive_path) = &self.cleanup.archive_path {
            actions.push(RecoveryAction::RemoveManagedArchive {
                archive_path: archive_path.clone(),
            });
        }

        actions.push(RecoveryAction::ReopenAddFlow {
            request: self.request.clone(),
        });

        actions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCreatePlan {
    pub strategy: StoreCreateStrategy,
    pub final_path: String,
    pub staging_name: Option<String>,
    pub staging_path: Option<String>,
    pub archive_path: Option<String>,
    pub cleanup: CleanupPlan,
}

pub fn choose_strategy(
    probe: CapabilityProbe,
    seed_exists: bool,
    real_id: &str,
    request: &StoreCreateRequest,
) -> StoreCreatePlan {
    let operation_id = operation_fragment(&request.target_name);
    let final_path = request.target_path.clone();

    if !seed_exists && request.target_name == real_id {
        return StoreCreatePlan {
            strategy: match probe {
                CapabilityProbe::Supported => StoreCreateStrategy::DirectInstall,
                CapabilityProbe::Unsupported | CapabilityProbe::Unknown => {
                    StoreCreateStrategy::DirectInstall
                }
            },
            final_path: final_path.clone(),
            staging_name: None,
            staging_path: None,
            archive_path: None,
            cleanup: CleanupPlan {
                owned_distros: vec![request.target_name.clone()],
                owned_paths: vec![final_path],
                archive_path: None,
            },
        };
    }

    if seed_exists {
        let archive_path = archive_path_for(&request.target_path, &operation_id);
        return StoreCreatePlan {
            strategy: StoreCreateStrategy::ExistingSeedPromote,
            final_path: final_path.clone(),
            staging_name: None,
            staging_path: None,
            archive_path: Some(archive_path.clone()),
            cleanup: CleanupPlan {
                owned_distros: vec![request.target_name.clone()],
                owned_paths: vec![final_path],
                archive_path: Some(archive_path),
            },
        };
    }

    let staging_name = staging_distro_name(&operation_id);
    let staging_path = staging_install_path(&request.target_path, &operation_id);
    let archive_path = archive_path_for(&request.target_path, &operation_id);

    StoreCreatePlan {
        strategy: StoreCreateStrategy::FreshStagedPromote,
        final_path: final_path.clone(),
        staging_name: Some(staging_name.clone()),
        staging_path: Some(staging_path.clone()),
        archive_path: Some(archive_path.clone()),
        cleanup: CleanupPlan {
            owned_distros: vec![staging_name, request.target_name.clone()],
            owned_paths: vec![staging_path, final_path],
            archive_path: Some(archive_path),
        },
    }
}

pub fn journal_path(base_dir: &Path, operation_id: &str) -> PathBuf {
    base_dir
        .join(JOURNAL_DIR_NAME)
        .join(format!("store-create-{}.json", operation_fragment(operation_id)))
}

pub fn staging_distro_name(operation_id: &str) -> String {
    format!("{}-{}", STAGING_PREFIX, operation_fragment(operation_id))
}

pub fn staging_install_path(target_path: &str, operation_id: &str) -> String {
    parent_dir_for(target_path)
        .join(format!(".{}-{}", STAGING_PREFIX, operation_fragment(operation_id)))
        .to_string_lossy()
        .into_owned()
}

pub fn archive_path_for(target_path: &str, operation_id: &str) -> String {
    parent_dir_for(target_path)
        .join(format!("{}.tar", staging_distro_name(operation_id)))
        .to_string_lossy()
        .into_owned()
}

fn parent_dir_for(target_path: &str) -> PathBuf {
    if target_path.trim().is_empty() {
        return PathBuf::from(".");
    }

    let path = Path::new(target_path);
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from(target_path),
    }
}

fn operation_fragment(seed: &str) -> String {
    seed.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>()
        .to_lowercase()
}
