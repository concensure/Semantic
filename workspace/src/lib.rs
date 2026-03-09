use anyhow::Result;
use engine::RepositoryRecord;
use std::path::Path;

pub struct WorkspaceRegistry {
    storage: storage::Storage,
}

impl WorkspaceRegistry {
    pub fn new(storage: storage::Storage) -> Self {
        Self { storage }
    }

    pub fn register_repository(&self, path: &Path) -> Result<RepositoryRecord> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repository");
        self.storage
            .register_repository(name, &path.to_string_lossy().replace('\\', "/"))
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositoryRecord>> {
        self.storage.list_repositories()
    }

    pub fn get_repository(&self, id: i64) -> Result<Option<RepositoryRecord>> {
        self.storage.get_repository(id)
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceRegistry;

    #[test]
    fn registers_repository() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("db.sqlite");
        let idx = tmp.path().join("idx");
        let storage = storage::Storage::open(&db, &idx).expect("storage");
        let registry = WorkspaceRegistry::new(storage);

        let repo = registry
            .register_repository(tmp.path())
            .expect("register repo");
        assert!(repo.id.is_some());
    }
}
