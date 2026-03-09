use engine::DependencyRecord;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct InvalidationEngine {
    symbol_dependents: HashMap<String, HashSet<String>>,
}

impl InvalidationEngine {
    pub fn build(deps: &[DependencyRecord]) -> Self {
        let mut out = Self::default();
        for dep in deps {
            out.symbol_dependents
                .entry(dep.callee_symbol.clone())
                .or_default()
                .insert(dep.caller_symbol.clone());
        }
        out
    }

    pub fn stale_symbols_for(&self, changed_symbol: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .symbol_dependents
            .get(changed_symbol)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        out.sort();
        out
    }
}
