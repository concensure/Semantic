use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};

pub const SESSION_TTL_SECS: u64 = 60 * 60;
const MAX_SESSION_CONTEXT_ENTRIES: usize = 200;
const MAX_SESSION_RAW_SPANS: usize = 256;
const RAW_BUDGET_NORMAL_CHARS: usize = 4_000;
const RAW_BUDGET_STRICT_CHARS: usize = 1_500;
const RAW_BUDGET_INVESTIGATE_CHARS: usize = 12_000;

#[derive(Debug, Clone)]
pub struct SemanticMiddlewareState {
    pub semantic_first_enabled: bool,
    pub sessions: HashMap<String, SessionContextState>,
}

#[derive(Debug, Clone)]
pub struct SessionContextState {
    pub last_seen_epoch_s: u64,
    pub index_revision: u64,
    pub accepted_refs: HashSet<String>,
    pub accepted_order: VecDeque<String>,
    pub last_target_symbols: VecDeque<String>,
    pub intent_symbol_cache: HashMap<String, String>,
    pub summary_delivered: bool,
    pub last_summary_file_set: HashSet<String>,
    pub last_refs_hash: HashMap<String, u64>,
    pub last_context_keys: HashMap<String, Vec<(String, u64, u64)>>,
    pub raw_expanded_spans: HashSet<String>,
    pub raw_expanded_order: VecDeque<String>,
    pub raw_expansion_used_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawExpansionMode {
    Normal,
    Strict,
    Investigate,
}

impl RawExpansionMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("normal").trim().to_ascii_lowercase().as_str() {
            "strict" => Self::Strict,
            "investigate" => Self::Investigate,
            _ => Self::Normal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Strict => "strict",
            Self::Investigate => "investigate",
        }
    }

    pub fn budget_chars(self) -> usize {
        match self {
            Self::Normal => RAW_BUDGET_NORMAL_CHARS,
            Self::Strict => RAW_BUDGET_STRICT_CHARS,
            Self::Investigate => RAW_BUDGET_INVESTIGATE_CHARS,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RawExpansionControlOutcome {
    pub already_opened_hits: usize,
    pub budget_exhausted: bool,
    pub raw_chars_kept: usize,
}

impl Default for SemanticMiddlewareState {
    fn default() -> Self {
        Self {
            semantic_first_enabled: true,
            sessions: HashMap::new(),
        }
    }
}

pub fn now_epoch_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn auto_session_id() -> String {
    format!(
        "auto-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

pub fn touch_or_create_session<'a>(
    middleware: &'a mut SemanticMiddlewareState,
    session_id: &str,
    index_revision: u64,
) -> &'a mut SessionContextState {
    let now = now_epoch_s();
    middleware
        .sessions
        .retain(|_, v| now.saturating_sub(v.last_seen_epoch_s) <= SESSION_TTL_SECS);
    let entry = middleware
        .sessions
        .entry(session_id.to_string())
        .or_insert_with(|| SessionContextState {
            last_seen_epoch_s: now,
            index_revision,
            accepted_refs: HashSet::new(),
            accepted_order: VecDeque::new(),
            last_target_symbols: VecDeque::new(),
            intent_symbol_cache: HashMap::new(),
            summary_delivered: false,
            last_summary_file_set: HashSet::new(),
            last_refs_hash: HashMap::new(),
            last_context_keys: HashMap::new(),
            raw_expanded_spans: HashSet::new(),
            raw_expanded_order: VecDeque::new(),
            raw_expansion_used_chars: 0,
        });
    if entry.index_revision != index_revision {
        entry.index_revision = index_revision;
        entry.accepted_refs.clear();
        entry.accepted_order.clear();
        entry.last_target_symbols.clear();
        entry.intent_symbol_cache.clear();
        entry.summary_delivered = false;
        entry.last_refs_hash.clear();
        entry.last_context_keys.clear();
        entry.raw_expanded_spans.clear();
        entry.raw_expanded_order.clear();
        entry.raw_expansion_used_chars = 0;
    }
    entry.last_seen_epoch_s = now;
    entry
}

pub fn apply_session_context_reuse(
    result: &mut serde_json::Value,
    session: &mut SessionContextState,
) -> usize {
    let Some(context) = result.get_mut("context").and_then(|v| v.as_array_mut()) else {
        return 0;
    };
    let mut filtered = Vec::new();
    let mut reused = 0usize;
    for item in context.drain(..) {
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let start = item
            .get("start")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let end = item.get("end").and_then(|v| v.as_u64()).unwrap_or_default();
        let key = format!("{file}:{start}-{end}");
        if file.is_empty() || start == 0 || end == 0 {
            filtered.push(item);
            continue;
        }
        if session.accepted_refs.contains(&key) {
            reused += 1;
            continue;
        }
        session.accepted_refs.insert(key.clone());
        session.accepted_order.push_back(key);
        while session.accepted_order.len() > MAX_SESSION_CONTEXT_ENTRIES {
            if let Some(old) = session.accepted_order.pop_front() {
                session.accepted_refs.remove(&old);
            }
        }
        filtered.push(item);
    }
    *context = filtered;
    reused
}

pub fn fnv1a_hash(s: &str) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

pub fn refs_from_result(result: &serde_json::Value) -> Vec<(String, u64, u64)> {
    result
        .get("context")
        .and_then(|v| v.as_array())
        .map(|ctx| {
            ctx.iter()
                .filter_map(|item| {
                    Some((
                        item.get("file")?.as_str()?.to_string(),
                        item.get("start")?.as_u64()?,
                        item.get("end")?.as_u64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn context_delta(
    previous: Option<&Vec<(String, u64, u64)>>,
    current: &[(String, u64, u64)],
) -> (bool, Option<serde_json::Value>) {
    let Some(previous) = previous else {
        return (false, None);
    };
    let prev_set: HashSet<_> = previous.iter().cloned().collect();
    let curr_set: HashSet<_> = current.iter().cloned().collect();
    let added: Vec<_> = curr_set.difference(&prev_set).cloned().collect();
    let removed: Vec<_> = prev_set.difference(&curr_set).cloned().collect();
    let total_changed = added.len() + removed.len();
    if total_changed == 0 || total_changed >= 5 {
        return (false, None);
    }
    (
        true,
        Some(json!({
            "added": added.iter().map(|t| json!({"file":t.0,"start":t.1,"end":t.2})).collect::<Vec<_>>(),
            "removed": removed.iter().map(|t| json!({"file":t.0,"start":t.1,"end":t.2})).collect::<Vec<_>>(),
        })),
    )
}

pub fn apply_session_raw_expansion_controls(
    value: &mut serde_json::Value,
    session: &mut SessionContextState,
    mode: RawExpansionMode,
) -> RawExpansionControlOutcome {
    let mut outcome = RawExpansionControlOutcome::default();
    let budget_limit = mode.budget_chars();
    apply_raw_controls_recursive(value, session, budget_limit, &mut outcome);
    outcome
}

fn apply_raw_controls_recursive(
    value: &mut serde_json::Value,
    session: &mut SessionContextState,
    budget_limit: usize,
    outcome: &mut RawExpansionControlOutcome,
) {
    match value {
        serde_json::Value::Object(map) => {
            for nested in map.values_mut() {
                apply_raw_controls_recursive(nested, session, budget_limit, outcome);
            }
            maybe_compact_raw_span_object(map, session, budget_limit, outcome);
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                apply_raw_controls_recursive(item, session, budget_limit, outcome);
            }
        }
        _ => {}
    }
}

fn maybe_compact_raw_span_object(
    map: &mut serde_json::Map<String, serde_json::Value>,
    session: &mut SessionContextState,
    budget_limit: usize,
    outcome: &mut RawExpansionControlOutcome,
) {
    let Some(code) = map.get("code").and_then(|v| v.as_str()) else {
        return;
    };
    if code.is_empty() {
        return;
    }
    let Some(file) = map.get("file").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(start) = map.get("start").and_then(|v| v.as_u64()) else {
        return;
    };
    let Some(end) = map.get("end").and_then(|v| v.as_u64()) else {
        return;
    };
    let key = format!("{file}:{start}-{end}");
    if session.raw_expanded_spans.contains(&key) {
        map.remove("code");
        map.remove("raw_included");
        map.insert("already_in_context".to_string(), json!(true));
        outcome.already_opened_hits += 1;
        return;
    }
    let code_len = code.chars().count();
    if session.raw_expansion_used_chars.saturating_add(code_len) > budget_limit {
        map.remove("code");
        map.remove("raw_included");
        map.insert("raw_budget_exhausted".to_string(), json!(true));
        outcome.budget_exhausted = true;
        return;
    }
    session.raw_expansion_used_chars =
        session.raw_expansion_used_chars.saturating_add(code_len);
    outcome.raw_chars_kept = outcome.raw_chars_kept.saturating_add(code_len);
    session.raw_expanded_spans.insert(key.clone());
    session.raw_expanded_order.push_back(key);
    while session.raw_expanded_order.len() > MAX_SESSION_RAW_SPANS {
        if let Some(old) = session.raw_expanded_order.pop_front() {
            session.raw_expanded_spans.remove(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_session() -> SessionContextState {
        SessionContextState {
            last_seen_epoch_s: 0,
            index_revision: 0,
            accepted_refs: HashSet::new(),
            accepted_order: VecDeque::new(),
            last_target_symbols: VecDeque::new(),
            intent_symbol_cache: HashMap::new(),
            summary_delivered: false,
            last_summary_file_set: HashSet::new(),
            last_refs_hash: HashMap::new(),
            last_context_keys: HashMap::new(),
            raw_expanded_spans: HashSet::new(),
            raw_expanded_order: VecDeque::new(),
            raw_expansion_used_chars: 0,
        }
    }

    #[test]
    fn session_raw_controls_replace_already_opened_code_with_back_reference() {
        let mut session = empty_session();
        session
            .raw_expanded_spans
            .insert("src/main.rs:10-20".to_string());
        let mut value = json!({
            "context": [
                {
                    "file": "src/main.rs",
                    "start": 10,
                    "end": 20,
                    "code": "fn main() {}",
                    "raw_included": true
                }
            ]
        });
        let outcome =
            apply_session_raw_expansion_controls(&mut value, &mut session, RawExpansionMode::Normal);
        let item = value["context"][0].as_object().expect("context item");
        assert_eq!(item.get("code"), None);
        assert_eq!(item.get("already_in_context").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(outcome.already_opened_hits, 1);
    }

    #[test]
    fn session_raw_controls_surface_budget_exhaustion() {
        let mut session = empty_session();
        session.raw_expansion_used_chars = RAW_BUDGET_STRICT_CHARS;
        let mut value = json!({
            "code_span": {
                "file": "README.md",
                "start": 1,
                "end": 30,
                "code": "some text"
            }
        });
        let outcome =
            apply_session_raw_expansion_controls(&mut value, &mut session, RawExpansionMode::Strict);
        assert_eq!(
            value["code_span"]["raw_budget_exhausted"].as_bool(),
            Some(true)
        );
        assert!(outcome.budget_exhausted);
    }
}
