use anyhow::Result;
use engine::RepositoryRecord;

pub struct RepoRegistry {
    storage: storage::Storage,
}

impl RepoRegistry {
    pub fn new(storage: storage::Storage) -> Self {
        Self { storage }
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositoryRecord>> {
        self.storage.list_repositories()
    }
}
