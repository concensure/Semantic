use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const KG_DIR: &str = ".semantic/knowledge_graph";
const KG_LOG: &str = "knowledge.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub timestamp: u64,
    pub category: String,
    pub repository: String,
    pub title: String,
    pub details: String,
}

pub struct KnowledgeGraph {
    root: PathBuf,
}

impl KnowledgeGraph {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let root = repo_root.join(KG_DIR);
        fs::create_dir_all(&root)?;
        let log = root.join(KG_LOG);
        if !log.exists() {
            File::create(log)?;
        }
        Ok(Self { root })
    }

    pub fn append(&self, entry: &KnowledgeEntry) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(KG_LOG))?;
        writeln!(file, "{}", serde_json::to_string(entry)?)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<KnowledgeEntry>> {
        let file = File::open(self.root.join(KG_LOG))?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<KnowledgeEntry>(&line) {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{KnowledgeEntry, KnowledgeGraph};
    use tempfile::tempdir;

    #[test]
    fn appends_and_reads_entries() {
        let tmp = tempdir().expect("tempdir");
        let kg = KnowledgeGraph::open(tmp.path()).expect("open");
        kg.append(&KnowledgeEntry {
            timestamp: 1,
            category: "design_decision".to_string(),
            repository: "repo".to_string(),
            title: "service_layer pattern introduced".to_string(),
            details: "2026-02-20".to_string(),
        })
        .expect("append");
        let entries = kg.list().expect("list");
        assert_eq!(entries.len(), 1);
    }
}
