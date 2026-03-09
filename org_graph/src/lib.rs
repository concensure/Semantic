use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrganizationGraph {
    pub repositories: Vec<RepositoryNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryNode {
    pub name: String,
    pub language: String,
    pub dependencies: Vec<String>,
}

pub struct OrganizationGraphBuilder;

impl OrganizationGraphBuilder {
    pub fn build(storage: &storage::Storage) -> Result<OrganizationGraph> {
        let repos = storage.list_repositories()?;
        let repo_deps = storage.list_repo_dependencies()?;
        let symbols = storage.list_symbols()?;

        let mut id_to_name = HashMap::new();
        for r in &repos {
            id_to_name.insert(r.id.unwrap_or_default(), r.name.clone());
        }

        let mut by_repo_lang: HashMap<i64, HashMap<String, usize>> = HashMap::new();
        for s in symbols {
            let langs = by_repo_lang.entry(s.repo_id).or_default();
            *langs.entry(s.language).or_insert(0) += 1;
        }

        let mut out = Vec::new();
        for repo in repos {
            let repo_id = repo.id.unwrap_or_default();
            let dependencies = repo_deps
                .iter()
                .filter(|d| d.from_repo == repo_id)
                .filter_map(|d| id_to_name.get(&d.to_repo).cloned())
                .collect::<Vec<_>>();
            let language = by_repo_lang
                .get(&repo_id)
                .and_then(|m| m.iter().max_by_key(|(_, c)| *c).map(|(k, _)| k.clone()))
                .unwrap_or_else(|| "unknown".to_string());

            out.push(RepositoryNode {
                name: repo.name,
                language,
                dependencies,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(OrganizationGraph { repositories: out })
    }
}
