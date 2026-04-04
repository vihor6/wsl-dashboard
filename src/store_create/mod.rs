pub mod plan;

use std::fs;
use std::path::{Path, PathBuf};

use plan::{StoreCreateJournal, StoreCreatePhase};

pub fn save_journal(base_dir: &Path, journal: &StoreCreateJournal) -> Result<PathBuf, String> {
    let path = plan::journal_path(base_dir, &journal.operation_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let payload = serde_json::to_string_pretty(journal).map_err(|e| e.to_string())?;
    fs::write(&path, payload).map_err(|e| e.to_string())?;
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
    let payload = serde_json::to_string_pretty(&journal).map_err(|e| e.to_string())?;
    fs::write(path, payload).map_err(|e| e.to_string())?;
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
