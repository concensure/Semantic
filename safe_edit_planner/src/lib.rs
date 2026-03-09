use anyhow::Result;
use engine::{EditContextItem, EditPlan, EditType};

pub struct SafeEditPlanner;

impl SafeEditPlanner {
    pub fn plan(storage: &storage::Storage, symbol: &str, edit_description: &str) -> Result<EditPlan> {
        Self::plan_with_risk(storage, symbol, edit_description, 0.0)
    }

    pub fn plan_with_memory(
        storage: &storage::Storage,
        symbol: &str,
        edit_description: &str,
        memory: &patch_memory::PatchMemory,
    ) -> Result<EditPlan> {
        let edit_type = classify_edit_type(edit_description);
        let risk = memory.edit_risk_score(edit_type.clone())?.risk;
        Self::plan_with_risk(storage, symbol, edit_description, risk)
    }

    fn plan_with_risk(
        storage: &storage::Storage,
        symbol: &str,
        edit_description: &str,
        risk: f32,
    ) -> Result<EditPlan> {
        let impact = impact_analysis::ImpactAnalyzer::analyze(storage, symbol)?;
        let edit_type = classify_edit_type(edit_description);

        let mut required_context = Vec::new();
        if let Some(sym) = storage.get_symbol_any(symbol)? {
            required_context.push(EditContextItem {
                file_path: sym.file,
                start_line: sym.start_line as usize,
                end_line: sym.end_line as usize,
                priority: 0,
                text: format!("context for {symbol}"),
            });
        }

        for impacted in &impact.impacted_symbols {
            if let Some(sym) = storage.get_symbol_any(impacted)? {
                required_context.push(EditContextItem {
                    file_path: sym.file,
                    start_line: sym.start_line as usize,
                    end_line: sym.end_line as usize,
                    priority: 1,
                    text: format!("impacted {impacted}"),
                });
            }
        }

        if risk >= 0.4 {
            if let Some(target) = storage.get_symbol_any(symbol)? {
                if let Some(target_id) = target.id {
                    let callers = storage.get_symbol_callers(target_id)?;
                    for caller in callers {
                        let caller_name = caller.name.clone();
                        required_context.push(EditContextItem {
                            file_path: caller.file,
                            start_line: caller.start_line as usize,
                            end_line: caller.end_line as usize,
                            priority: 1,
                            text: format!("caller context for {caller_name}"),
                        });
                    }
                }
            }
        }

        required_context.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        required_context.dedup_by(|a, b| {
            a.file_path == b.file_path
                && a.start_line == b.start_line
                && a.end_line == b.end_line
                && a.priority == b.priority
        });

        Ok(EditPlan {
            target_symbol: symbol.to_string(),
            edit_type,
            impacted_symbols: impact.impacted_symbols,
            required_context,
        })
    }
}

fn classify_edit_type(text: &str) -> EditType {
    let t = text.to_lowercase();
    if t.contains("rename") {
        EditType::RenameSymbol
    } else if t.contains("signature") || t.contains("parameter") {
        EditType::ChangeSignature
    } else if t.contains("refactor") {
        EditType::RefactorFunction
    } else {
        EditType::ModifyLogic
    }
}

#[cfg(test)]
mod tests {
    use super::SafeEditPlanner;
    use patch_memory::PatchMemory;
    use engine::{DependencyRecord, SymbolRecord, SymbolType};
    use storage::Storage;

    #[test]
    fn includes_target_and_impacted_context() {
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
                    },
                ],
                &[DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[],
            )
            .expect("replace index");

        let plan = SafeEditPlanner::plan(&storage, "retryRequest", "refactor retry logic")
            .expect("safe plan");
        assert_eq!(plan.target_symbol, "retryRequest");
        assert!(plan.required_context.len() >= 2);
    }

    #[test]
    fn planner_uses_patch_memory_risk_for_context_expansion() {
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
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "callerA".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 10,
                        end_line: 12,
                        language: "typescript".into(),
                        summary: "Function callerA".into(),
                    },
                ],
                &[DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "callerA".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[],
            )
            .expect("replace index");

        let mem = PatchMemory::open(tmp.path()).expect("patch memory");
        mem.append_record(&engine::PatchRecord {
            patch_id: "p_1".into(),
            timestamp: 1,
            repository: "repo".into(),
            file_path: "src/client.ts".into(),
            target_symbol: "retryRequest".into(),
            edit_type: engine::EditType::RenameSymbol,
            model_used: "openai".into(),
            provider: "openai".into(),
            diff: "x".into(),
            ast_transform: Some(engine::ASTTransformation::RenameSymbol),
            impacted_symbols: vec!["callerA".into()],
            approved_by_user: false,
            validation_passed: false,
            tests_passed: false,
            rollback_occurred: true,
            rollback_reason: Some("validation_failed".into()),
        })
        .expect("append");

        let plan = SafeEditPlanner::plan_with_memory(&storage, "retryRequest", "rename symbol", &mem)
            .expect("plan");
        assert!(plan.required_context.iter().any(|c| c.text.contains("caller context")));
    }
}
