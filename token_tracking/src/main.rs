use anyhow::Result;
use axum::{
    extract::{Path, State},
    response::Html,
    routing::get,
    Json, Router,
};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use telemetry::{RedactionLevel, TelemetryConfig, TelemetryEvent};

#[derive(Clone)]
struct AppState {
    log_path: PathBuf,
    db_path: PathBuf,
    redaction_level: RedactionLevel,
}

#[derive(Debug, Clone)]
struct EventRow {
    timestamp: u64,
    task_id: Option<String>,
    route_id: Option<String>,
    event_type: String,
    category: Option<String>,
    tool_name: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    total_tokens_reported: Option<usize>,
    prompt_tokens_estimated: Option<usize>,
    completion_tokens_estimated: Option<usize>,
    latency_ms: Option<u128>,
    status: Option<String>,
    error_code: Option<String>,
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct TaskSummary {
    task_id: String,
    route_id: String,
    started_at: u64,
    duration_ms: u64,
    success: bool,
    total_tokens: usize,
    repeated_errors: usize,
    wasted_calls: usize,
    top_category: String,
    category_tokens: serde_json::Value,
}

#[tokio::main]
async fn main() -> Result<()> {
    let repo_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let config = TelemetryConfig::from_repo_root(&repo_root);
    let db_path = repo_root
        .join(".semantic")
        .join("token_tracking")
        .join("tracker.sqlite");
    let state = AppState {
        log_path: config.log_path,
        db_path,
        redaction_level: config.redaction_level,
    };
    init_db(&state.db_path)?;

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/tasks", get(get_tasks))
        .route("/api/tasks/:task_id", get(get_task_detail))
        .route("/api/hotspots", get(get_hotspots))
        .route("/api/trends", get(get_trends))
        .with_state(Arc::new(state));

    let addr: SocketAddr = "127.0.0.1:4319".parse()?;
    println!("token tracking dashboard listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

async fn get_tasks(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tasks = refresh_and_load_tasks(&state).unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "redaction_level": format!("{:?}", state.redaction_level).to_lowercase(),
        "tasks": tasks,
    }))
}

async fn get_task_detail(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Json<serde_json::Value> {
    let events = refresh_and_load_events(&state, Some(&task_id)).unwrap_or_default();
    let hints = build_hints(&events);
    let timeline: Vec<_> = events
        .iter()
        .map(|event| {
            serde_json::json!({
                "timestamp": event.timestamp,
                "event_type": event.event_type,
                "category": event.category,
                "tool_name": event.tool_name,
                "provider": event.provider,
                "model": event.model,
                "status": event.status,
                "tokens": event_tokens(event),
                "latency_ms": event.latency_ms,
                "error_code": event.error_code,
                "metadata": event.metadata,
            })
        })
        .collect();
    Json(serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "timeline": timeline,
        "hints": hints,
    }))
}

async fn get_hotspots(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let events = refresh_and_load_events(&state, None).unwrap_or_default();
    let mut by_tool: HashMap<String, usize> = HashMap::new();
    let mut by_provider: HashMap<String, usize> = HashMap::new();
    let mut wasted = 0usize;
    for event in &events {
        let tokens = event_tokens(event);
        if let Some(tool) = &event.tool_name {
            *by_tool.entry(tool.clone()).or_default() += tokens;
        }
        if let Some(provider) = &event.provider {
            *by_provider.entry(provider.clone()).or_default() += tokens;
        }
        if event.status.as_deref() == Some("error") {
            wasted += tokens.max(event.prompt_tokens_estimated.unwrap_or_default());
        }
    }
    Json(serde_json::json!({
        "ok": true,
        "top_tools": top_counts(by_tool),
        "top_providers": top_counts(by_provider),
        "wasted_tokens_estimated": wasted,
    }))
}

async fn get_trends(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tasks = refresh_and_load_tasks(&state).unwrap_or_default();
    let mut by_day: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for task in tasks {
        let day = day_label(task.started_at);
        let entry = by_day.entry(day).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += task.total_tokens;
        if task.success {
            entry.2 += 1;
        }
    }
    let rows: Vec<_> = by_day
        .into_iter()
        .map(|(day, (tasks, tokens, success))| {
            serde_json::json!({
                "day": day,
                "tasks": tasks,
                "tokens": tokens,
                "success_rate_pct": if tasks == 0 { 0.0 } else { (success as f64 / tasks as f64) * 100.0 }
            })
        })
        .collect();
    Json(serde_json::json!({"ok": true, "rows": rows}))
}

async fn index() -> Html<&'static str> {
    Html(r#"<!doctype html>
<html>
<head>
<meta charset="utf-8"/>
<title>Semantic Token Tracking</title>
<style>
:root{--bg:#f6f2ea;--panel:#fffdf8;--line:#d4c8b7;--ink:#1e1b16;--muted:#6b6256;--accent:#b45309;--warn:#b91c1c}
body{margin:0;background:radial-gradient(circle at top,#fff7ed,transparent 28%),var(--bg);color:var(--ink);font:14px/1.45 "Segoe UI",sans-serif}
.shell{display:grid;grid-template-columns:380px 1fr;min-height:100vh}
.panel{padding:24px;border-right:1px solid var(--line);background:rgba(255,253,248,.9);backdrop-filter:blur(4px)}
.detail{padding:24px}
h1,h2{margin:0 0 12px}
.kpi{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:12px;margin:0 0 18px}
.card,.task{background:var(--panel);border:1px solid var(--line);border-radius:16px;padding:14px;box-shadow:0 12px 30px rgba(30,27,22,.06)}
.task{margin-bottom:12px;cursor:pointer}
.task strong{display:block}
.muted{color:var(--muted)}
.pill{display:inline-block;padding:2px 8px;border-radius:999px;background:#ffedd5;color:#9a3412;font-size:12px}
.error{color:var(--warn)}
pre{white-space:pre-wrap;word-break:break-word;background:#fff;border:1px solid var(--line);padding:12px;border-radius:12px}
ul{padding-left:18px}
@media (max-width: 900px){.shell{grid-template-columns:1fr}.panel{border-right:none;border-bottom:1px solid var(--line)}}
</style>
</head>
<body>
<div class="shell">
<aside class="panel">
  <h1>Token Tracking</h1>
  <p class="muted">Per-task token burn across retrieve, autoroute, and edit flows.</p>
  <div id="kpis" class="kpi"></div>
  <div id="tasks"></div>
</aside>
<main class="detail">
  <h2 id="detail-title">Select a task</h2>
  <div id="detail-body" class="muted">Task drilldown will appear here.</div>
</main>
</div>
<script>
async function loadDashboard(){
  const [tasksRes, hotspotsRes, trendsRes] = await Promise.all([
    fetch('/api/tasks').then(r=>r.json()),
    fetch('/api/hotspots').then(r=>r.json()),
    fetch('/api/trends').then(r=>r.json())
  ]);
  const tasks = tasksRes.tasks || [];
  const totalTokens = tasks.reduce((sum,t)=>sum+t.total_tokens,0);
  const successCount = tasks.filter(t=>t.success).length;
  document.getElementById('kpis').innerHTML = [
    card('Tasks', tasks.length),
    card('Tokens', totalTokens),
    card('Success', tasks.length ? Math.round(successCount*100/tasks.length)+'%' : 'n/a'),
    card('Wasted', hotspotsRes.wasted_tokens_estimated || 0)
  ].join('');
  document.getElementById('tasks').innerHTML = tasks.map(task => `
    <div class="task" onclick="showTask('${task.task_id}')">
      <strong>${task.route_id}</strong>
      <div class="muted">${task.task_id}</div>
      <div>${task.total_tokens} tokens <span class="pill">${task.top_category}</span></div>
      <div class="${task.success ? '' : 'error'}">${task.success ? 'completed' : 'failed'} · repeated errors ${task.repeated_errors}</div>
    </div>`).join('') || '<div class="muted">No events yet. Enable token tracking in <code>.semantic/token_tracking.toml</code> and run a task.</div>';
  if(tasks[0]) showTask(tasks[0].task_id);
}
function card(label, value){ return `<div class="card"><div class="muted">${label}</div><strong>${value}</strong></div>`; }
async function showTask(taskId){
  const res = await fetch('/api/tasks/'+encodeURIComponent(taskId)).then(r=>r.json());
  document.getElementById('detail-title').textContent = taskId;
  document.getElementById('detail-body').innerHTML = `
    <h3>Hints</h3>
    <ul>${(res.hints || []).map(h => `<li>${h}</li>`).join('') || '<li>No optimization hints yet.</li>'}</ul>
    <h3>Timeline</h3>
    ${(res.timeline || []).map(item => `
      <div class="card" style="margin-bottom:10px">
        <strong>${item.event_type}</strong>
        <div class="muted">${item.category || 'uncategorized'} · ${item.status || 'n/a'} · ${item.tokens || 0} tokens</div>
        ${item.error_code ? `<div class="error">${item.error_code}</div>` : ''}
        <pre>${JSON.stringify(item.metadata, null, 2)}</pre>
      </div>`).join('')}
  `;
}
loadDashboard();
</script>
</body>
</html>"#)
}

fn refresh_and_load_tasks(state: &AppState) -> Result<Vec<TaskSummary>> {
    let events = refresh_and_load_events(state, None)?;
    Ok(build_task_summaries(&events))
}

fn refresh_and_load_events(state: &AppState, task_id: Option<&str>) -> Result<Vec<EventRow>> {
    init_db(&state.db_path)?;
    ingest_events(&state.log_path, &state.db_path)?;
    let conn = Connection::open(&state.db_path)?;
    load_events(&conn, task_id)
}

fn init_db(db_path: &FsPath) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "create table if not exists meta (key text primary key, value text not null);
         create table if not exists events (
           event_id text primary key,
           timestamp integer not null,
           task_id text,
           route_id text,
           event_type text not null,
           category text,
           tool_name text,
           provider text,
           model text,
           total_tokens_reported integer,
           prompt_tokens_estimated integer,
           completion_tokens_estimated integer,
           latency_ms integer,
           status text,
           error_code text,
           metadata_json text not null
         );",
    )?;
    Ok(())
}

fn ingest_events(log_path: &FsPath, db_path: &FsPath) -> Result<()> {
    let conn = Connection::open(db_path)?;
    let offset = conn
        .query_row(
            "select value from meta where key = 'offset'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0);
    let Ok(bytes) = std::fs::read(log_path) else {
        return Ok(());
    };
    if offset >= bytes.len() {
        return Ok(());
    }
    let chunk = String::from_utf8_lossy(&bytes[offset..]).to_string();
    for line in chunk.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<TelemetryEvent>(line) else {
            continue;
        };
        conn.execute(
            "insert or ignore into events (
                event_id, timestamp, task_id, route_id, event_type, category, tool_name, provider, model,
                total_tokens_reported, prompt_tokens_estimated, completion_tokens_estimated, latency_ms,
                status, error_code, metadata_json
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                event.event_id,
                event.timestamp as i64,
                event.task_id,
                event.route_id,
                event.event_type,
                event.category,
                event.tool_name,
                event.provider,
                event.model,
                event.total_tokens_reported.map(|v| v as i64),
                event.prompt_tokens_estimated.map(|v| v as i64),
                event.completion_tokens_estimated.map(|v| v as i64),
                event.latency_ms.map(|v| v as i64),
                event.status,
                event.error_code,
                serde_json::to_string(&event.metadata)?,
            ],
        )?;
    }
    conn.execute(
        "insert into meta(key, value) values('offset', ?1)
         on conflict(key) do update set value = excluded.value",
        [bytes.len().to_string()],
    )?;
    Ok(())
}

fn load_events(conn: &Connection, task_id: Option<&str>) -> Result<Vec<EventRow>> {
    let mut sql = "select event_id, timestamp, task_id, route_id, event_type, category, tool_name, provider, model,
                   total_tokens_reported, prompt_tokens_estimated, completion_tokens_estimated, latency_ms, status,
                   error_code, metadata_json from events".to_string();
    if task_id.is_some() {
        sql.push_str(" where task_id = ?1");
    }
    sql.push_str(" order by timestamp asc");
    let mut stmt = conn.prepare(&sql)?;
    let mapped = if let Some(task_id) = task_id {
        stmt.query_map([task_id], row_to_event)?
    } else {
        stmt.query_map([], row_to_event)?
    };
    let mut events = Vec::new();
    for row in mapped {
        events.push(row?);
    }
    Ok(events)
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    let metadata_raw: String = row.get(15)?;
    Ok(EventRow {
        timestamp: row.get::<_, i64>(1)? as u64,
        task_id: row.get(2)?,
        route_id: row.get(3)?,
        event_type: row.get(4)?,
        category: row.get(5)?,
        tool_name: row.get(6)?,
        provider: row.get(7)?,
        model: row.get(8)?,
        total_tokens_reported: row.get::<_, Option<i64>>(9)?.map(|v| v as usize),
        prompt_tokens_estimated: row.get::<_, Option<i64>>(10)?.map(|v| v as usize),
        completion_tokens_estimated: row.get::<_, Option<i64>>(11)?.map(|v| v as usize),
        latency_ms: row.get::<_, Option<i64>>(12)?.map(|v| v as u128),
        status: row.get(13)?,
        error_code: row.get(14)?,
        metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| serde_json::json!({})),
    })
}

fn build_task_summaries(events: &[EventRow]) -> Vec<TaskSummary> {
    let mut grouped: HashMap<String, Vec<EventRow>> = HashMap::new();
    for event in events {
        if let Some(task_id) = &event.task_id {
            grouped.entry(task_id.clone()).or_default().push(event.clone());
        }
    }
    let mut tasks: Vec<_> = grouped
        .into_iter()
        .map(|(task_id, items)| {
            let started_at = items.first().map(|v| v.timestamp).unwrap_or_default();
            let ended_at = items.last().map(|v| v.timestamp).unwrap_or(started_at);
            let mut category_tokens: HashMap<String, usize> = HashMap::new();
            let mut total_tokens = 0usize;
            let mut wasted_calls = 0usize;
            let mut seen_errors = HashSet::new();
            let mut repeated_errors = 0usize;
            let mut success = false;
            let route_id = items
                .iter()
                .find_map(|event| event.route_id.clone())
                .unwrap_or_else(|| "unknown".to_string());
            for event in &items {
                let tokens = event_tokens(event);
                total_tokens += tokens;
                if let Some(category) = &event.category {
                    *category_tokens.entry(category.clone()).or_default() += tokens;
                }
                if event.status.as_deref() == Some("error") {
                    wasted_calls += tokens.max(event.prompt_tokens_estimated.unwrap_or_default());
                    if let Some(error) = &event.error_code {
                        if !seen_errors.insert(error.clone()) {
                            repeated_errors += 1;
                        }
                    }
                }
                if event.event_type == "task_completed" {
                    success = true;
                }
                if event.event_type == "task_failed" {
                    success = false;
                }
            }
            let top_category = category_tokens
                .iter()
                .max_by_key(|(_, tokens)| *tokens)
                .map(|(category, _)| category.clone())
                .unwrap_or_else(|| "uncategorized".to_string());
            TaskSummary {
                task_id,
                route_id,
                started_at,
                duration_ms: ended_at.saturating_sub(started_at),
                success,
                total_tokens,
                repeated_errors,
                wasted_calls,
                top_category,
                category_tokens: serde_json::json!(category_tokens),
            }
        })
        .collect();
    tasks.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    tasks
}

fn build_hints(events: &[EventRow]) -> Vec<String> {
    let mut hints = Vec::new();
    let mut category_tokens: HashMap<String, usize> = HashMap::new();
    let mut errors: HashMap<String, usize> = HashMap::new();
    for event in events {
        let tokens = event_tokens(event);
        if let Some(category) = &event.category {
            *category_tokens.entry(category.clone()).or_default() += tokens;
        }
        if let Some(error) = &event.error_code {
            *errors.entry(error.clone()).or_default() += 1;
        }
    }
    let retrieval_tokens = category_tokens.get("retrieval").copied().unwrap_or_default();
    let code_tokens = category_tokens.get("code_generation").copied().unwrap_or_default();
    if retrieval_tokens > code_tokens.saturating_mul(2) && retrieval_tokens > 0 {
        hints.push("Retrieval is consuming materially more tokens than code generation. Tighten context breadth or lower max tokens for lookup-heavy routes.".to_string());
    }
    if errors.values().any(|count| *count >= 2) {
        hints.push("The same error repeated in this task. Mark these calls as wasted and change strategy earlier on the next retry.".to_string());
    }
    if category_tokens.get("wasted_calls").copied().unwrap_or_default() > 0 {
        hints.push("Failed LLM calls consumed prompt budget. Review provider selection, retry policy, or model fallback order.".to_string());
    }
    if hints.is_empty() {
        hints.push("No obvious hotspot was detected from the current event stream.".to_string());
    }
    hints
}

fn event_tokens(event: &EventRow) -> usize {
    event
        .total_tokens_reported
        .or_else(|| {
            match (event.prompt_tokens_estimated, event.completion_tokens_estimated) {
                (Some(prompt), Some(completion)) => Some(prompt + completion),
                (Some(prompt), None) => Some(prompt),
                _ => None,
            }
        })
        .unwrap_or_default()
}

fn top_counts(map: HashMap<String, usize>) -> Vec<serde_json::Value> {
    let mut rows: Vec<_> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.into_iter()
        .take(10)
        .map(|(label, tokens)| serde_json::json!({"label": label, "tokens": tokens}))
        .collect()
}

fn day_label(timestamp_ms: u64) -> String {
    let day = timestamp_ms / 86_400_000;
    format!("day-{day}")
}
