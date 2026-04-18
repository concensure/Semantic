use crate::extractor::{RustSymbol, RustSymbolKind};
use engine::DependencyRecord;

pub fn build_relationships(file: &str, symbols: &[RustSymbol]) -> Vec<DependencyRecord> {
    let mut dependencies = Vec::new();
    for symbol in symbols {
        if let RustSymbolKind::ImplBlock = symbol.kind {
            if let Some(owner) = symbol.owner.as_deref() {
                dependencies.push(DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: owner.to_string(),
                    callee_symbol: symbol.name.clone(),
                    file: file.to_string(),
                    callee_file: Some(file.to_string()),
                });
            }
            if let Some(trait_name) = symbol.trait_name.as_deref() {
                dependencies.push(DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: trait_name.to_string(),
                    callee_symbol: symbol.name.clone(),
                    file: file.to_string(),
                    callee_file: Some(file.to_string()),
                });
            }
        }
        if let RustSymbolKind::Method = symbol.kind {
            if let Some(owner) = symbol.owner.as_deref() {
                dependencies.push(DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: owner.to_string(),
                    callee_symbol: symbol.name.clone(),
                    file: file.to_string(),
                    callee_file: Some(file.to_string()),
                });
            }
        }
    }
    dependencies
}
