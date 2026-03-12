use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
        self.build_plan_with_modules_and_hint(
            query,
            symbols,
            symbol_to_module,
            module_dependencies,
            None,
        )
    }

    pub fn build_plan_with_modules_and_hint(
        &self,
        query: &str,
        symbols: &[String],
        symbol_to_module: &std::collections::HashMap<String, String>,
        module_dependencies: &[(String, String)],
        preferred_symbol: Option<&str>,
    ) -> Option<RetrievalPlan> {
        let intent = self.detect_intent(query);
        let target_symbol = find_target_symbol(query, symbols, preferred_symbol)?;

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

fn find_target_symbol(
    query: &str,
    symbols: &[String],
    preferred_symbol: Option<&str>,
) -> Option<String> {
    if symbols.is_empty() {
        return None;
    }
    if let Some(preferred) = preferred_symbol {
        let trimmed = preferred.trim();
        if !trimmed.is_empty() {
            if let Some(exact) = symbols
                .iter()
                .find(|s| s.eq_ignore_ascii_case(trimmed))
                .cloned()
            {
                return Some(exact);
            }
        }
    }

    let q_lc = query.to_lowercase();
    let q_norm = normalize_for_match(query);
    let q_tokens: HashSet<&str> = q_norm.split_whitespace().collect();
    let preferred_norm = preferred_symbol
        .map(normalize_for_match)
        .unwrap_or_default();

    let mut best: Option<(&String, i64)> = None;
    for symbol in symbols {
        let sym_lc = symbol.to_lowercase();
        let sym_norm = normalize_for_match(symbol);
        let sym_tokens: Vec<&str> = sym_norm.split_whitespace().collect();
        let multi_token_symbol = sym_tokens.len() >= 2;
        let short_single_token = sym_tokens.len() == 1 && sym_tokens[0].len() <= 3;

        let mut score = 0i64;
        if q_lc.contains(&sym_lc) {
            score += if multi_token_symbol {
                90 + sym_lc.len() as i64
            } else {
                20 + sym_lc.len() as i64
            };
        }
        if !sym_norm.is_empty() && q_norm.contains(&sym_norm) {
            score += if multi_token_symbol {
                220 + sym_norm.len() as i64
            } else {
                60 + sym_norm.len() as i64
            };
        }
        if !sym_tokens.is_empty() && sym_tokens.iter().all(|tok| q_tokens.contains(tok)) {
            score += if multi_token_symbol {
                120 + (sym_tokens.len() as i64 * 3)
            } else {
                25
            };
        }
        let overlap = sym_tokens
            .iter()
            .filter(|tok| q_tokens.contains(**tok))
            .count() as i64;
        score += overlap * 5;
        if short_single_token {
            score -= 30;
        }
        if !preferred_norm.is_empty() && sym_norm == preferred_norm {
            score += 50;
        }

        if score <= 0 {
            continue;
        }
        match best {
            None => best = Some((symbol, score)),
            Some((best_symbol, best_score)) => {
                if score > best_score || (score == best_score && symbol.len() > best_symbol.len()) {
                    best = Some((symbol, score));
                }
            }
        }
    }
    best.map(|(s, _)| s.clone())
}

fn normalize_for_match(input: &str) -> String {
    let mut out = String::new();
    let mut prev_was_space = true;
    let mut prev_was_lower_or_digit = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_was_lower_or_digit && !prev_was_space {
                out.push(' ');
            }
            out.push(ch.to_ascii_lowercase());
            prev_was_space = false;
            prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else if !prev_was_space {
            out.push(' ');
            prev_was_space = true;
            prev_was_lower_or_digit = false;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Planner, QueryIntent};
    use std::collections::HashMap;

    #[test]
    fn detects_intent() {
        let planner = Planner::new();
        assert_eq!(
            planner.detect_intent("fix bug in retry"),
            QueryIntent::Debug
        );
        assert_eq!(
            planner.detect_intent("refactor fetchData"),
            QueryIntent::Refactor
        );
        assert_eq!(
            planner.detect_intent("how does fetchData work"),
            QueryIntent::Understand
        );
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
        assert_eq!(
            plan.scoped_modules,
            vec!["api".to_string(), "utils".to_string()]
        );
    }

    #[test]
    fn matches_camel_symbol_from_spaced_query() {
        let planner = Planner::new();
        let symbols = vec![
            "App".to_string(),
            "createTask".to_string(),
            "addTask".to_string(),
        ];
        let plan = planner
            .build_plan("todo app create task due date validation", &symbols)
            .expect("plan");
        assert_eq!(plan.target_symbol, "createTask");
    }

    #[test]
    fn honors_preferred_symbol_hint() {
        let planner = Planner::new();
        let symbols = vec![
            "App".to_string(),
            "TaskMenu".to_string(),
            "listTasks".to_string(),
        ];
        let plan = planner
            .build_plan_with_modules_and_hint(
                "todo app ui tools menu integrate actions app navigation",
                &symbols,
                &HashMap::new(),
                &[],
                Some("TaskMenu"),
            )
            .expect("plan");
        assert_eq!(plan.target_symbol, "TaskMenu");
    }
}
