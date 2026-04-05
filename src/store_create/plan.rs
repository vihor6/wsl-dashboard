use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const JOURNAL_DIR_NAME: &str = "operations";
pub const OWNERSHIP_MARKER_PREFIX: &str = ".wsl-dashboard-store-create";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityProbe {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreCreateStrategy {
    DirectInstall,
    FreshSeedPromote,
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

    pub fn register_owned_path(&mut self, path: String) {
        if !self.owns_path(&path) {
            self.owned_paths.push(path);
        }
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
    pub created_at: String,
    pub request: StoreCreateRequest,
    pub phase: StoreCreatePhase,
    pub cleanup: CleanupPlan,
    pub seed_created_by_operation: bool,
}

impl StoreCreateJournal {
    pub fn new(
        operation_id: impl Into<String>,
        request: StoreCreateRequest,
        cleanup: CleanupPlan,
        seed_created_by_operation: bool,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            created_at: Utc::now().to_rfc3339(),
            request,
            phase: StoreCreatePhase::JournalCreated,
            cleanup,
            seed_created_by_operation,
        }
    }

    pub fn advance_to(&mut self, phase: StoreCreatePhase) {
        self.phase = phase;
    }

    pub fn can_cleanup_distro(&self, distro_name: &str, current_install_path: Option<&str>) -> bool {
        if !self.cleanup.owns_distro(distro_name) {
            return false;
        }

        if distro_name == self.request.target_name {
            return current_install_path == Some(self.request.target_path.as_str());
        }

        if distro_name == self.request.store_id {
            return self.seed_created_by_operation
                && current_install_path.is_some()
                && current_install_path.is_some_and(|path| self.cleanup.owns_path(path));
        }

        false
    }

    pub fn can_cleanup_path(&self, install_path: &str) -> bool {
        self.cleanup.owns_path(install_path)
            && install_path == self.request.target_path
    }

    pub fn can_cleanup_archive(&self, archive_path: &str) -> bool {
        self.cleanup.archive_path.as_deref() == Some(archive_path)
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
    pub archive_path: Option<String>,
    pub cleanup: CleanupPlan,
    pub seed_created_by_operation: bool,
}

pub fn choose_strategy(
    probe: CapabilityProbe,
    seed_exists: bool,
    real_id: &str,
    request: &StoreCreateRequest,
) -> StoreCreatePlan {
    let final_path = request.target_path.clone();
    let can_direct_install = matches!(probe, CapabilityProbe::Supported);

    if can_direct_install {
        return StoreCreatePlan {
            strategy: StoreCreateStrategy::DirectInstall,
            final_path: final_path.clone(),
            archive_path: None,
            cleanup: CleanupPlan {
                owned_distros: vec![request.target_name.clone()],
                owned_paths: if final_path.is_empty() {
                    Vec::new()
                } else {
                    vec![final_path]
                },
                archive_path: None,
            },
            seed_created_by_operation: true,
        };
    }

    let archive_path = archive_path_for(&request.target_path, &operation_fragment(&request.target_name));
    StoreCreatePlan {
        strategy: StoreCreateStrategy::FreshSeedPromote,
        final_path: final_path.clone(),
        archive_path: Some(archive_path.clone()),
        cleanup: CleanupPlan {
            owned_distros: vec![request.store_id.clone(), request.target_name.clone()],
            owned_paths: vec![final_path],
            archive_path: Some(archive_path),
        },
        seed_created_by_operation: true,
    }
}

pub fn journal_path(base_dir: &Path, operation_id: &str) -> PathBuf {
    base_dir
        .join(JOURNAL_DIR_NAME)
        .join(format!("store-create-{}.json", operation_fragment(operation_id)))
}

pub fn ownership_marker_path(install_path: &str, operation_id: &str) -> PathBuf {
    PathBuf::from(install_path).join(format!(
        "{}-{}.marker",
        OWNERSHIP_MARKER_PREFIX,
        operation_fragment(operation_id)
    ))
}

pub fn archive_path_for(target_path: &str, operation_id: &str) -> String {
    parent_dir_for(target_path)
        .join(format!("wsl_store_create_{}.tar", operation_fragment(operation_id)))
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
