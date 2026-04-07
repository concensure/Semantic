use anyhow::Result;
use org_graph::OrganizationGraph;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyInsight {
    pub cross_repo_dependencies: Vec<(String, String)>,
    pub shared_libraries: Vec<String>,
    pub sdk_usage: Vec<(String, String)>,
    pub version_mismatches: Vec<String>,
}

pub struct DependencyIntelligence;

impl DependencyIntelligence {
    pub fn analyze(
        storage: &storage::Storage,
        org_graph: &OrganizationGraph,
    ) -> Result<DependencyInsight> {
        let cross_repo_dependencies = org_graph
            .repositories
            .iter()
            .flat_map(|r| {
                r.dependencies
                    .iter()
                    .map(move |d| (r.name.clone(), d.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
        for (from, to) in &cross_repo_dependencies {
            dependents
                .entry(to.clone())
                .or_default()
                .insert(from.clone());
        }
        let mut shared_libraries = dependents
            .iter()
            .filter(|(_, users)| users.len() >= 2)
            .map(|(repo, _)| repo.clone())
            .collect::<Vec<_>>();
        shared_libraries.sort();

        let repos = storage.list_repositories()?;
        let sdk_usage = repos
            .iter()
            .flat_map(|r| {
                shared_libraries
                    .iter()
                    .filter(move |lib| r.name != **lib)
                    .map(move |lib| (r.name.clone(), lib.clone()))
            })
            .collect::<Vec<_>>();

        let version_mismatches = shared_libraries
            .iter()
            .map(|lib| format!("potential version mismatch for shared library '{}'", lib))
            .collect::<Vec<_>>();

        Ok(DependencyInsight {
            cross_repo_dependencies,
            shared_libraries,
            sdk_usage,
            version_mismatches,
        })
    }
}
