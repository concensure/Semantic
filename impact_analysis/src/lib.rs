use anyhow::Result;
use engine::{ImpactReport, SignatureImpact};

pub struct ImpactAnalyzer;

impl ImpactAnalyzer {
    pub fn analyze(storage: &storage::Storage, changed_symbol: &str) -> Result<ImpactReport> {
        let deps = storage.list_all_dependencies()?;
        let inv = invalidation_engine::InvalidationEngine::build(&deps);
        let impacted_symbols = inv.stale_symbols_for(changed_symbol);

        let mut impacted_files = Vec::new();
        let mut impacted_tests = Vec::new();
        for symbol_name in &impacted_symbols {
            if let Some(sym) = storage.get_symbol_any(symbol_name)? {
                impacted_files.push(sym.file.clone());
                let lower = sym.file.to_lowercase();
                if lower.contains("test") || lower.contains("spec") {
                    impacted_tests.push(sym.file);
                }
            }
        }
        impacted_files.sort();
        impacted_files.dedup();
        impacted_tests.sort();
        impacted_tests.dedup();

        let signature_impact = Some(SignatureImpact {
            callers: impacted_symbols.clone(),
            implementors: Vec::new(),
        });

        Ok(ImpactReport {
            changed_symbol: changed_symbol.to_string(),
            impacted_symbols,
            impacted_files,
            impacted_tests,
            signature_impact,
        })
    }

    pub fn analyze_in_file(
        storage: &storage::Storage,
        changed_symbol: &str,
        changed_file: &str,
    ) -> Result<ImpactReport> {
        let target = storage
            .file_outline(changed_file)?
            .into_iter()
            .find(|symbol| symbol.name.eq_ignore_ascii_case(changed_symbol));

        let impacted_callers = match target.and_then(|symbol| symbol.id) {
            Some(symbol_id) => storage.get_symbol_callers(symbol_id)?,
            None => return Self::analyze(storage, changed_symbol),
        };

        let impacted_symbols = impacted_callers
            .iter()
            .map(|symbol| symbol.name.clone())
            .collect::<Vec<_>>();
        let mut impacted_files = impacted_callers
            .iter()
            .map(|symbol| symbol.file.clone())
            .collect::<Vec<_>>();
        let mut impacted_tests = impacted_callers
            .iter()
            .filter_map(|symbol| {
                let lower = symbol.file.to_lowercase();
                if lower.contains("test") || lower.contains("spec") {
                    Some(symbol.file.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        impacted_files.sort();
        impacted_files.dedup();
        impacted_tests.sort();
        impacted_tests.dedup();

        let signature_impact = Some(SignatureImpact {
            callers: impacted_symbols.clone(),
            implementors: Vec::new(),
        });

        Ok(ImpactReport {
            changed_symbol: changed_symbol.to_string(),
            impacted_symbols,
            impacted_files,
            impacted_tests,
            signature_impact,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ImpactAnalyzer;
    use engine::{DependencyRecord, SymbolRecord, SymbolType};
    use storage::Storage;

    #[test]
    fn computes_impacted_symbols_and_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("storage");

        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "retryRequest".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 1,
                        end_line: 3,
                        language: "typescript".into(),
                        summary: "Function retryRequest".into(),
                        signature: None,
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "fetchData".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 5,
                        end_line: 9,
                        language: "typescript".into(),
                        summary: "Function fetchData".into(),
                        signature: None,
                    },
                ],
                &[DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                    callee_file: None,
                }],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        let report = ImpactAnalyzer::analyze(&storage, "retryRequest").expect("impact");
        assert!(report.impacted_symbols.iter().any(|s| s == "fetchData"));
        assert!(report.impacted_files.iter().any(|f| f == "src/client.ts"));
    }

    #[test]
    fn computes_impacted_symbols_in_exact_file_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("storage");

        storage
            .replace_file_index(
                0,
                "packages/api/auth.py",
                "python",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "load_config".into(),
                        symbol_type: SymbolType::Function,
                        file: "packages/api/auth.py".into(),
                        start_line: 1,
                        end_line: 3,
                        language: "python".into(),
                        summary: "Function load_config".into(),
                        signature: None,
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "init_auth".into(),
                        symbol_type: SymbolType::Function,
                        file: "packages/api/auth.py".into(),
                        start_line: 5,
                        end_line: 8,
                        language: "python".into(),
                        summary: "Function init_auth".into(),
                        signature: None,
                    },
                ],
                &[DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "init_auth".into(),
                    callee_symbol: "load_config".into(),
                    file: "packages/api/auth.py".into(),
                    callee_file: Some("packages/api/auth.py".into()),
                }],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace api index");

        storage
            .replace_file_index(
                0,
                "packages/worker/auth.py",
                "python",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "load_config".into(),
                        symbol_type: SymbolType::Function,
                        file: "packages/worker/auth.py".into(),
                        start_line: 1,
                        end_line: 3,
                        language: "python".into(),
                        summary: "Function load_config".into(),
                        signature: None,
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "init_auth".into(),
                        symbol_type: SymbolType::Function,
                        file: "packages/worker/auth.py".into(),
                        start_line: 5,
                        end_line: 8,
                        language: "python".into(),
                        summary: "Function init_auth".into(),
                        signature: None,
                    },
                ],
                &[DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "init_auth".into(),
                    callee_symbol: "load_config".into(),
                    file: "packages/worker/auth.py".into(),
                    callee_file: Some("packages/worker/auth.py".into()),
                }],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace worker index");

        let report = ImpactAnalyzer::analyze_in_file(&storage, "load_config", "packages/api/auth.py")
            .expect("impact");
        assert_eq!(report.impacted_symbols, vec!["init_auth".to_string()]);
        assert_eq!(report.impacted_files, vec!["packages/api/auth.py".to_string()]);
    }
}
