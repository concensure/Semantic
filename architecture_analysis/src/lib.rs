use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureIssue {
    pub module: String,
    pub issue_type: ArchitectureIssueType,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArchitectureIssueType {
    LayerViolation,
    TightCoupling,
    MissingInterface,
    CyclicModuleDependency,
    GodModule,
}

pub struct ArchitectureAnalyzer;

impl ArchitectureAnalyzer {
    pub fn analyze(storage: &storage::Storage) -> Result<Vec<ArchitectureIssue>> {
        let modules = storage.list_modules()?;
        let named_edges = storage.list_named_module_dependencies()?;
        let symbols = storage.list_symbols()?;

        let mut issues = Vec::new();
        let mut outgoing: HashMap<String, HashSet<String>> = HashMap::new();
        for (from, to) in &named_edges {
            outgoing.entry(from.clone()).or_default().insert(to.clone());
        }

        for (from, targets) in &outgoing {
            if targets.len() > 4 {
                issues.push(ArchitectureIssue {
                    module: from.clone(),
                    issue_type: ArchitectureIssueType::TightCoupling,
                    description: format!("module '{}' depends on {} modules", from, targets.len()),
                });
            }
            if from.contains("domain") && targets.iter().any(|t| t.contains("api")) {
                issues.push(ArchitectureIssue {
                    module: from.clone(),
                    issue_type: ArchitectureIssueType::LayerViolation,
                    description: "domain layer depends on api layer".to_string(),
                });
            }
        }

        for (from, to) in &named_edges {
            if named_edges.iter().any(|(a, b)| a == to && b == from) && from < to {
                issues.push(ArchitectureIssue {
                    module: from.clone(),
                    issue_type: ArchitectureIssueType::CyclicModuleDependency,
                    description: format!("cyclic dependency between '{}' and '{}'", from, to),
                });
            }
        }

        let mut module_symbol_counts: HashMap<String, usize> = HashMap::new();
        let mut module_public_fn_counts: HashMap<String, usize> = HashMap::new();
        for sym in &symbols {
            if let Some(m) = storage.get_module_by_file(&sym.file)? {
                *module_symbol_counts.entry(m.name.clone()).or_insert(0) += 1;
                if sym.name.starts_with("get") || sym.name.starts_with("set") || sym.name.starts_with("handle")
                {
                    *module_public_fn_counts.entry(m.name).or_insert(0) += 1;
                }
            }
        }

        for m in &modules {
            let count = module_symbol_counts.get(&m.name).copied().unwrap_or_default();
            if count > 100 {
                issues.push(ArchitectureIssue {
                    module: m.name.clone(),
                    issue_type: ArchitectureIssueType::GodModule,
                    description: format!("module '{}' contains {} symbols", m.name, count),
                });
            }
            let public_count = module_public_fn_counts
                .get(&m.name)
                .copied()
                .unwrap_or_default();
            if public_count > 30 {
                issues.push(ArchitectureIssue {
                    module: m.name.clone(),
                    issue_type: ArchitectureIssueType::MissingInterface,
                    description: format!(
                        "module '{}' exposes many entrypoints ({}) without clear interface boundary",
                        m.name, public_count
                    ),
                });
            }
        }

        issues.sort_by(|a, b| a.module.cmp(&b.module));
        Ok(issues)
    }
}
