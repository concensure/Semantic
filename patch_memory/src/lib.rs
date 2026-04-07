use anyhow::Result;
use engine::{EditRiskScore, EditType, ModelPerformance, PatchRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PATCH_DIR: &str = ".semantic/patch_memory";
const PATCH_LOG_FILE: &str = "patch_log.jsonl";
const PATCH_STATS_FILE: &str = "patch_stats.json";
const MODEL_PERF_FILE: &str = "model_performance.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchQuery {
    pub repository: Option<String>,
    pub symbol: Option<String>,
    pub model: Option<String>,
    pub time_range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PatchStats {
    pub total_patches: usize,
    pub success_rate: f32,
    pub rollback_frequency: f32,
    pub model_success_rate: HashMap<String, f32>,
    pub edit_type_success_rate: HashMap<String, f32>,
}

pub struct PatchMemory {
    root: PathBuf,
}

impl PatchMemory {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let root = repo_root.join(PATCH_DIR);
        fs::create_dir_all(&root)?;
        let log = root.join(PATCH_LOG_FILE);
        let stats = root.join(PATCH_STATS_FILE);
        let perf = root.join(MODEL_PERF_FILE);
        if !log.exists() {
            File::create(&log)?;
        }
        if !stats.exists() {
            fs::write(&stats, "{}")?;
        }
        if !perf.exists() {
            fs::write(&perf, "[]")?;
        }
        Ok(Self { root })
    }

    pub fn new_record_id() -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        format!("p_{ts}")
    }

    pub fn append_record(&self, record: &PatchRecord) -> Result<()> {
        let line = serde_json::to_string(record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(PATCH_LOG_FILE))?;
        writeln!(file, "{line}")?;
        self.recompute_aggregates()?;
        Ok(())
    }

    pub fn list_records(&self, filter: &PatchQuery) -> Result<Vec<PatchRecord>> {
        let file = File::open(self.root.join(PATCH_LOG_FILE))?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: PatchRecord = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if !matches_filter(&record, filter) {
                continue;
            }
            out.push(record);
        }
        out.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.patch_id.cmp(&b.patch_id))
        });
        Ok(out)
    }

    pub fn stats(&self, filter: &PatchQuery) -> Result<PatchStats> {
        let records = self.list_records(filter)?;
        Ok(build_stats(&records))
    }

    pub fn model_performance(&self, filter: &PatchQuery) -> Result<Vec<ModelPerformance>> {
        let records = self.list_records(filter)?;
        Ok(build_model_performance(&records))
    }

    pub fn edit_risk_score(&self, edit_type: EditType) -> Result<EditRiskScore> {
        let records = self.list_records(&PatchQuery::default())?;
        let mut total = 0usize;
        let mut failures = 0usize;
        for r in &records {
            if r.edit_type != edit_type {
                continue;
            }
            total += 1;
            if !is_success(r) {
                failures += 1;
            }
        }
        let risk = if total == 0 {
            0.0
        } else {
            failures as f32 / total as f32
        };
        Ok(EditRiskScore { edit_type, risk })
    }

    pub fn export_history_graph(&self) -> Result<serde_json::Value> {
        let records = self.list_records(&PatchQuery::default())?;
        let timeline: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "timestamp": r.timestamp,
                    "symbol": r.target_symbol,
                    "model": r.model_used,
                    "success": is_success(r),
                })
            })
            .collect();
        let models = build_model_performance(&records);
        let failure_clusters: Vec<serde_json::Value> = records
            .iter()
            .filter(|r| !is_success(r))
            .map(|r| {
                serde_json::json!({
                    "patch_id": r.patch_id,
                    "symbol": r.target_symbol,
                    "edit_type": format!("{:?}", r.edit_type),
                    "rollback_reason": r.rollback_reason,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "timeline": timeline,
            "model_trends": models,
            "failure_clusters": failure_clusters,
        }))
    }

    fn recompute_aggregates(&self) -> Result<()> {
        let records = self.list_records(&PatchQuery::default())?;
        let stats = build_stats(&records);
        let models = build_model_performance(&records);
        fs::write(
            self.root.join(PATCH_STATS_FILE),
            serde_json::to_string_pretty(&stats)?,
        )?;
        fs::write(
            self.root.join(MODEL_PERF_FILE),
            serde_json::to_string_pretty(&models)?,
        )?;
        Ok(())
    }
}

fn matches_filter(record: &PatchRecord, filter: &PatchQuery) -> bool {
    if let Some(repo) = &filter.repository {
        if &record.repository != repo {
            return false;
        }
    }
    if let Some(symbol) = &filter.symbol {
        if &record.target_symbol != symbol {
            return false;
        }
    }
    if let Some(model) = &filter.model {
        if &record.model_used != model {
            return false;
        }
    }
    if let Some((from, to)) = filter.time_range {
        if record.timestamp < from || record.timestamp > to {
            return false;
        }
    }
    true
}

fn is_success(record: &PatchRecord) -> bool {
    record.approved_by_user
        && record.validation_passed
        && record.tests_passed
        && !record.rollback_occurred
}

fn build_stats(records: &[PatchRecord]) -> PatchStats {
    if records.is_empty() {
        return PatchStats::default();
    }

    let mut success = 0usize;
    let mut rollback = 0usize;
    let mut model_totals: HashMap<String, (usize, usize)> = HashMap::new();
    let mut edit_totals: HashMap<String, (usize, usize)> = HashMap::new();

    for r in records {
        let ok = is_success(r);
        if ok {
            success += 1;
        }
        if r.rollback_occurred {
            rollback += 1;
        }

        let m = model_totals.entry(r.model_used.clone()).or_insert((0, 0));
        m.0 += 1;
        if ok {
            m.1 += 1;
        }

        let key = format!("{:?}", r.edit_type);
        let e = edit_totals.entry(key).or_insert((0, 0));
        e.0 += 1;
        if ok {
            e.1 += 1;
        }
    }

    let model_success_rate = model_totals
        .into_iter()
        .map(|(k, (t, s))| (k, s as f32 / t as f32))
        .collect();
    let edit_type_success_rate = edit_totals
        .into_iter()
        .map(|(k, (t, s))| (k, s as f32 / t as f32))
        .collect();

    PatchStats {
        total_patches: records.len(),
        success_rate: success as f32 / records.len() as f32,
        rollback_frequency: rollback as f32 / records.len() as f32,
        model_success_rate,
        edit_type_success_rate,
    }
}

fn build_model_performance(records: &[PatchRecord]) -> Vec<ModelPerformance> {
    let mut grouped: HashMap<String, (usize, usize)> = HashMap::new();
    for r in records {
        let entry = grouped.entry(r.model_used.clone()).or_insert((0, 0));
        entry.0 += 1;
        if is_success(r) {
            entry.1 += 1;
        }
    }

    let mut out: Vec<ModelPerformance> = grouped
        .into_iter()
        .map(|(model, (total, success))| ModelPerformance {
            model,
            success_rate: if total == 0 {
                0.0
            } else {
                success as f32 / total as f32
            },
            avg_latency_ms: 0,
            avg_cost: 0.0,
        })
        .collect();
    out.sort_by(|a, b| a.model.cmp(&b.model));
    out
}

#[cfg(test)]
mod tests {
    use super::{PatchMemory, PatchQuery};
    use engine::{ASTTransformation, EditType, PatchRecord};
    use tempfile::tempdir;

    #[test]
    fn appends_and_filters_records() {
        let tmp = tempdir().expect("tempdir");
        let mem = PatchMemory::open(tmp.path()).expect("open");

        let record = PatchRecord {
            patch_id: "p_1".to_string(),
            timestamp: 1,
            repository: "repoA".to_string(),
            file_path: "src/a.ts".to_string(),
            target_symbol: "retryRequest".to_string(),
            edit_type: EditType::ModifyLogic,
            model_used: "openai".to_string(),
            provider: "openai".to_string(),
            diff: "---".to_string(),
            ast_transform: Some(ASTTransformation::ReplaceFunctionBody),
            impacted_symbols: vec!["fetchData".to_string()],
            approved_by_user: true,
            validation_passed: true,
            tests_passed: true,
            rollback_occurred: false,
            rollback_reason: None,
        };
        mem.append_record(&record).expect("append");

        let all = mem.list_records(&PatchQuery::default()).expect("all");
        assert_eq!(all.len(), 1);

        let filtered = mem
            .list_records(&PatchQuery {
                repository: Some("repoA".to_string()),
                symbol: Some("retryRequest".to_string()),
                model: Some("openai".to_string()),
                time_range: Some((0, 10)),
            })
            .expect("filter");
        assert_eq!(filtered.len(), 1);
    }
}
