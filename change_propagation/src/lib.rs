use dependency_intelligence::DependencyInsight;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePropagation {
    pub origin_repo: String,
    pub impacted_repositories: Vec<String>,
}

pub struct ChangePropagationEngine;

impl ChangePropagationEngine {
    pub fn predict(origin_repo: &str, insight: &DependencyInsight) -> ChangePropagation {
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();
        for (from, to) in &insight.cross_repo_dependencies {
            graph.entry(from.clone()).or_default().push(to.clone());
        }

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(origin_repo.to_string());
        visited.insert(origin_repo.to_string());

        while let Some(repo) = queue.pop_front() {
            if let Some(neighbors) = graph.get(&repo) {
                for next in neighbors {
                    if visited.insert(next.clone()) {
                        queue.push_back(next.clone());
                    }
                }
            }
        }

        let mut impacted_repositories = visited.into_iter().collect::<Vec<_>>();
        impacted_repositories.sort();
        impacted_repositories.retain(|r| r != origin_repo);

        ChangePropagation {
            origin_repo: origin_repo.to_string(),
            impacted_repositories,
        }
    }
}
