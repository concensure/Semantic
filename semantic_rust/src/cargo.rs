use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize)]
pub struct CargoWorkspaceInfo {
    pub package_name: Option<String>,
    pub members: Vec<String>,
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CargoCrateInfo {
    pub crate_name: Option<String>,
    pub crate_root: Option<String>,
    pub manifest_path: Option<PathBuf>,
}

pub fn discover_workspace(repo_root: &Path) -> CargoWorkspaceInfo {
    let manifest_path = repo_root.join("Cargo.toml");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        return CargoWorkspaceInfo::default();
    };

    let mut info = CargoWorkspaceInfo {
        manifest_path: Some(manifest_path),
        ..CargoWorkspaceInfo::default()
    };

    let mut in_workspace_members = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            if key == "name" && info.package_name.is_none() {
                info.package_name = Some(value.to_string());
            }
        }
        if trimmed == "members = [" || trimmed == "members=[" {
            in_workspace_members = true;
            continue;
        }
        if in_workspace_members {
            if trimmed == "]" {
                break;
            }
            let member = trimmed.trim_matches(',').trim().trim_matches('"');
            if !member.is_empty() {
                info.members.push(member.to_string());
            }
        }
    }

    info
}

pub fn discover_crate_for_file(repo_root: &Path, relative_file: &str) -> CargoCrateInfo {
    let absolute_file = repo_root.join(relative_file);
    let mut current = absolute_file.parent().map(PathBuf::from);
    while let Some(dir) = current {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            let crate_root = dir
                .strip_prefix(repo_root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .filter(|path| !path.is_empty())
                .or_else(|| Some(".".to_string()));
            return CargoCrateInfo {
                crate_name: parse_package_name(&manifest),
                crate_root,
                manifest_path: Some(manifest),
            };
        }
        if dir == repo_root {
            break;
        }
        current = dir.parent().map(PathBuf::from);
    }
    CargoCrateInfo::default()
}

fn parse_package_name(manifest_path: &Path) -> Option<String> {
    let raw = fs::read_to_string(manifest_path).ok()?;
    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() == "name" {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}
