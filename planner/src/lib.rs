use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryIntent {
    Debug,
    Refactor,
    Understand,
    LocateSymbol,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievalPlan {
    pub target_symbol: String,
    pub logic_radius: usize,
    pub dependency_radius: usize,
    pub include_callers: bool,
    pub scoped_modules: Vec<String>,
}

pub struct Planner;

impl Planner {
    pub fn new() -> Self {
        Self
    }

    pub fn detect_intent(&self, query: &str) -> QueryIntent {
        let q = query.to_lowercase();
        if q.contains("fix") || q.contains("bug") || q.contains("error") {
            QueryIntent::Debug
        } else if q.contains("refactor") || q.contains("rewrite") || q.contains("optimize") {
            QueryIntent::Refactor
        } else if q.contains("how does") || q.contains("explain") {
            QueryIntent::Understand
        } else {
            QueryIntent::LocateSymbol
        }
    }

    pub fn build_plan(&self, query: &str, symbols: &[String]) -> Option<RetrievalPlan> {
        self.build_plan_with_modules(query, symbols, &std::collections::HashMap::new(), &[])
    }

    pub fn build_plan_with_modules(
        &self,
        query: &str,
        symbols: &[String],
        symbol_to_module: &std::collections::HashMap<String, String>,
        module_dependencies: &[(String, String)],
    ) -> Option<RetrievalPlan> {
        let intent = self.detect_intent(query);
        let target_symbol = find_target_symbol(query, symbols)?;

        let (logic_radius, dependency_radius) = match intent {
            QueryIntent::Debug => (1, 1),
            QueryIntent::Refactor => (1, 2),
            QueryIntent::Understand => (2, 1),
            QueryIntent::LocateSymbol => (1, 1),
        };

        let mut scoped_modules = Vec::new();
        if let Some(target_module) = symbol_to_module.get(&target_symbol) {
            scoped_modules.push(target_module.clone());
            for (from, to) in module_dependencies {
                if from == target_module && !scoped_modules.contains(to) {
                    scoped_modules.push(to.clone());
                }
            }
            scoped_modules.sort();
            scoped_modules.dedup();
            if !scoped_modules.is_empty() {
                scoped_modules.retain(|m| m != target_module);
                scoped_modules.insert(0, target_module.clone());
            }
        }

        Some(RetrievalPlan {
            target_symbol,
            logic_radius,
            dependency_radius,
            include_callers: false,
            scoped_modules,
        })
    }
}

fn find_target_symbol(query: &str, symbols: &[String]) -> Option<String> {
    let q = query.to_lowercase();
    let mut candidates = symbols.to_vec();
    candidates.sort();
    candidates.reverse();

    for symbol in candidates {
        if q.contains(&symbol.to_lowercase()) {
            return Some(symbol);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Planner, QueryIntent};
    use std::collections::HashMap;

    #[test]
    fn detects_intent() {
        let planner = Planner::new();
        assert_eq!(planner.detect_intent("fix bug in retry"), QueryIntent::Debug);
        assert_eq!(planner.detect_intent("refactor fetchData"), QueryIntent::Refactor);
        assert_eq!(planner.detect_intent("how does fetchData work"), QueryIntent::Understand);
    }

    #[test]
    fn builds_plan_deterministically() {
        let planner = Planner::new();
        let plan = planner
            .build_plan(
                "refactor fetchData and optimize retries",
                &["fetchData".to_string(), "retryRequest".to_string()],
            )
            .expect("plan");
        assert_eq!(plan.target_symbol, "fetchData");
        assert_eq!(plan.dependency_radius, 2);
        assert_eq!(plan.logic_radius, 1);
        assert!(plan.scoped_modules.is_empty());
    }

    #[test]
    fn builds_module_scoped_plan() {
        let planner = Planner::new();
        let mut symbol_to_module = HashMap::new();
        symbol_to_module.insert("fetchData".to_string(), "api".to_string());
        let module_deps = vec![("api".to_string(), "utils".to_string())];

        let plan = planner
            .build_plan_with_modules(
                "refactor fetchData",
                &["fetchData".to_string()],
                &symbol_to_module,
                &module_deps,
            )
            .expect("plan");
        assert_eq!(plan.scoped_modules, vec!["api".to_string(), "utils".to_string()]);
    }
}
