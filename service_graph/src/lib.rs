use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceNode {
    pub service_name: String,
    pub repository: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceGraph {
    pub services: Vec<ServiceNode>,
}

pub struct ServiceGraphBuilder;

impl ServiceGraphBuilder {
    pub fn build(storage: &storage::Storage) -> Result<ServiceGraph> {
        let repos = storage.list_repositories()?;
        let module_edges = storage.list_named_module_dependencies()?;

        let mut services = Vec::new();
        for repo in repos {
            let service_name = if repo.name.contains("service") {
                repo.name.clone()
            } else {
                format!("{}_service", repo.name)
            };
            let mut deps = HashSet::new();
            for (from, to) in &module_edges {
                if from.contains("api") || from.contains("service") {
                    deps.insert(format!("{to}_service"));
                }
            }
            services.push(ServiceNode {
                service_name,
                repository: repo.name,
                dependencies: deps.into_iter().collect(),
            });
        }

        let mut uniq: HashMap<String, ServiceNode> = HashMap::new();
        for s in services {
            uniq.entry(s.service_name.clone())
                .and_modify(|e| {
                    let mut d: HashSet<String> = e.dependencies.iter().cloned().collect();
                    d.extend(s.dependencies.clone());
                    e.dependencies = d.into_iter().collect();
                })
                .or_insert(s);
        }
        let mut out: Vec<ServiceNode> = uniq.into_values().collect();
        out.sort_by(|a, b| a.service_name.cmp(&b.service_name));
        for s in &mut out {
            s.dependencies.sort();
            s.dependencies.dedup();
        }
        Ok(ServiceGraph { services: out })
    }
}
