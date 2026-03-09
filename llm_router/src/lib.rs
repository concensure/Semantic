use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LLMTask {
    Planning,
    CodeExecution,
    InteractiveChat,
}

#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub provider: String,
    pub endpoint: String,
}

#[derive(Debug, Deserialize, Default)]
struct TaskPref {
    preferred: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RoutingConfig {
    planning: Option<TaskPref>,
    execution: Option<TaskPref>,
    interactive: Option<TaskPref>,
}

pub struct LLMRouter {
    providers: HashMap<String, String>,
    routing: RoutingConfig,
    metrics: HashMap<String, ModelMetric>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct ModelMetric {
    success_rate: Option<f64>,
    latency_ms: Option<f64>,
    token_cost: Option<f64>,
    failure_rate: Option<f64>,
}

fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut parts = trimmed.splitn(2, '=');
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

fn parse_quoted_string(value: &str) -> Option<String> {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        Some(v[1..v.len() - 1].to_string())
    } else {
        None
    }
}

fn parse_string_array(value: &str) -> Vec<String> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    let inner = &v[1..v.len() - 1];
    inner
        .split(',')
        .filter_map(parse_quoted_string)
        .collect()
}

fn parse_providers(content: &str) -> HashMap<String, String> {
    let mut providers = HashMap::new();
    let mut in_providers = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_providers = line.eq_ignore_ascii_case("[providers]");
            continue;
        }
        if !in_providers {
            continue;
        }
        if let Some((key, value)) = parse_assignment(line) {
            if let Some(url) = parse_quoted_string(value) {
                providers.insert(key.to_string(), url);
            }
        }
    }
    providers
}

fn parse_routing(content: &str) -> RoutingConfig {
    let mut routing = RoutingConfig::default();
    let mut section = String::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        if let Some((key, value)) = parse_assignment(line) {
            if key != "preferred" {
                continue;
            }
            let preferred = parse_string_array(value);
            let pref = TaskPref {
                preferred: Some(preferred),
            };
            match section.as_str() {
                "planning" => routing.planning = Some(pref),
                "execution" => routing.execution = Some(pref),
                "interactive" => routing.interactive = Some(pref),
                _ => {}
            }
        }
    }
    routing
}

impl LLMRouter {
    pub fn from_files(provider_toml: &str, routing_toml: &str, metrics_json: &str) -> Result<Self> {
        let providers = parse_providers(provider_toml);
        let routing = parse_routing(routing_toml);
        let metrics: HashMap<String, ModelMetric> = serde_json::from_str(metrics_json).unwrap_or_default();

        Ok(Self {
            providers,
            routing,
            metrics,
        })
    }

    pub fn route(&self, task: LLMTask) -> Option<RouteDecision> {
        let preferred = match task {
            LLMTask::Planning => self.routing.planning.as_ref().and_then(|p| p.preferred.clone()),
            LLMTask::CodeExecution => self.routing.execution.as_ref().and_then(|p| p.preferred.clone()),
            LLMTask::InteractiveChat => self.routing.interactive.as_ref().and_then(|p| p.preferred.clone()),
        }
        .unwrap_or_default();

        let mut ranked: Vec<(String, f64)> = preferred
            .into_iter()
            .filter_map(|p| {
                let metric = self.metrics.get(&p).cloned().unwrap_or_default();
                let success = metric
                    .success_rate
                    .unwrap_or_else(|| 1.0 - metric.failure_rate.unwrap_or(0.0));
                let latency = metric.latency_ms.unwrap_or(500.0);
                let cost = metric.token_cost.unwrap_or(1.0);
                let latency_score = 1.0 / (1.0 + (latency / 1000.0));
                let cost_score = 1.0 / (1.0 + cost);
                let score = (success * 0.6) + (latency_score * 0.2) + (cost_score * 0.2);
                self.providers.get(&p).map(|_| (p, score))
            })
            .collect();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (provider, _) = ranked.first()?.clone();
        Some(RouteDecision {
            endpoint: self.providers.get(&provider)?.clone(),
            provider,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{LLMRouter, LLMTask};

    #[test]
    fn routes_by_preference_and_metrics() {
        let providers = r#"
[providers]
openai = "https://api.openai.com/v1"
anthropic = "https://api.anthropic.com"
together = "https://api.together.xyz"
"#;
        let routing = r#"
[planning]
preferred = ["anthropic", "openai"]

[execution]
preferred = ["openai", "together"]

[interactive]
preferred = ["openai"]
"#;
        let metrics = r#"{
  "openai": { "latency_ms": 120, "token_cost": 0.5, "failure_rate": 0.01 },
  "anthropic": { "latency_ms": 200, "token_cost": 0.3, "failure_rate": 0.005 },
  "together": { "latency_ms": 80, "token_cost": 0.8, "failure_rate": 0.02 }
}"#;

        let router = LLMRouter::from_files(providers, routing, metrics).expect("router");
        let planning = router.route(LLMTask::Planning).expect("planning route");
        let execution = router
            .route(LLMTask::CodeExecution)
            .expect("execution route");
        let interactive = router
            .route(LLMTask::InteractiveChat)
            .expect("interactive route");

        assert_eq!(planning.provider, "anthropic");
        assert_eq!(execution.provider, "openai");
        assert_eq!(interactive.provider, "openai");
    }
}
