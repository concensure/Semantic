use anyhow::Result;
use engine::SymbolType;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeHealthIssue {
    pub issue_id: String,
    pub repository: String,
    pub file_path: String,
    pub symbol: Option<String>,
    pub issue_type: IssueType,
    pub severity: Severity,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IssueType {
    DeadCode,
    DuplicateLogic,
    LargeFunction,
    LargeModule,
    CircularDependency,
    UnusedImport,
    DeepNesting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
}

pub struct CodeHealthAnalyzer;

impl CodeHealthAnalyzer {
    pub fn analyze(
        repo_root: &Path,
        repository: &str,
        storage: &storage::Storage,
    ) -> Result<Vec<CodeHealthIssue>> {
        let symbols = storage.list_symbols()?;
        let dependencies = storage.list_all_dependencies()?;
        let modules = storage.list_modules()?;
        let mut issues = Vec::new();

        let mut symbol_called: HashSet<String> = HashSet::new();
        for dep in &dependencies {
            symbol_called.insert(dep.callee_symbol.clone());
        }

        for sym in &symbols {
            if matches!(sym.symbol_type, SymbolType::Function) {
                let span = sym.end_line.saturating_sub(sym.start_line) + 1;
                if span > 120 {
                    issues.push(issue(
                        repository,
                        &sym.file,
                        Some(sym.name.clone()),
                        IssueType::LargeFunction,
                        Severity::High,
                        format!("function '{}' is {} lines", sym.name, span),
                    ));
                }
                if !symbol_called.contains(&sym.name) && !sym.name.starts_with("test") {
                    issues.push(issue(
                        repository,
                        &sym.file,
                        Some(sym.name.clone()),
                        IssueType::DeadCode,
                        Severity::Medium,
                        format!("function '{}' appears unused", sym.name),
                    ));
                }
            }
            if matches!(sym.symbol_type, SymbolType::Import) && !symbol_called.contains(&sym.name) {
                issues.push(issue(
                    repository,
                    &sym.file,
                    Some(sym.name.clone()),
                    IssueType::UnusedImport,
                    Severity::Low,
                    format!("import '{}' appears unused", sym.name),
                ));
            }
        }

        let mut module_symbol_counts: HashMap<String, usize> = HashMap::new();
        for sym in &symbols {
            if let Some(module) = storage.get_module_by_file(&sym.file)? {
                *module_symbol_counts.entry(module.name).or_insert(0) += 1;
            }
        }
        for module in modules {
            let count = module_symbol_counts.get(&module.name).copied().unwrap_or_default();
            if count > 80 {
                issues.push(issue(
                    repository,
                    &module.path,
                    None,
                    IssueType::LargeModule,
                    Severity::High,
                    format!("module '{}' has {} symbols", module.name, count),
                ));
            }
        }

        let mut lengths: HashMap<u32, Vec<&engine::SymbolRecord>> = HashMap::new();
        for sym in &symbols {
            if matches!(sym.symbol_type, SymbolType::Function) {
                let len = sym.end_line.saturating_sub(sym.start_line) + 1;
                lengths.entry(len).or_default().push(sym);
            }
        }
        for (len, group) in lengths {
            if len > 20 && group.len() > 2 {
                for sym in group {
                    issues.push(issue(
                        repository,
                        &sym.file,
                        Some(sym.name.clone()),
                        IssueType::DuplicateLogic,
                        Severity::Medium,
                        format!("function length {} appears duplicated in multiple symbols", len),
                    ));
                }
            }
        }

        let mut by_file_calls: HashMap<String, HashSet<String>> = HashMap::new();
        let mut symbol_to_file = HashMap::new();
        for sym in &symbols {
            symbol_to_file.insert(sym.name.clone(), sym.file.clone());
        }
        for dep in &dependencies {
            if let (Some(from), Some(to)) = (
                symbol_to_file.get(&dep.caller_symbol),
                symbol_to_file.get(&dep.callee_symbol),
            ) {
                by_file_calls
                    .entry(from.clone())
                    .or_default()
                    .insert(to.clone());
            }
        }
        for (a, targets) in &by_file_calls {
            for b in targets {
                if by_file_calls.get(b).map(|s| s.contains(a)).unwrap_or(false) && a < b {
                    issues.push(issue(
                        repository,
                        a,
                        None,
                        IssueType::CircularDependency,
                        Severity::High,
                        format!("circular file dependency between '{}' and '{}'", a, b),
                    ));
                }
            }
        }

        for file in storage.list_files()? {
            let abs = repo_root.join(&file);
            if let Ok(content) = fs::read_to_string(abs) {
                let depth = estimate_nesting(&content);
                if depth >= 6 {
                    issues.push(issue(
                        repository,
                        &file,
                        None,
                        IssueType::DeepNesting,
                        Severity::Medium,
                        format!("estimated nesting depth {} in file '{}'", depth, file),
                    ));
                }
            }
        }

        issues.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then_with(|| a.issue_id.cmp(&b.issue_id))
        });
        Ok(issues)
    }
}

fn estimate_nesting(content: &str) -> usize {
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    for ch in content.chars() {
        if ch == '{' || ch == ':' {
            depth += 1;
            max_depth = max_depth.max(depth);
        } else if (ch == '}' || ch == '\n') && depth > 0 {
            depth = depth.saturating_sub(1);
        }
    }
    max_depth
}

fn issue(
    repository: &str,
    file_path: &str,
    symbol: Option<String>,
    issue_type: IssueType,
    severity: Severity,
    description: String,
) -> CodeHealthIssue {
    let issue_id = format!(
        "{}:{}:{:?}",
        file_path,
        symbol.clone().unwrap_or_else(|| "module".to_string()),
        issue_type
    );
    CodeHealthIssue {
        issue_id,
        repository: repository.to_string(),
        file_path: file_path.to_string(),
        symbol,
        issue_type,
        severity,
        description,
    }
}
