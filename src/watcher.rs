use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

use crate::tui::app::AppEvent;

/// Spawns a background thread that watches `path` for external modifications.
/// Sends `AppEvent::ExternalFileChanged` over `tx` whenever the file changes on disk.
/// Returns a handle that keeps the watcher alive; drop it to stop watching.
pub fn spawn_file_watcher(
    path: PathBuf,
    tx: tokio_mpsc::Sender<AppEvent>,
) -> Result<WatcherHandle> {
    let (notify_tx, notify_rx) = mpsc::channel();

    let mut debouncer = new_debouncer(Duration::from_millis(500), notify_tx)?;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot watch root path"))?;

    debouncer
        .watcher()
        .watch(parent, notify::RecursiveMode::NonRecursive)?;

    let watched_path = path.clone();
    std::thread::spawn(move || {
        for result in notify_rx {
            match result {
                Ok(events) => {
                    let dominated = events
                        .iter()
                        .any(|ev| ev.kind == DebouncedEventKind::Any && ev.path == watched_path);
                    if dominated {
                        if let Ok(content) = std::fs::read_to_string(&watched_path) {
                            let _ = tx.blocking_send(AppEvent::ExternalFileChanged(content));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(AppEvent::StatusMessage(format!(
                        "File watcher error: {:?}",
                        e
                    )));
                }
            }
        }
    });

    Ok(WatcherHandle {
        _debouncer: debouncer,
    })
}

/// Keeps the file watcher alive. Drop to stop watching.
pub struct WatcherHandle {
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}
