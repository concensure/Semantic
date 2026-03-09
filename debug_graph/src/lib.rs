use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

const DEBUG_STATE_FILE: &str = ".semantic/debug_graph_state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureEvent {
    pub event_id: String,
    pub repository: String,
    pub timestamp: u64,
    pub failure_type: FailureType,
    pub stack_trace: Vec<String>,
    pub error_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FailureType {
    TestFailure,
    RuntimeException,
    BuildFailure,
    IntegrationFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugNode {
    pub symbol: String,
    pub file_path: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugGraph {
    pub nodes: Vec<DebugNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootCauseCandidate {
    pub symbol: String,
    pub probability: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSuggestion {
    pub suggested_fix: String,
    pub patch: String,
    pub llm_provider: Option<String>,
    pub llm_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugAnalysisState {
    pub last_failure: Option<FailureEvent>,
    pub debug_graph: DebugGraph,
    pub candidates: Vec<RootCauseCandidate>,
    pub patch_suggestion: Option<PatchSuggestion>,
}

pub struct DebugGraphEngine;

impl DebugGraphEngine {
    pub fn analyze_failure(
        repo_root: &Path,
        storage: &storage::Storage,
        event: FailureEvent,
    ) -> Result<DebugAnalysisState> {
        let stack_symbols = extract_symbols_from_stack_trace(&event.stack_trace);
        let mut graph = build_debug_graph(storage, &stack_symbols)?;
        graph.nodes.sort_by(|a, b| a.symbol.cmp(&b.symbol));

        let candidates = rank_root_causes(repo_root, storage, &event, &graph, &stack_symbols)?;
        let patch_suggestion = suggest_patch(&event, candidates.first(), &graph);

        let state = DebugAnalysisState {
            last_failure: Some(event),
            debug_graph: graph,
            candidates,
            patch_suggestion,
        };
        Self::write_state(repo_root, &state)?;
        Ok(state)
    }

    pub fn read_state(repo_root: &Path) -> Result<DebugAnalysisState> {
        let path = repo_root.join(DEBUG_STATE_FILE);
        if !path.exists() {
            return Ok(DebugAnalysisState::default());
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    pub fn write_state(repo_root: &Path, state: &DebugAnalysisState) -> Result<()> {
        let path = repo_root.join(DEBUG_STATE_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(state)?)?;
        Ok(())
    }
}

fn extract_symbols_from_stack_trace(stack_trace: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for frame in stack_trace {
        let cleaned = frame
            .replace("(", " ")
            .replace(")", " ")
            .replace("::", " ");
        for token in cleaned.split_whitespace() {
            let candidate = token
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            if candidate.len() < 3 {
                continue;
            }
            if candidate.contains('/') || candidate.contains('\\') {
                continue;
            }
            if candidate.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            out.push(candidate);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn build_debug_graph(storage: &storage::Storage, stack_symbols: &[String]) -> Result<DebugGraph> {
    let mut seed_ids = Vec::new();
    let mut nodes = HashMap::<i64, DebugNode>::new();
    for name in stack_symbols {
        if let Some(sym) = storage.get_symbol_any(name)? {
            let sid = sym.id.unwrap_or_default();
            seed_ids.push(sid);
            let deps = storage
                .get_symbol_dependencies(sid)?
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>();
            nodes.insert(
                sid,
                DebugNode {
                    symbol: sym.name,
                    file_path: sym.file,
                    dependencies: deps,
                },
            );
        }
    }

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    for sid in seed_ids {
        queue.push_back((sid, 0usize));
        visited.insert(sid);
    }

    while let Some((symbol_id, depth)) = queue.pop_front() {
        if depth >= 2 {
            continue;
        }
        let mut neighbors = storage.get_symbol_dependencies(symbol_id)?;
        neighbors.extend(storage.get_symbol_callers(symbol_id)?);
        for neighbor in neighbors {
            let nid = neighbor.id.unwrap_or_default();
            if visited.insert(nid) {
                queue.push_back((nid, depth + 1));
            }
            let deps = storage
                .get_symbol_dependencies(nid)?
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>();
            nodes.entry(nid).or_insert(DebugNode {
                symbol: neighbor.name,
                file_path: neighbor.file,
                dependencies: deps,
            });
        }
    }

    Ok(DebugGraph {
        nodes: nodes.into_values().collect(),
    })
}

fn rank_root_causes(
    repo_root: &Path,
    _storage: &storage::Storage,
    event: &FailureEvent,
    graph: &DebugGraph,
    stack_symbols: &[String],
) -> Result<Vec<RootCauseCandidate>> {
    let patch_memory = patch_memory::PatchMemory::open(repo_root)?;
    let patch_records = patch_memory.list_records(&patch_memory::PatchQuery::default())?;
    let mut by_symbol_count = HashMap::<String, usize>::new();
    let mut by_symbol_recent = HashMap::<String, u64>::new();
    for rec in patch_records {
        *by_symbol_count.entry(rec.target_symbol.clone()).or_insert(0) += 1;
        let entry = by_symbol_recent.entry(rec.target_symbol).or_insert(0);
        *entry = (*entry).max(rec.timestamp);
    }

    let mut candidates = Vec::new();
    for node in &graph.nodes {
        let proximity = if stack_symbols.iter().any(|s| s == &node.symbol) {
            1.0
        } else if stack_symbols
            .iter()
            .any(|s| node.dependencies.iter().any(|d| d == s))
        {
            0.7
        } else {
            0.35
        };

        let recency = by_symbol_recent
            .get(&node.symbol)
            .map(|ts| {
                if event.timestamp > *ts {
                    let delta = event.timestamp - *ts;
                    1.0 / (1.0 + (delta as f32 / 86400.0))
                } else {
                    1.0
                }
            })
            .unwrap_or(0.2);

        let change_freq = by_symbol_count
            .get(&node.symbol)
            .map(|v| (*v as f32 / 10.0).min(1.0))
            .unwrap_or(0.1);

        let depth = if node.dependencies.is_empty() {
            1.0
        } else {
            (1.0 / (1.0 + node.dependencies.len() as f32)).max(0.1)
        };

        let score = (proximity * 0.45) + (recency * 0.2) + (change_freq * 0.2) + (depth * 0.15);
        candidates.push(RootCauseCandidate {
            symbol: node.symbol.clone(),
            probability: score,
        });
    }

    candidates.sort_by(|a, b| {
        b.probability
            .partial_cmp(&a.probability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(max) = candidates.first().map(|c| c.probability) {
        if max > 0.0 {
            for c in &mut candidates {
                c.probability /= max;
            }
        }
    }
    Ok(candidates)
}

fn suggest_patch(
    event: &FailureEvent,
    candidate: Option<&RootCauseCandidate>,
    graph: &DebugGraph,
) -> Option<PatchSuggestion> {
    let candidate = candidate?;
    let node = graph.nodes.iter().find(|n| n.symbol == candidate.symbol)?;

    let provider_toml = "[providers]\nopenai = \"https://api.openai.com/v1\"\n";
    let routing_toml = "[planning]\npreferred = [\"openai\"]\n";
    let metrics_json = "{\"openai\":{\"success_rate\":0.9,\"latency_ms\":200,\"token_cost\":0.2}}";
    let route = llm_router::LLMRouter::from_files(provider_toml, routing_toml, metrics_json)
        .ok()
        .and_then(|r| r.route(llm_router::LLMTask::Planning));

    let suggested_fix = format!(
        "Investigate '{}' in '{}' and add input guards + explicit error handling for '{}'.",
        node.symbol, node.file_path, event.error_message
    );
    let patch = format!(
        "--- a/{0}\n+++ b/{0}\n@@\n+// TODO(debug): harden {1} against failure: {2}\n",
        node.file_path, node.symbol, event.error_message
    );
    Some(PatchSuggestion {
        suggested_fix,
        patch,
        llm_provider: route.as_ref().map(|r| r.provider.clone()),
        llm_endpoint: route.as_ref().map(|r| r.endpoint.clone()),
    })
}
