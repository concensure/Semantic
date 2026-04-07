use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TestGapType {
    NoTests,
    MissingEdgeCases,
    MissingErrorTests,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGap {
    pub symbol: String,
    pub file_path: String,
    pub gap_type: TestGapType,
}

pub struct TestCoverageAnalyzer;

impl TestCoverageAnalyzer {
    pub fn has_gap_for_symbol(storage: &storage::Storage, symbol_name: &str) -> Result<bool> {
        let sym_name = symbol_name.to_lowercase();
        let files = storage.list_files()?;
        let test_files = files
            .into_iter()
            .filter(|f| {
                let lower = f.to_lowercase();
                lower.contains("test")
                    || lower.contains("__tests__")
                    || lower.ends_with("_spec.rs")
                    || lower.ends_with(".spec.ts")
                    || lower.ends_with(".test.ts")
            })
            .collect::<Vec<_>>();

        let mut test_symbols = HashSet::new();
        for file in test_files {
            for sym in storage.file_outline(&file)? {
                test_symbols.insert(sym.name.to_lowercase());
            }
        }

        let has_test = test_symbols
            .iter()
            .any(|ts| ts.contains(&sym_name) || ts.contains("test"));
        if !has_test {
            return Ok(true);
        }

        let missing_edge_cases = !(sym_name.contains("edge") || sym_name.contains("boundary"));
        let missing_error_tests = !(sym_name.contains("error") || sym_name.contains("fail"));
        Ok(missing_edge_cases || missing_error_tests)
    }

    pub fn analyze(storage: &storage::Storage) -> Result<Vec<TestGap>> {
        let symbols = storage.list_symbols()?;
        let files = storage.list_files()?;

        let test_files = files
            .into_iter()
            .filter(|f| {
                let lower = f.to_lowercase();
                lower.contains("test")
                    || lower.contains("__tests__")
                    || lower.ends_with("_spec.rs")
                    || lower.ends_with(".spec.ts")
                    || lower.ends_with(".test.ts")
            })
            .collect::<Vec<_>>();

        let mut test_symbols = HashSet::new();
        for file in test_files {
            for sym in storage.file_outline(&file)? {
                test_symbols.insert(sym.name.to_lowercase());
            }
        }

        let mut gaps = Vec::new();
        for sym in symbols {
            let sym_name = sym.name.to_lowercase();
            let has_test = test_symbols
                .iter()
                .any(|ts| ts.contains(&sym_name) || ts.contains("test"));
            if !has_test {
                gaps.push(TestGap {
                    symbol: sym.name.clone(),
                    file_path: sym.file.clone(),
                    gap_type: TestGapType::NoTests,
                });
                continue;
            }

            if !(sym_name.contains("edge") || sym_name.contains("boundary")) {
                gaps.push(TestGap {
                    symbol: sym.name.clone(),
                    file_path: sym.file.clone(),
                    gap_type: TestGapType::MissingEdgeCases,
                });
            }
            if !(sym_name.contains("error") || sym_name.contains("fail")) {
                gaps.push(TestGap {
                    symbol: sym.name,
                    file_path: sym.file,
                    gap_type: TestGapType::MissingErrorTests,
                });
            }
        }

        gaps.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        Ok(gaps)
    }
}
