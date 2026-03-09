use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct APIContract {
    pub service: String,
    pub version: String,
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct APIContractGraph {
    pub contracts: Vec<APIContract>,
}

pub struct APIContractGraphBuilder;

impl APIContractGraphBuilder {
    pub fn scan(repo_root: &Path, storage: &storage::Storage) -> Result<APIContractGraph> {
        let files = storage.list_files()?;
        let mut contracts = Vec::new();

        for file in files {
            let lower = file.to_lowercase();
            if !(lower.ends_with(".yaml")
                || lower.ends_with(".yml")
                || lower.ends_with(".graphql")
                || lower.ends_with(".proto")
                || lower.contains("openapi"))
            {
                continue;
            }

            let abs = repo_root.join(&file);
            let content = fs::read_to_string(&abs).unwrap_or_default();
            let service = infer_service_name(&file);
            let version = infer_version(&content);
            let endpoints = infer_endpoints(&content, &lower);
            contracts.push(APIContract {
                service,
                version,
                endpoints,
            });
        }

        contracts.sort_by(|a, b| a.service.cmp(&b.service));
        Ok(APIContractGraph { contracts })
    }
}

fn infer_service_name(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() > 1 {
        parts[0].to_string()
    } else {
        "unknown_service".to_string()
    }
}

fn infer_version(content: &str) -> String {
    for line in content.lines() {
        let lower = line.to_lowercase();
        if lower.contains("version") {
            let v = line.split(':').nth(1).unwrap_or("v1").trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "v1".to_string()
}

fn infer_endpoints(content: &str, file_lower: &str) -> Vec<Endpoint> {
    let mut endpoints = Vec::new();
    if file_lower.ends_with(".proto") {
        for line in content.lines().filter(|l| l.trim_start().starts_with("rpc ")) {
            let name = line.trim().split_whitespace().nth(1).unwrap_or("Unknown");
            endpoints.push(Endpoint {
                method: "RPC".to_string(),
                path: name.to_string(),
            });
        }
    } else if file_lower.ends_with(".graphql") {
        for line in content
            .lines()
            .filter(|l| l.trim_start().starts_with("type Query") || l.trim_start().starts_with("type Mutation"))
        {
            endpoints.push(Endpoint {
                method: "GRAPHQL".to_string(),
                path: line.trim().to_string(),
            });
        }
    } else {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("/api/") {
                endpoints.push(Endpoint {
                    method: "HTTP".to_string(),
                    path: trimmed.trim_end_matches(':').to_string(),
                });
            }
        }
    }
    endpoints
}
