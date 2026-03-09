use anyhow::Result;
use indexer::Indexer;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct RepoWatcher {
    _watcher: RecommendedWatcher,
}

impl RepoWatcher {
    pub fn start(repo_root: PathBuf, indexer: Arc<Mutex<Indexer>>) -> Result<Self> {
        let root_for_cb = repo_root.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let event = match res {
                    Ok(ev) => ev,
                    Err(_) => return,
                };

                for path in &event.paths {
                    if let Some(relative) = to_relative(&root_for_cb, path) {
                        let mut idx = indexer.lock();
                        match event.kind {
                            EventKind::Remove(_) => {
                                let _ = idx.delete_file(&relative);
                            }
                            EventKind::Modify(_) | EventKind::Create(_) => {
                                let _ = idx.index_file(&root_for_cb, &relative);
                            }
                            _ => {}
                        }
                    }
                }
            },
            Config::default(),
        )?;

        watcher.watch(&repo_root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

fn to_relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let normalized = relative.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}
