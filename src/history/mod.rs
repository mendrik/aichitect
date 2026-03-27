use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::fs;
use std::path::{Path, PathBuf};

/// One entry in the history list.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub timestamp: DateTime<Local>,
    pub label: String,
}

/// Manages on-disk snapshots for a single document.
///
/// Snapshots live at:  `~/.aichitect/history/<stem>/<YYYY-MM-DDTHH-MM-SS>.md`
pub struct HistoryStore {
    dir: PathBuf,
}

impl HistoryStore {
    /// Create (or reuse) the history directory for `doc_path`.
    pub fn for_doc(doc_path: &Path) -> Result<Self> {
        let stem = doc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document");

        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".aichitect")
            .join("history")
            .join(stem);

        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Write `content` as a new snapshot.  Returns the path written.
    pub fn save_snapshot(&self, content: &str) -> Result<PathBuf> {
        let ts = Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let filename = format!("{}.md", ts);
        let path = self.dir.join(&filename);
        fs::write(&path, content)?;
        Ok(path)
    }

    /// List all snapshots newest-first.
    pub fn list(&self) -> Vec<HistoryEntry> {
        let Ok(rd) = fs::read_dir(&self.dir) else { return vec![] };
        let mut entries: Vec<HistoryEntry> = rd
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .filter_map(|e| {
                let path = e.path();
                let stem = path.file_stem()?.to_str()?.to_string();
                // Parse "YYYY-MM-DDTHH-MM-SS"
                let normalized = stem.replace('T', " ").replacen('-', "/", 2).replace('-', ":");
                // normalised: "YYYY/MM/DD HH:MM:SS"
                let dt = NaiveDateTime::parse_from_str(&normalized, "%Y/%m/%d %H:%M:%S").ok()?;
                let timestamp = Local.from_local_datetime(&dt).single()?;
                let label = timestamp.format("%Y-%m-%d  %H:%M:%S").to_string();
                Some(HistoryEntry { path, timestamp, label })
            })
            .collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries
    }

    /// Load snapshot content from a path.
    pub fn load(path: &Path) -> Result<String> {
        Ok(fs::read_to_string(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_save_and_list() {
        let tmp = tempdir().unwrap();
        let doc_path = tmp.path().join("plan.md");

        // Redirect history dir to temp for testing.
        let store = HistoryStore { dir: tmp.path().join("history") };
        fs::create_dir_all(&store.dir).unwrap();

        store.save_snapshot("# Version 1\nHello").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.save_snapshot("# Version 2\nWorld").unwrap();

        let entries = store.list();
        assert_eq!(entries.len(), 2);
        // Newest first.
        assert!(entries[0].timestamp > entries[1].timestamp);

        let content = HistoryStore::load(&entries[1].path).unwrap();
        assert!(content.contains("Version 1"));
        drop(doc_path);
    }
}
