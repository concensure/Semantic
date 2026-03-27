use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_tokens: usize,
    pub reserved_prompt: usize,
}

impl ContextBudget {
    pub fn available_tokens(&self) -> usize {
        self.max_tokens.saturating_sub(self.reserved_prompt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub file_path: String,
    pub module_name: String,
    pub module_rank: u8,
    pub start_line: usize,
    pub end_line: usize,
    pub priority: u8,
    pub text: String,
}

pub fn estimate_tokens(text: &str) -> usize {
    // Use chars/3 rather than chars/4 to add a ~25% safety margin.
    // Code tokens (Rust/TypeScript generics, punctuation, short identifiers)
    // average ~3 chars each, so chars/4 consistently underestimates and causes
    // the budget to select more items than actually fit in the context window.
    text.chars().count().div_ceil(3)
}

pub fn select_with_budget(mut items: Vec<ContextItem>, budget: &ContextBudget) -> Vec<ContextItem> {
    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.module_rank.cmp(&b.module_rank))
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.end_line.cmp(&b.end_line))
    });

    let mut selected = Vec::new();
    let mut remaining = budget.available_tokens();

    for item in items {
        let cost = estimate_tokens(&item.text);
        if cost <= remaining {
            remaining -= cost;
            selected.push(item);
        } else {
            break;
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::{estimate_tokens, select_with_budget, ContextBudget, ContextItem};

    #[test]
    fn token_estimation_is_stable() {
        // chars/3 with div_ceil: 4 chars → 2, 6 chars → 2, 9 chars → 3
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("abcdefghi"), 3);
    }

    #[test]
    fn budget_truncates_by_priority() {
        let items = vec![
            ContextItem {
                file_path: "b.ts".into(),
                module_name: "utils".into(),
                module_rank: 1,
                start_line: 10,
                end_line: 20,
                priority: 2,
                text: "x".repeat(200),
            },
            ContextItem {
                file_path: "a.ts".into(),
                module_name: "api".into(),
                module_rank: 0,
                start_line: 1,
                end_line: 5,
                priority: 0,
                text: "x".repeat(40),
            },
        ];

        let budget = ContextBudget {
            max_tokens: 20,
            reserved_prompt: 5,
        };
        let selected = select_with_budget(items, &budget);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].priority, 0);
    }
}
