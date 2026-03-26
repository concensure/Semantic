pub use engine::{ErrorPattern, ErrorSolution};

use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyTier {
    Strict,
    Balanced,
    Debug,
}

impl PrivacyTier {
    fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "balanced" => Self::Balanced,
            "debug" => Self::Debug,
            _ => Self::Strict,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ErrorContext {
    pub pattern: ErrorPattern,
    pub solutions: Vec<ErrorSolution>,
}

pub struct ErrorLogger<'a> {
    storage: &'a storage::Storage,
    privacy: PrivacyTier,
    max_patterns: usize,
}

impl<'a> ErrorLogger<'a> {
    pub fn new(storage: &'a storage::Storage, repo_root: &Path) -> Self {
        let (privacy, max_patterns) = load_config(repo_root);
        Self {
            storage,
            privacy,
            max_patterns,
        }
    }

    pub fn migrate(&self) -> Result<()> {
        self.storage.ensure_error_log_schema()
    }

    /// Record a new error occurrence. Returns the pattern_id.
    pub fn record_error(
        &self,
        error_kind: &str,
        message: &str,
        file_hint: Option<&str>,
        symbol_hint: Option<&str>,
    ) -> Result<i64> {
        let normalized = normalize_message(message);
        let hash = error_hash(error_kind, &normalized);
        let file_stored = match self.privacy {
            PrivacyTier::Strict => file_hint.map(hash_path).unwrap_or_default(),
            _ => file_hint.unwrap_or("").to_string(),
        };
        let id = self.storage.upsert_error_pattern(
            &hash,
            error_kind,
            &normalized,
            &file_stored,
            symbol_hint.unwrap_or(""),
            now_ts(),
        )?;
        self.storage.prune_error_patterns(self.max_patterns)?;
        Ok(id)
    }

    /// Record a solution attempt for a pattern.
    pub fn record_solution(
        &self,
        pattern_id: i64,
        solution: &str,
        outcome: &str,
        token_cost: i64,
    ) -> Result<i64> {
        let trimmed: String = solution.chars().take(200).collect();
        self.storage
            .insert_error_solution(pattern_id, &trimmed, outcome, now_ts(), token_cost)
    }

    /// Query similar errors. Returns top matches by hit_count.
    pub fn query_similar(
        &self,
        error_kind: &str,
        message: &str,
        limit: usize,
    ) -> Result<Vec<ErrorContext>> {
        let normalized = normalize_message(message);
        let hash = error_hash(error_kind, &normalized);
        let mut patterns = self.storage.find_error_patterns_by_hash(&hash)?;
        if patterns.is_empty() {
            patterns = self.storage.find_error_patterns_by_kind(error_kind, limit)?;
        }
        patterns.truncate(limit);
        let mut out = Vec::new();
        for p in patterns {
            let solutions = self.storage.get_error_solutions(p.id)?;
            out.push(ErrorContext {
                pattern: p,
                solutions,
            });
        }
        Ok(out)
    }

    /// Build a compact injection block for the LLM (~50–150 tokens).
    /// Returns None when there are no prior matches.
    pub fn build_hint_block(
        &self,
        error_kind: &str,
        message: &str,
    ) -> Result<Option<serde_json::Value>> {
        let matches = self.query_similar(error_kind, message, 3)?;
        if matches.is_empty() {
            return Ok(None);
        }
        let top = &matches[0];
        let resolved: Vec<&str> = top
            .solutions
            .iter()
            .filter(|s| s.outcome == "resolved")
            .map(|s| s.solution.as_str())
            .collect();
        let failed: Vec<&str> = top
            .solutions
            .iter()
            .filter(|s| s.outcome == "failed")
            .map(|s| s.solution.as_str())
            .collect();
        Ok(Some(serde_json::json!({
            "error_log": {
                "pattern": top.pattern.message,
                "error_kind": top.pattern.error_kind,
                "hit_count": top.pattern.hit_count,
                "symbol_hint": top.pattern.symbol_hint,
                "last_resolved_by": resolved.first().copied().unwrap_or(""),
                "failed_attempts": failed,
            }
        })))
    }

    /// Recurring issues summary for project_summariser (hit_count >= threshold).
    pub fn recurring_issues(&self, min_hits: i64) -> Result<Vec<ErrorPattern>> {
        let all = self.storage.list_error_patterns(20)?;
        Ok(all.into_iter().filter(|p| p.hit_count >= min_hits).collect())
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn error_hash(kind: &str, normalized_message: &str) -> String {
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);
    normalized_message.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn hash_path(path: &str) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("path:{:016x}", h.finish())
}

/// Strip absolute paths, line numbers, and hex addresses from error messages.
fn normalize_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    for word in msg.split_whitespace() {
        let skip = word.starts_with('/')
            || word.starts_with('\\')
            || word.starts_with("0x")
            || word.chars().all(|c| c.is_ascii_digit() || c == ':');
        if skip {
            out.push_str("<_> ");
        } else {
            out.push_str(word);
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn load_config(repo_root: &Path) -> (PrivacyTier, usize) {
    let path = repo_root.join(".semantic").join("error_log.toml");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (PrivacyTier::Strict, 500);
    };
    let mut privacy = PrivacyTier::Strict;
    let mut max_patterns = 500usize;
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "privacy" => privacy = PrivacyTier::from_str(v),
                "max_patterns" => {
                    if let Ok(n) = v.parse() {
                        max_patterns = n;
                    }
                }
                _ => {}
            }
        }
    }
    (privacy, max_patterns)
}
