use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionLevel {
    Strict,
    Balanced,
    Debug,
}

impl RedactionLevel {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "debug" => Self::Debug,
            "balanced" => Self::Balanced,
            _ => Self::Strict,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub log_path: PathBuf,
    pub redaction_level: RedactionLevel,
}

impl TelemetryConfig {
    pub fn from_repo_root(repo_root: &Path) -> Self {
        let semantic_dir = repo_root.join(".semantic");
        let default_dir = semantic_dir.join("token_tracking");
        let default_path = default_dir.join("events.ndjson");
        let config_path = semantic_dir.join("token_tracking.toml");
        let mut cfg = Self {
            enabled: std::env::var("SEMANTIC_TOKEN_TRACKING_ENABLED")
                .map(|v| {
                    matches!(
                        v.trim().to_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
                .unwrap_or(false),
            log_path: std::env::var("SEMANTIC_TOKEN_TRACKING_LOG_PATH")
                .map(PathBuf::from)
                .unwrap_or(default_path),
            redaction_level: std::env::var("SEMANTIC_TOKEN_TRACKING_REDACTION")
                .map(|v| RedactionLevel::parse(&v))
                .unwrap_or(RedactionLevel::Strict),
        };
        let Ok(raw) = fs::read_to_string(config_path) else {
            return cfg;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "enabled" => {
                    cfg.enabled =
                        matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
                }
                "redaction_level" => {
                    cfg.redaction_level = RedactionLevel::parse(value);
                }
                "log_path" => {
                    let path = PathBuf::from(value);
                    cfg.log_path = if path.is_absolute() {
                        path
                    } else {
                        repo_root.join(path)
                    };
                }
                _ => {}
            }
        }
        cfg
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScope {
    pub session_id: Option<String>,
    pub task_id: String,
    pub route_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub event_id: String,
    pub schema_version: u32,
    pub timestamp: u64,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub route_id: Option<String>,
    pub parent_event_id: Option<String>,
    pub event_type: String,
    pub component: String,
    pub category: Option<String>,
    pub tool_name: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_tokens_reported: Option<usize>,
    pub completion_tokens_reported: Option<usize>,
    pub total_tokens_reported: Option<usize>,
    pub prompt_tokens_estimated: Option<usize>,
    pub completion_tokens_estimated: Option<usize>,
    pub latency_ms: Option<u128>,
    pub status: Option<String>,
    pub error_code: Option<String>,
    pub redaction_level: RedactionLevel,
    pub metadata: Value,
}

#[derive(Debug, Default)]
struct EventWriter {
    file: Option<File>,
}

#[derive(Debug)]
struct TelemetryInner {
    config: TelemetryConfig,
    writer: Mutex<EventWriter>,
    counter: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct TelemetrySink {
    inner: Arc<TelemetryInner>,
}

thread_local! {
    static TASK_SCOPE: RefCell<Option<(TelemetrySink, TaskScope)>> = const { RefCell::new(None) };
}

impl TelemetrySink {
    pub fn from_repo_root(repo_root: &Path) -> Self {
        Self::new(TelemetryConfig::from_repo_root(repo_root))
    }

    pub fn new(config: TelemetryConfig) -> Self {
        Self {
            inner: Arc::new(TelemetryInner {
                config,
                writer: Mutex::new(EventWriter::default()),
                counter: AtomicU64::new(1),
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.config.enabled
    }

    pub fn config(&self) -> &TelemetryConfig {
        &self.inner.config
    }

    pub fn with_task_scope<T, F>(&self, scope: TaskScope, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        TASK_SCOPE.with(|slot| {
            let previous = slot.replace(Some((self.clone(), scope)));
            let result = f();
            slot.replace(previous);
            result
        })
    }

    pub fn current_scope() -> Option<(TelemetrySink, TaskScope)> {
        TASK_SCOPE.with(|slot| slot.borrow().clone())
    }

    pub fn emit(&self, event: TelemetryEvent) {
        if !self.enabled() {
            return;
        }
        let mut writer = self.inner.writer.lock();
        if writer.file.is_none() {
            if let Some(parent) = self.inner.config.log_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            writer.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.inner.config.log_path)
                .ok();
        }
        let Some(file) = writer.file.as_mut() else {
            return;
        };
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }

    pub fn next_event_id(&self, prefix: &str) -> String {
        let ts = now_epoch_ms();
        let seq = self.inner.counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{ts}-{seq}")
    }

    pub fn event(
        &self,
        scope: Option<&TaskScope>,
        event_type: &str,
        component: &str,
        category: Option<&str>,
    ) -> TelemetryEvent {
        TelemetryEvent {
            event_id: self.next_event_id(event_type),
            schema_version: 1,
            timestamp: now_epoch_ms(),
            session_id: scope.and_then(|s| s.session_id.clone()),
            task_id: scope.map(|s| s.task_id.clone()),
            route_id: scope.map(|s| s.route_id.clone()),
            parent_event_id: None,
            event_type: event_type.to_string(),
            component: component.to_string(),
            category: category.map(|v| v.to_string()),
            tool_name: None,
            provider: None,
            model: None,
            prompt_tokens_reported: None,
            completion_tokens_reported: None,
            total_tokens_reported: None,
            prompt_tokens_estimated: None,
            completion_tokens_estimated: None,
            latency_ms: None,
            status: None,
            error_code: None,
            redaction_level: self.inner.config.redaction_level,
            metadata: Value::Object(Map::new()),
        }
    }

    pub fn sanitize_metadata(&self, metadata: Value) -> Value {
        sanitize_value(metadata, self.inner.config.redaction_level)
    }

    pub fn path_label(&self, path: &str) -> String {
        match self.inner.config.redaction_level {
            RedactionLevel::Debug => path.to_string(),
            _ => format!("path:{}", hash_string(path)),
        }
    }
}

pub fn emit_current<F>(builder: F)
where
    F: FnOnce(&TelemetrySink, &TaskScope) -> TelemetryEvent,
{
    if let Some((sink, scope)) = TelemetrySink::current_scope() {
        sink.emit(builder(&sink, &scope));
    }
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn estimate_tokens(text: &str) -> usize {
    ((text.len() as f32) / 4.0).ceil() as usize
}

pub fn hash_string(input: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

fn sanitize_value(value: Value, level: RedactionLevel) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                let lowered = key.to_lowercase();
                if lowered.contains("api_key")
                    || lowered.contains("authorization")
                    || lowered.contains("auth_header")
                    || lowered.contains("bridge_token")
                {
                    continue;
                }
                if lowered.contains("prompt")
                    || lowered.contains("content")
                    || lowered.contains("output")
                {
                    match level {
                        RedactionLevel::Strict => continue,
                        RedactionLevel::Balanced => {
                            if let Some(snippet) = value.as_str() {
                                out.insert(key, Value::String(truncate(snippet, 120)));
                            }
                            continue;
                        }
                        RedactionLevel::Debug => {}
                    }
                }
                if lowered.contains("path") || lowered.contains("file") {
                    if let Some(raw) = value.as_str() {
                        match level {
                            RedactionLevel::Debug => {
                                out.insert(key, Value::String(raw.to_string()));
                            }
                            _ => {
                                out.insert(
                                    key,
                                    Value::String(format!("path:{}", hash_string(raw))),
                                );
                            }
                        }
                        continue;
                    }
                }
                out.insert(key, sanitize_value(value, level));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| sanitize_value(item, level))
                .collect(),
        ),
        Value::String(raw) => match level {
            RedactionLevel::Strict if raw.len() > 120 => Value::String(truncate(&raw, 32)),
            _ => Value::String(raw),
        },
        other => other,
    }
}

fn truncate(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    format!("{}...", &value[..max_len])
}

pub fn metadata_pairs(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

pub fn task_summary_metadata(
    operation: &str,
    ok: bool,
    extra: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let mut map = Map::new();
    map.insert("operation".to_string(), json!(operation));
    map.insert("ok".to_string(), json!(ok));
    for (key, value) in extra {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}
