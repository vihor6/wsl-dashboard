pub mod plan;

use std::fs;
use std::path::{Path, PathBuf};

use plan::{ownership_marker_path, StoreCreateJournal, StoreCreatePhase};

fn persist_journal(path: &Path, journal: &StoreCreateJournal) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let payload = serde_json::to_string_pretty(journal).map_err(|e| e.to_string())?;
    fs::write(path, payload).map_err(|e| e.to_string())
}

pub fn save_journal(base_dir: &Path, journal: &StoreCreateJournal) -> Result<PathBuf, String> {
    let path = plan::journal_path(base_dir, &journal.operation_id);
    persist_journal(&path, journal)?;
    Ok(path)
}

pub fn load_journal(path: &Path) -> Result<StoreCreateJournal, String> {
    let payload = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&payload).map_err(|e| e.to_string())
}

pub fn update_journal_phase(
    path: &Path,
    phase: StoreCreatePhase,
) -> Result<StoreCreateJournal, String> {
    let mut journal = load_journal(path)?;
    journal.advance_to(phase);
    persist_journal(path, &journal)?;
    Ok(journal)
}

pub fn register_owned_path(path: &Path, owned_path: String) -> Result<StoreCreateJournal, String> {
    let mut journal = load_journal(path)?;
    journal.cleanup.register_owned_path(owned_path);
    persist_journal(path, &journal)?;
    Ok(journal)
}

pub fn remove_journal(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn list_journals(base_dir: &Path) -> Vec<PathBuf> {
    let journal_dir = base_dir.join(plan::JOURNAL_DIR_NAME);
    let Ok(entries) = fs::read_dir(journal_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect()
}

pub fn create_ownership_marker(install_path: &str, operation_id: &str) -> Result<PathBuf, String> {
    let marker_path = ownership_marker_path(install_path, operation_id);
    if let Some(parent) = marker_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&marker_path, operation_id).map_err(|e| e.to_string())?;
    Ok(marker_path)
}

pub fn remove_ownership_marker(install_path: &str, operation_id: &str) -> Result<(), String> {
    let marker_path = ownership_marker_path(install_path, operation_id);
    if marker_path.exists() {
        fs::remove_file(marker_path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn ownership_marker_exists(install_path: &str, operation_id: &str) -> bool {
    ownership_marker_path(install_path, operation_id).exists()
}
