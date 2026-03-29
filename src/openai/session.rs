use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedSessionState {
    doc_path: String,
    patch_previous_response_id: Option<String>,
    analysis_previous_response_id: Option<String>,
}

pub struct DocumentSessionStore {
    path: PathBuf,
    state: PersistedSessionState,
}

impl DocumentSessionStore {
    pub fn for_doc(doc_path: &Path) -> Result<Self> {
        let base_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".aichitect");
        Self::from_base_dir(&base_dir, doc_path)
    }

    fn from_base_dir(base_dir: &Path, doc_path: &Path) -> Result<Self> {
        let dir = base_dir.join("sessions");
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create session dir {}", dir.display()))?;

        let path = dir.join(Self::session_filename(doc_path));
        let doc_path_string = doc_path.to_string_lossy().to_string();

        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read session state {}", path.display()))?;
            let mut state: PersistedSessionState = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse session state {}", path.display()))?;
            if state.doc_path.is_empty() {
                state.doc_path = doc_path_string;
            }
            Ok(Self { path, state })
        } else {
            Ok(Self {
                path,
                state: PersistedSessionState {
                    doc_path: doc_path_string,
                    ..PersistedSessionState::default()
                },
            })
        }
    }

    pub fn patch_previous_response_id(&self) -> Option<String> {
        self.state.patch_previous_response_id.clone()
    }

    pub fn analysis_previous_response_id(&self) -> Option<String> {
        self.state.analysis_previous_response_id.clone()
    }

    pub fn set_patch_previous_response_id(&mut self, response_id: Option<String>) -> Result<()> {
        self.state.patch_previous_response_id = response_id;
        self.persist()
    }

    pub fn set_analysis_previous_response_id(&mut self, response_id: Option<String>) -> Result<()> {
        self.state.analysis_previous_response_id = response_id;
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.state)
            .context("Failed to serialize session state")?;
        fs::write(&self.path, content)
            .with_context(|| format!("Failed to write session state {}", self.path.display()))?;
        Ok(())
    }

    fn session_filename(doc_path: &Path) -> String {
        let stem = doc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document");
        let mut hasher = DefaultHasher::new();
        doc_path.to_string_lossy().hash(&mut hasher);
        format!("{}-{:016x}.json", sanitize_filename(stem), hasher.finish())
    }
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }

    let sanitized = out
        .trim_matches('-')
        .to_string()
        .chars()
        .take(40)
        .collect::<String>();
    if sanitized.is_empty() {
        "document".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persists_response_ids_per_document() {
        let tmp = tempdir().unwrap();
        let doc_path = tmp.path().join("docs").join("plan.md");

        let mut store = DocumentSessionStore::from_base_dir(tmp.path(), &doc_path).unwrap();
        assert_eq!(store.patch_previous_response_id(), None);
        assert_eq!(store.analysis_previous_response_id(), None);

        store
            .set_patch_previous_response_id(Some("resp_patch".to_string()))
            .unwrap();
        store
            .set_analysis_previous_response_id(Some("resp_analysis".to_string()))
            .unwrap();

        let reloaded = DocumentSessionStore::from_base_dir(tmp.path(), &doc_path).unwrap();
        assert_eq!(
            reloaded.patch_previous_response_id(),
            Some("resp_patch".to_string())
        );
        assert_eq!(
            reloaded.analysis_previous_response_id(),
            Some("resp_analysis".to_string())
        );
    }

    #[test]
    fn different_documents_get_different_session_files() {
        let tmp = tempdir().unwrap();
        let doc_a = tmp.path().join("a").join("spec.md");
        let doc_b = tmp.path().join("b").join("spec.md");

        let store_a = DocumentSessionStore::from_base_dir(tmp.path(), &doc_a).unwrap();
        let store_b = DocumentSessionStore::from_base_dir(tmp.path(), &doc_b).unwrap();

        assert_ne!(store_a.path, store_b.path);
    }
}
