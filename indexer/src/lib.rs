use anyhow::Result;
use parser::SupportedLanguage;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use walkdir::WalkDir;

pub struct Indexer {
    parser: parser::CodeParser,
    pub storage: storage::Storage,
    repo_id: i64,
}

impl Indexer {
    pub fn new(storage: storage::Storage) -> Self {
        Self {
            parser: parser::CodeParser::new(),
            storage,
            repo_id: 0,
        }
    }

    pub fn set_repo_id(&mut self, repo_id: i64) {
        self.repo_id = repo_id;
    }

    pub fn index_repo(&mut self, repo_path: &Path) -> Result<()> {
        let mut seen = HashSet::new();

        for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            let full_path = entry.path();
            let rel = match full_path.strip_prefix(repo_path) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };

            if SupportedLanguage::from_path(&rel).is_none() {
                continue;
            }

            seen.insert(rel.clone());
            self.index_file(repo_path, &rel)?;
        }

        for existing in self.storage.list_files()? {
            if !seen.contains(&existing) {
                self.storage.delete_file_records(&existing)?;
                self.storage.delete_file_metadata(&existing)?;
            }
        }

        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        Ok(())
    }

    pub fn index_file(&mut self, repo_path: &Path, relative_file: &str) -> Result<()> {
        let file_path = repo_path.join(relative_file);
        let content = fs::read_to_string(&file_path)?;
        let checksum = checksum(&content);

        if let Some(existing) = self.storage.get_file_checksum(relative_file)? {
            if existing == checksum {
                return Ok(());
            }
        }

        let parsed = self.parser.parse(relative_file, &content)?;
        self.storage.replace_file_index(
            self.repo_id,
            relative_file,
            &parsed.language,
            &checksum,
            &parsed.symbols,
            &parsed.dependencies,
            &parsed.logic_nodes,
        )?;
        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        Ok(())
    }

    pub fn delete_file(&mut self, relative_file: &str) -> Result<()> {
        self.storage.delete_file_records(relative_file)?;
        self.storage.delete_file_metadata(relative_file)?;
        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        Ok(())
    }

    fn rebuild_module_graph(&mut self) -> Result<()> {
        let mut files = self.storage.list_files()?;
        files.sort();

        self.storage.clear_module_graph()?;
        let mut modules: HashMap<String, i64> = HashMap::new();
        let mut file_to_module: HashMap<String, i64> = HashMap::new();

        for file in &files {
            let (module_name, module_path) = detect_module_from_file(file);
            let key = format!("{module_path}:{module_name}");
            let module_id = if let Some(id) = modules.get(&key) {
                *id
            } else {
                let id = self.storage.insert_module(&module_name, &module_path)?;
                modules.insert(key, id);
                id
            };
            self.storage.insert_module_file(module_id, file)?;
            file_to_module.insert(file.clone(), module_id);
        }

        let mut symbol_to_file: HashMap<String, String> = HashMap::new();
        let mut symbols = self.storage.list_symbols()?;
        symbols.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        for symbol in symbols {
            symbol_to_file
                .entry(symbol.name)
                .or_insert(symbol.file);
        }

        let deps = self.storage.list_all_dependencies()?;
        let mut module_edges = HashSet::new();
        for dep in deps {
            let caller_file = symbol_to_file
                .get(&dep.caller_symbol)
                .cloned()
                .unwrap_or(dep.file.clone());
            let Some(callee_file) = symbol_to_file.get(&dep.callee_symbol).cloned() else {
                continue;
            };

            let Some(caller_module) = file_to_module.get(&caller_file) else {
                continue;
            };
            let Some(callee_module) = file_to_module.get(&callee_file) else {
                continue;
            };

            if caller_module != callee_module {
                module_edges.insert((*caller_module, *callee_module));
            }
        }

        let mut edges: Vec<(i64, i64)> = module_edges.into_iter().collect();
        edges.sort_unstable();
        for (from_module, to_module) in edges {
            self.storage.insert_module_dependency(from_module, to_module)?;
        }

        Ok(())
    }
}

fn detect_module_from_file(file: &str) -> (String, String) {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() >= 3 && (parts[0] == "src" || parts[0] == "lib") {
        (parts[1].to_string(), format!("{}/{}", parts[0], parts[1]))
    } else if parts.len() >= 2 && (parts[0] == "src" || parts[0] == "lib") {
        (parts[0].to_string(), parts[0].to_string())
    } else if parts.len() >= 2 {
        (parts[0].to_string(), parts[0].to_string())
    } else {
        ("root".to_string(), "root".to_string())
    }
}

fn checksum(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::Indexer;
    use std::fs;
    use storage::Storage;

    #[test]
    fn indexes_simple_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("a.py"),
            "def retry_request():\n    return 1\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let symbols = indexer
            .storage
            .search_symbol_by_name("retry", 10)
            .expect("query symbols");
        assert_eq!(symbols.len(), 1);
    }

    #[test]
    fn builds_module_graph() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("api")).expect("mkdir api");
        fs::create_dir_all(repo.join("src").join("utils")).expect("mkdir utils");
        fs::write(
            repo.join("src").join("utils").join("retry.ts"),
            "export function retryRequest(){ return 1; }\n",
        )
        .expect("write retry");
        fs::write(
            repo.join("src").join("api").join("client.ts"),
            "import { retryRequest } from '../utils/retry';\nexport function fetchData(){ return retryRequest(); }\n",
        )
        .expect("write client");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let modules = indexer.storage.list_modules().expect("list modules");
        assert!(modules.iter().any(|m| m.name == "api"));
        assert!(modules.iter().any(|m| m.name == "utils"));

        let deps = indexer
            .storage
            .list_named_module_dependencies()
            .expect("module deps");
        assert!(deps.iter().any(|(from, to)| from == "api" && to == "utils"));
    }
}
