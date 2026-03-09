use parking_lot::RwLock;
use std::collections::HashMap;
use tree_sitter::Tree;

#[derive(Default)]
pub struct AstCache {
    inner: RwLock<HashMap<String, Tree>>,
}

impl AstCache {
    pub fn get(&self, key: &str) -> Option<Tree> {
        self.inner.read().get(key).cloned()
    }

    pub fn set(&self, key: String, tree: Tree) {
        self.inner.write().insert(key, tree);
    }
}
