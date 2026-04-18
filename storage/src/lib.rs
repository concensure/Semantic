use anyhow::Result;
use engine::{
    DependencyRecord, FlowEdgeKind, FlowEdgeRecord, LogicClusterRecord, LogicEdgeRecord,
    LogicNodeRecord, LogicNodeType, ModuleDependency, ModuleFile, ModuleRecord, RepoDependency,
    RepositoryRecord, RustImportRecord, RustIndexedSymbolRecord, RustModuleDeclRecord,
    RustSymbolMetadataRecord, SymbolRecord, SymbolType,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index};

pub const SCHEMA_SQL: &str = include_str!("../sql/schema.sql");

fn now_timestamp_string() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

pub struct TantivySymbolIndex {
    index: Index,
    name: Field,
    file: Field,
    language: Field,
    symbol_type: Field,
}

impl TantivySymbolIndex {
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let mut schema_builder = tantivy::schema::Schema::builder();
        let name = schema_builder.add_text_field("name", TEXT | STORED);
        let file = schema_builder.add_text_field("file", STRING | STORED);
        let language = schema_builder.add_text_field("language", STRING | STORED);
        let symbol_type = schema_builder.add_text_field("type", STRING | STORED);
        let schema = schema_builder.build();

        let index = match std::fs::create_dir_all(path) {
            Ok(_) => match Index::open_in_dir(path) {
                Ok(idx) => idx,
                Err(_) => match Index::create_in_dir(path, schema.clone()) {
                    Ok(idx) => idx,
                    Err(_) => Index::create_in_ram(schema.clone()),
                },
            },
            Err(_) => Index::create_in_ram(schema.clone()),
        };

        Ok(Self {
            index,
            name,
            file,
            language,
            symbol_type,
        })
    }

    pub fn rebuild(&self, symbols: &[SymbolRecord]) -> Result<()> {
        let mut writer = match self.index.writer(30_000_000) {
            Ok(w) => w,
            Err(_) => return Ok(()),
        };
        if writer.delete_all_documents().is_err() {
            return Ok(());
        }
        for sym in symbols {
            if writer
                .add_document(doc!(
                    self.name => sym.name.clone(),
                    self.file => sym.file.clone(),
                    self.language => sym.language.clone(),
                    self.symbol_type => symbol_type_to_str(&sym.symbol_type).to_string(),
                ))
                .is_err()
            {
                return Ok(());
            }
        }
        if writer.commit().is_err() {
            return Ok(());
        }
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
        let reader = match self.index.reader() {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.name]);
        let q = match parser.parse_query(query) {
            Ok(q) => q,
            Err(_) => return Ok(Vec::new()),
        };
        let top_docs = match searcher.search(&q, &TopDocs::with_limit(limit)) {
            Ok(docs) => docs,
            Err(_) => return Ok(Vec::new()),
        };

        let mut out = Vec::new();
        for (_, doc_addr) in top_docs {
            let retrieved: tantivy::TantivyDocument = searcher.doc(doc_addr)?;
            let name = retrieved
                .get_first(self.name)
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let file = retrieved
                .get_first(self.file)
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let language = retrieved
                .get_first(self.language)
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            out.push((name, file, language));
        }

        Ok(out)
    }
}

pub struct Storage {
    conn: Connection,
    pub symbol_index: TantivySymbolIndex,
}

#[derive(Debug, Clone)]
pub struct RetrievalCacheEntry {
    pub cache_key: String,
    pub cache_kind: String,
    pub value_json: Option<String>,
    pub prompt_text: Option<String>,
    pub cached_at_epoch_s: u64,
    pub source_revision: u64,
}

impl Storage {
    pub fn open(db_path: &Path, tantivy_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        ensure_runtime_migrations(&conn)?;
        let symbol_index = TantivySymbolIndex::open_or_create(tantivy_path)?;
        Ok(Self { conn, symbol_index })
    }

    pub fn upsert_file(&self, path: &str, language: &str, checksum: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files(path, language, checksum, indexed_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                language=excluded.language,
                checksum=excluded.checksum,
                indexed_at=excluded.indexed_at",
            params![path, language, checksum, now_timestamp_string()],
        )?;
        Ok(())
    }

    pub fn upsert_retrieval_cache_entry(&self, entry: &RetrievalCacheEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO retrieval_cache(cache_key, cache_kind, value_json, prompt_text, cached_at_epoch_s, source_revision)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(cache_key) DO UPDATE SET
                cache_kind=excluded.cache_kind,
                value_json=excluded.value_json,
                prompt_text=excluded.prompt_text,
                cached_at_epoch_s=excluded.cached_at_epoch_s,
                source_revision=excluded.source_revision",
            params![
                entry.cache_key,
                entry.cache_kind,
                entry.value_json,
                entry.prompt_text,
                entry.cached_at_epoch_s as i64,
                entry.source_revision as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get_retrieval_cache_entry(
        &self,
        cache_key: &str,
        cache_kind: &str,
    ) -> Result<Option<RetrievalCacheEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT cache_key, cache_kind, value_json, prompt_text, cached_at_epoch_s, source_revision
             FROM retrieval_cache
             WHERE cache_key = ?1 AND cache_kind = ?2
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![cache_key, cache_kind])?;
        if let Some(row) = rows.next()? {
            Ok(Some(RetrievalCacheEntry {
                cache_key: row.get(0)?,
                cache_kind: row.get(1)?,
                value_json: row.get(2)?,
                prompt_text: row.get(3)?,
                cached_at_epoch_s: row.get::<_, i64>(4)? as u64,
                source_revision: row.get::<_, i64>(5)? as u64,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn delete_retrieval_cache_entry(&self, cache_key: &str, cache_kind: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM retrieval_cache WHERE cache_key = ?1 AND cache_kind = ?2",
            params![cache_key, cache_kind],
        )?;
        Ok(())
    }

    pub fn clear_retrieval_cache(&self) -> Result<()> {
        self.conn.execute("DELETE FROM retrieval_cache", [])?;
        Ok(())
    }

    pub fn count_retrieval_cache_entries(&self, cache_kind: &str) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM retrieval_cache WHERE cache_kind = ?1")?;
        let count = stmt.query_row(params![cache_kind], |row| row.get::<_, i64>(0))?;
        Ok(count.max(0) as usize)
    }

    pub fn prune_retrieval_cache_kind(
        &self,
        cache_kind: &str,
        max_entries: usize,
    ) -> Result<usize> {
        let count = self.count_retrieval_cache_entries(cache_kind)?;
        if count <= max_entries {
            return Ok(0);
        }
        let remove_count = count - max_entries;
        self.conn.execute(
            "DELETE FROM retrieval_cache
             WHERE cache_key IN (
               SELECT cache_key FROM retrieval_cache
               WHERE cache_kind = ?1
               ORDER BY cached_at_epoch_s ASC
               LIMIT ?2
             )",
            params![cache_kind, remove_count as i64],
        )?;
        Ok(remove_count)
    }

    pub fn replace_file_index(
        &mut self,
        repo_id: i64,
        file: &str,
        language: &str,
        checksum: &str,
        symbols: &[SymbolRecord],
        deps: &[DependencyRecord],
        logic_nodes: &[LogicNodeRecord],
        control_flow_edges: &[FlowEdgeRecord],
        data_flow_edges: &[FlowEdgeRecord],
        logic_clusters: &[LogicClusterRecord],
    ) -> Result<()> {
        self.replace_file_index_with_rust_metadata(
            repo_id,
            file,
            language,
            checksum,
            symbols,
            deps,
            logic_nodes,
            control_flow_edges,
            data_flow_edges,
            logic_clusters,
            &[],
            &[],
            &[],
        )
    }

    pub fn replace_file_index_with_rust_metadata(
        &mut self,
        repo_id: i64,
        file: &str,
        language: &str,
        checksum: &str,
        symbols: &[SymbolRecord],
        deps: &[DependencyRecord],
        logic_nodes: &[LogicNodeRecord],
        control_flow_edges: &[FlowEdgeRecord],
        data_flow_edges: &[FlowEdgeRecord],
        logic_clusters: &[LogicClusterRecord],
        rust_metadata: &[RustSymbolMetadataRecord],
        rust_imports: &[RustImportRecord],
        rust_module_decls: &[RustModuleDeclRecord],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        tx.execute(
            "INSERT INTO files(path, language, checksum, indexed_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                language=excluded.language,
                checksum=excluded.checksum,
                indexed_at=excluded.indexed_at",
            params![file, language, checksum, now_timestamp_string()],
        )?;

        tx.execute(
            "DELETE FROM control_flow_edges
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        tx.execute(
            "DELETE FROM data_flow_edges
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        tx.execute(
            "DELETE FROM logic_clusters
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        tx.execute(
            "DELETE FROM logic_edges
             WHERE from_node_id IN (
                 SELECT ln.id FROM logic_nodes ln
                 JOIN symbols s ON s.id = ln.symbol_id
                 WHERE s.file = ?1
             )
             OR to_node_id IN (
                 SELECT ln.id FROM logic_nodes ln
                 JOIN symbols s ON s.id = ln.symbol_id
                 WHERE s.file = ?1
             )",
            params![file],
        )?;
        tx.execute(
            "DELETE FROM logic_nodes
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        tx.execute(
            "DELETE FROM rust_symbol_metadata
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        tx.execute("DELETE FROM rust_imports WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM rust_module_decls WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM symbols WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM dependencies WHERE file = ?1", params![file])?;

        let mut inserted_symbol_ids = Vec::with_capacity(symbols.len());
        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols(repo_id, name, type, file, start_line, end_line, language, summary, signature)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;

            for s in symbols {
                stmt.execute(params![
                    repo_id,
                    s.name,
                    symbol_type_to_str(&s.symbol_type),
                    s.file,
                    s.start_line,
                    s.end_line,
                    s.language,
                    s.summary,
                    s.signature,
                ])?;
                inserted_symbol_ids.push(tx.last_insert_rowid());
            }
        }

          {
              let mut stmt = tx.prepare(
                "INSERT INTO dependencies(repo_id, caller_symbol, callee_symbol, file, callee_file)
                  VALUES(?1, ?2, ?3, ?4, ?5)",
              )?;
              for d in deps {
                  stmt.execute(params![
                      repo_id,
                      d.caller_symbol,
                      d.callee_symbol,
                      d.file,
                      d.callee_file
                  ])?;
              }
          }

        let mut inserted_logic_nodes = Vec::new();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO logic_nodes(symbol_id, node_type, start_line, end_line, semantic_label)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
            )?;

            let mut sorted_nodes = logic_nodes.to_vec();
            sorted_nodes.sort_by_key(|n| (n.symbol_id, n.start_line, n.end_line));

            for node in sorted_nodes {
                let symbol_idx = (node.symbol_id - 1).max(0) as usize;
                if let Some(real_symbol_id) = inserted_symbol_ids.get(symbol_idx) {
                    stmt.execute(params![
                        real_symbol_id,
                        logic_node_type_to_str(&node.node_type),
                        node.start_line as i64,
                        node.end_line as i64,
                        node.semantic_label,
                    ])?;
                    inserted_logic_nodes.push(LogicNodeRecord {
                        id: Some(tx.last_insert_rowid()),
                        symbol_id: *real_symbol_id,
                        node_type: node.node_type,
                        start_line: node.start_line,
                        end_line: node.end_line,
                        semantic_label: node.semantic_label,
                    });
                }
            }
        }

        let mut per_symbol: HashMap<i64, Vec<&LogicNodeRecord>> = HashMap::new();
        for node in &inserted_logic_nodes {
            per_symbol.entry(node.symbol_id).or_default().push(node);
        }

        {
            let mut edge_stmt = tx.prepare(
                "INSERT INTO logic_edges(from_node_id, to_node_id)
                 VALUES(?1, ?2)",
            )?;
            for nodes in per_symbol.values_mut() {
                nodes.sort_by_key(|n| (n.start_line, n.end_line, n.id.unwrap_or_default()));
                for pair in nodes.windows(2) {
                    let from_id = pair[0].id.unwrap_or_default();
                    let to_id = pair[1].id.unwrap_or_default();
                    edge_stmt.execute(params![from_id, to_id])?;
                }
            }
        }

        insert_flow_edges_tx(
            &tx,
            "control_flow_edges",
            control_flow_edges,
            &inserted_logic_nodes,
        )?;
        insert_flow_edges_tx(
            &tx,
            "data_flow_edges",
            data_flow_edges,
            &inserted_logic_nodes,
        )?;
        insert_logic_clusters_tx(&tx, logic_clusters, &inserted_symbol_ids)?;
        insert_rust_metadata_tx(&tx, symbols, &inserted_symbol_ids, rust_metadata)?;
        insert_rust_imports_tx(&tx, rust_imports)?;
        insert_rust_module_decls_tx(&tx, rust_module_decls)?;

        tx.commit()?;
        Ok(())
    }

    pub fn delete_file_records(&self, file: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM control_flow_edges
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        self.conn.execute(
            "DELETE FROM data_flow_edges
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        self.conn.execute(
            "DELETE FROM logic_clusters
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        self.conn.execute(
            "DELETE FROM logic_edges
             WHERE from_node_id IN (
                 SELECT ln.id FROM logic_nodes ln
                 JOIN symbols s ON s.id = ln.symbol_id
                 WHERE s.file = ?1
             )
             OR to_node_id IN (
                 SELECT ln.id FROM logic_nodes ln
                 JOIN symbols s ON s.id = ln.symbol_id
                 WHERE s.file = ?1
             )",
            params![file],
        )?;
        self.conn.execute(
            "DELETE FROM logic_nodes
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        self.conn
            .execute("DELETE FROM rust_imports WHERE file = ?1", params![file])?;
        self.conn.execute(
            "DELETE FROM rust_module_decls WHERE file = ?1",
            params![file],
        )?;
        self.conn.execute(
            "DELETE FROM rust_symbol_metadata
             WHERE symbol_id IN (SELECT id FROM symbols WHERE file = ?1)",
            params![file],
        )?;
        self.conn
            .execute("DELETE FROM symbols WHERE file = ?1", params![file])?;
        self.conn
            .execute("DELETE FROM dependencies WHERE file = ?1", params![file])?;
        Ok(())
    }

    pub fn insert_symbols(&mut self, symbols: &[SymbolRecord]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let mut inserted = Vec::with_capacity(symbols.len());
        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols(repo_id, name, type, file, start_line, end_line, language, summary, signature)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for s in symbols {
                stmt.execute(params![
                    s.repo_id,
                    s.name,
                    symbol_type_to_str(&s.symbol_type),
                    s.file,
                    s.start_line,
                    s.end_line,
                    s.language,
                    s.summary,
                    s.signature,
                ])?;
                inserted.push(tx.last_insert_rowid());
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn insert_dependencies(&self, deps: &[DependencyRecord]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO dependencies(repo_id, caller_symbol, callee_symbol, file, callee_file)
              VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for d in deps {
            stmt.execute(params![
                d.repo_id,
                d.caller_symbol,
                d.callee_symbol,
                d.file,
                d.callee_file
            ])?;
        }
        Ok(())
    }

    pub fn insert_logic_nodes(
        &self,
        symbol_id: i64,
        nodes: &[LogicNodeRecord],
    ) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO logic_nodes(symbol_id, node_type, start_line, end_line, semantic_label)
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut ids = Vec::with_capacity(nodes.len());
        for node in nodes {
            stmt.execute(params![
                symbol_id,
                logic_node_type_to_str(&node.node_type),
                node.start_line as i64,
                node.end_line as i64,
                node.semantic_label,
            ])?;
            ids.push(self.conn.last_insert_rowid());
        }
        Ok(ids)
    }

    pub fn insert_logic_edges(&self, edges: &[LogicEdgeRecord]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO logic_edges(from_node_id, to_node_id)
             VALUES(?1, ?2)",
        )?;
        for edge in edges {
            stmt.execute(params![edge.from_node_id, edge.to_node_id])?;
        }
        Ok(())
    }

    pub fn list_symbols(&self) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature FROM symbols",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_rust_indexed_symbols(&self) -> Result<Vec<RustIndexedSymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.name, s.type, s.file, s.start_line, s.end_line, s.summary, s.signature,
                    r.kind, r.owner_name, r.trait_name, r.module_path, r.crate_name, r.crate_root
             FROM rust_symbol_metadata r
             JOIN symbols s ON s.id = r.symbol_id
             ORDER BY s.file, s.start_line, s.end_line, s.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RustIndexedSymbolRecord {
                symbol_id: row.get(0)?,
                name: row.get(1)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(2)?),
                file: row.get(3)?,
                start_line: row.get(4)?,
                end_line: row.get(5)?,
                summary: row.get(6)?,
                signature: row.get(7)?,
                kind: row.get(8)?,
                owner_name: row.get(9)?,
                trait_name: row.get(10)?,
                module_path: row.get(11)?,
                crate_name: row.get(12)?,
                crate_root: row.get(13)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn search_rust_indexed_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RustIndexedSymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.name, s.type, s.file, s.start_line, s.end_line, s.summary, s.signature,
                    r.kind, r.owner_name, r.trait_name, r.module_path, r.crate_name, r.crate_root
             FROM rust_symbol_metadata r
             JOIN symbols s ON s.id = r.symbol_id
             WHERE s.language = 'rust'
               AND (
                    s.name = ?1 COLLATE NOCASE
                    OR s.name LIKE ?2
                    OR r.owner_name = ?1 COLLATE NOCASE
                    OR r.trait_name = ?1 COLLATE NOCASE
                    OR r.module_path LIKE ?2
                    OR r.crate_name = ?1 COLLATE NOCASE
               )
             ORDER BY s.file, s.start_line, s.end_line, s.id
             LIMIT ?3",
        )?;
        let pattern = format!("%{query}%");
        let rows = stmt.query_map(params![query, pattern, limit as i64], |row| {
            Ok(RustIndexedSymbolRecord {
                symbol_id: row.get(0)?,
                name: row.get(1)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(2)?),
                file: row.get(3)?,
                start_line: row.get(4)?,
                end_line: row.get(5)?,
                summary: row.get(6)?,
                signature: row.get(7)?,
                kind: row.get(8)?,
                owner_name: row.get(9)?,
                trait_name: row.get(10)?,
                module_path: row.get(11)?,
                crate_name: row.get(12)?,
                crate_root: row.get(13)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_rust_imports_for_file(&self, file: &str) -> Result<Vec<RustImportRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT file, path, alias, is_glob, start_line, end_line, crate_name
             FROM rust_imports
             WHERE file = ?1
             ORDER BY start_line, end_line, path, alias",
        )?;
        let rows = stmt.query_map(params![file], |row| {
            Ok(RustImportRecord {
                file: row.get(0)?,
                path: row.get(1)?,
                alias: row.get(2)?,
                is_glob: row.get(3)?,
                start_line: row.get(4)?,
                end_line: row.get(5)?,
                crate_name: row.get(6)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_rust_module_decls_for_file(&self, file: &str) -> Result<Vec<RustModuleDeclRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT file, module_name, resolved_path, is_inline, start_line, end_line, crate_name
             FROM rust_module_decls
             WHERE file = ?1
             ORDER BY start_line, end_line, module_name",
        )?;
        let rows = stmt.query_map(params![file], |row| {
            Ok(RustModuleDeclRecord {
                file: row.get(0)?,
                module_name: row.get(1)?,
                resolved_path: row.get(2)?,
                is_inline: row.get(3)?,
                start_line: row.get(4)?,
                end_line: row.get(5)?,
                crate_name: row.get(6)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn search_symbol_by_name(&self, name: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE name LIKE ?1 ORDER BY name LIMIT ?2",
        )?;
        let pattern = format!("%{name}%");
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn search_symbol_by_exact_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols
             WHERE name = ?1 COLLATE NOCASE
             ORDER BY name, file
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit as i64], |row| {
            Ok(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_symbol_exact(
        &self,
        name: &str,
        symbol_type: SymbolType,
    ) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE name = ?1 AND type = ?2 ORDER BY id DESC LIMIT 1",
        )?;

        let mut rows = stmt.query(params![name, symbol_type_to_str(&symbol_type)])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_symbol_any(&self, name: &str) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE name = ?1
             ORDER BY CASE type WHEN 'function' THEN 0 WHEN 'class' THEN 1 ELSE 2 END, id DESC
             LIMIT 1",
        )?;

        let mut rows = stmt.query(params![name])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            }))
        } else {
            Ok(None)
        }
    }

    fn get_symbol_in_file(&self, name: &str, file: &str) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE name = ?1 AND file = ?2
             ORDER BY CASE type WHEN 'function' THEN 0 WHEN 'class' THEN 1 ELSE 2 END, id DESC
             LIMIT 1",
        )?;

        let mut rows = stmt.query(params![name, file])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_symbol_by_id(&self, symbol_id: i64) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE id = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![symbol_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn file_outline(&self, file: &str) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary, signature
             FROM symbols WHERE file = ?1 ORDER BY start_line",
        )?;
        let rows = stmt.query_map(params![file], |row| {
            Ok(SymbolRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                name: row.get(2)?,
                symbol_type: str_to_symbol_type(&row.get::<_, String>(3)?),
                file: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                language: row.get(7)?,
                summary: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_files(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_file_checksum(&self, path: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT checksum FROM files WHERE path = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn delete_file_metadata(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn clear_module_graph(&self) -> Result<()> {
        self.conn.execute("DELETE FROM module_dependencies", [])?;
        self.conn.execute("DELETE FROM module_files", [])?;
        self.conn.execute("DELETE FROM modules", [])?;
        Ok(())
    }

    pub fn insert_module(&self, name: &str, path: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO modules(name, path) VALUES(?1, ?2)",
            params![name, path],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_module_file(&self, module_id: i64, file_path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO module_files(module_id, file_path) VALUES(?1, ?2)",
            params![module_id, file_path],
        )?;
        Ok(())
    }

    pub fn insert_module_dependency(&self, from_module: i64, to_module: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO module_dependencies(from_module, to_module) VALUES(?1, ?2)",
            params![from_module, to_module],
        )?;
        Ok(())
    }

    pub fn list_modules(&self) -> Result<Vec<ModuleRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path FROM modules ORDER BY path, name, id")?;
        let rows = stmt.query_map([], |row| {
            Ok(ModuleRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
            })
        })?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn list_module_files(&self, module_id: i64) -> Result<Vec<ModuleFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT module_id, file_path FROM module_files WHERE module_id = ?1 ORDER BY file_path",
        )?;
        let rows = stmt.query_map(params![module_id], |row| {
            Ok(ModuleFile {
                module_id: row.get(0)?,
                file_path: row.get(1)?,
            })
        })?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn get_module_by_file(&self, file_path: &str) -> Result<Option<ModuleRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.name, m.path
             FROM modules m
             JOIN module_files mf ON mf.module_id = m.id
             WHERE mf.file_path = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![file_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(ModuleRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_module_dependencies(&self) -> Result<Vec<ModuleDependency>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_module, to_module
             FROM module_dependencies
             ORDER BY from_module, to_module",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ModuleDependency {
                from_module: row.get(0)?,
                to_module: row.get(1)?,
            })
        })?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn list_named_module_dependencies(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT m1.name, m2.name
             FROM module_dependencies md
             JOIN modules m1 ON m1.id = md.from_module
             JOIN modules m2 ON m2.id = md.to_module
             ORDER BY m1.name, m2.name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn get_module_dependency_names(&self, module_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT m2.name
             FROM module_dependencies md
             JOIN modules m2 ON m2.id = md.to_module
             WHERE md.from_module = ?1
             ORDER BY m2.name",
        )?;
        let rows = stmt.query_map(params![module_id], |row| row.get::<_, String>(0))?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn register_repository(&self, name: &str, path: &str) -> Result<RepositoryRecord> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path FROM repositories WHERE path = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            return Ok(RepositoryRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
            });
        }

        self.conn.execute(
            "INSERT INTO repositories(name, path) VALUES(?1, ?2)",
            params![name, path],
        )?;
        Ok(RepositoryRecord {
            id: Some(self.conn.last_insert_rowid()),
            name: name.to_string(),
            path: path.to_string(),
        })
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositoryRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path FROM repositories ORDER BY name, path")?;
        let rows = stmt.query_map([], |row| {
            Ok(RepositoryRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
            })
        })?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn get_repository(&self, repo_id: i64) -> Result<Option<RepositoryRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path FROM repositories WHERE id = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![repo_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(RepositoryRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn add_repo_dependency(&self, from_repo: i64, to_repo: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO repo_dependencies(from_repo, to_repo) VALUES(?1, ?2)",
            params![from_repo, to_repo],
        )?;
        Ok(())
    }

    pub fn list_repo_dependencies(&self) -> Result<Vec<RepoDependency>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_repo, to_repo FROM repo_dependencies ORDER BY from_repo, to_repo",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RepoDependency {
                from_repo: row.get(0)?,
                to_repo: row.get(1)?,
            })
        })?;
        let out: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(out?)
    }

    pub fn get_dependencies(&self, caller: &str) -> Result<Vec<DependencyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, caller_symbol, callee_symbol, file, callee_file
             FROM dependencies WHERE caller_symbol = ?1 ORDER BY callee_symbol",
        )?;
        let rows = stmt.query_map(params![caller], |row| {
            Ok(DependencyRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                caller_symbol: row.get(2)?,
                callee_symbol: row.get(3)?,
                file: row.get(4)?,
                callee_file: row.get(5)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_all_dependencies(&self) -> Result<Vec<DependencyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, caller_symbol, callee_symbol, file, callee_file
             FROM dependencies
             ORDER BY caller_symbol, callee_symbol, file",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DependencyRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                caller_symbol: row.get(2)?,
                callee_symbol: row.get(3)?,
                file: row.get(4)?,
                callee_file: row.get(5)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_symbol_dependencies(&self, symbol_id: i64) -> Result<Vec<SymbolRecord>> {
        let symbol = self
            .get_symbol_by_id(symbol_id)?
            .ok_or_else(|| anyhow::anyhow!("symbol not found: {symbol_id}"))?;

        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT callee_symbol, file, callee_file
             FROM dependencies
             WHERE caller_symbol = ?1 AND file = ?2
             ORDER BY callee_symbol, file, callee_file",
        )?;
        let names = stmt.query_map(params![symbol.name, symbol.file], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let names: rusqlite::Result<Vec<_>> = names.collect();

        let mut out = Vec::new();
        for (name, dep_file, callee_file) in names? {
            let neighbor = if let Some(callee_file) = callee_file.as_deref() {
                self.get_symbol_in_file(&name, callee_file)?
            } else {
                self.get_symbol_in_file(&name, &dep_file)?
                    .or(self.get_symbol_any(&name)?)
            };
            if let Some(neighbor) = neighbor {
                out.push(neighbor);
            }
        }
        out.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
        });
        Ok(out)
    }

    pub fn get_symbol_callers(&self, symbol_id: i64) -> Result<Vec<SymbolRecord>> {
        let symbol = self
            .get_symbol_by_id(symbol_id)?
            .ok_or_else(|| anyhow::anyhow!("symbol not found: {symbol_id}"))?;

        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT caller_symbol, file, callee_file
             FROM dependencies
             WHERE callee_symbol = ?1
             ORDER BY caller_symbol, file, callee_file",
        )?;
        let names = stmt.query_map(params![symbol.name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let names: rusqlite::Result<Vec<_>> = names.collect();

        let mut out = Vec::new();
        for (name, dep_file, callee_file) in names? {
            let callee_matches = callee_file
                .as_deref()
                .map(|file| file == symbol.file)
                .unwrap_or(true);
            if !callee_matches {
                continue;
            }
            if let Some(neighbor) = self
                .get_symbol_in_file(&name, &dep_file)?
                .or(self.get_symbol_any(&name)?)
            {
                out.push(neighbor);
            }
        }
        out.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
        });
        Ok(out)
    }

    pub fn get_dependency_neighbors(
        &self,
        symbol_id: i64,
        radius: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let start = self
            .get_symbol_by_id(symbol_id)?
            .ok_or_else(|| anyhow::anyhow!("symbol not found: {symbol_id}"))?;

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut out_ids = Vec::new();
        queue.push_back((start.id.unwrap_or_default(), 0usize));
        visited.insert(start.id.unwrap_or_default());

        while let Some((current_id, depth)) = queue.pop_front() {
            out_ids.push(current_id);
            if depth >= radius {
                continue;
            }

            let mut neighbors = self.get_symbol_dependencies(current_id)?;
            neighbors.extend(self.get_symbol_callers(current_id)?);
            neighbors.sort_by(|a, b| {
                a.file
                    .cmp(&b.file)
                    .then_with(|| a.start_line.cmp(&b.start_line))
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
            });
            neighbors.dedup_by_key(|s| s.id.unwrap_or_default());

            for neighbor in neighbors {
                let nid = neighbor.id.unwrap_or_default();
                if visited.insert(nid) {
                    queue.push_back((nid, depth + 1));
                }
            }
        }

        let mut records = Vec::new();
        for id in out_ids {
            if let Some(symbol) = self.get_symbol_by_id(id)? {
                records.push(symbol);
            }
        }
        records.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
        });
        Ok(records)
    }

    pub fn get_logic_nodes(&self, symbol_id: i64) -> Result<Vec<LogicNodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, symbol_id, node_type, start_line, end_line, semantic_label
             FROM logic_nodes WHERE symbol_id = ?1
             ORDER BY start_line, end_line, id",
        )?;
        let rows = stmt.query_map(params![symbol_id], |row| {
            Ok(LogicNodeRecord {
                id: row.get(0)?,
                symbol_id: row.get(1)?,
                node_type: str_to_logic_node_type(&row.get::<_, String>(2)?),
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                semantic_label: row.get(5)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_logic_node(&self, node_id: i64) -> Result<Option<LogicNodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, symbol_id, node_type, start_line, end_line, semantic_label
             FROM logic_nodes WHERE id = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![node_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(LogicNodeRecord {
                id: row.get(0)?,
                symbol_id: row.get(1)?,
                node_type: str_to_logic_node_type(&row.get::<_, String>(2)?),
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                semantic_label: row.get(5)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_control_flow_edges(&self, symbol_id: i64) -> Result<Vec<FlowEdgeRecord>> {
        self.get_flow_edges("control_flow_edges", symbol_id)
    }

    pub fn get_data_flow_edges(&self, symbol_id: i64) -> Result<Vec<FlowEdgeRecord>> {
        self.get_flow_edges("data_flow_edges", symbol_id)
    }

    pub fn get_logic_clusters(&self, symbol_id: i64) -> Result<Vec<LogicClusterRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, symbol_id, label, start_line, end_line, node_count
             FROM logic_clusters
             WHERE symbol_id = ?1
             ORDER BY start_line, end_line, id",
        )?;
        let rows = stmt.query_map(params![symbol_id], |row| {
            Ok(LogicClusterRecord {
                id: row.get(0)?,
                symbol_id: row.get(1)?,
                label: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                node_count: row.get::<_, i64>(5)? as usize,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    fn get_flow_edges(&self, table: &str, symbol_id: i64) -> Result<Vec<FlowEdgeRecord>> {
        let sql = format!(
            "SELECT id, symbol_id, from_node_id, to_node_id, kind, variable_name
             FROM {table}
             WHERE symbol_id = ?1
             ORDER BY from_node_id, to_node_id, id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![symbol_id], |row| {
            Ok(FlowEdgeRecord {
                id: row.get(0)?,
                symbol_id: row.get(1)?,
                from_node_id: row.get(2)?,
                to_node_id: row.get(3)?,
                kind: str_to_flow_edge_kind(&row.get::<_, String>(4)?),
                variable_name: row.get(5)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_logic_node_file(&self, node_id: i64) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.file
             FROM logic_nodes ln
             JOIN symbols s ON s.id = ln.symbol_id
             WHERE ln.id = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![node_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_logic_neighbors(&self, node_id: i64, radius: usize) -> Result<Vec<LogicNodeRecord>> {
        if radius == 0 {
            return self
                .get_logic_node(node_id)
                .map(|v| v.into_iter().collect::<Vec<_>>());
        }

        let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT from_node_id, to_node_id FROM logic_edges ORDER BY from_node_id, to_node_id")?;
            let rows =
                stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
            for row in rows {
                let (from, to) = row?;
                adjacency.entry(from).or_default().push(to);
                adjacency.entry(to).or_default().push(from);
            }
        }

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back((node_id, 0usize));
        visited.insert(node_id);

        let mut reached = Vec::new();
        while let Some((current, depth)) = queue.pop_front() {
            reached.push(current);
            if depth >= radius {
                continue;
            }
            if let Some(neighbors) = adjacency.get(&current) {
                for next in neighbors {
                    if visited.insert(*next) {
                        queue.push_back((*next, depth + 1));
                    }
                }
            }
        }

        let mut out = Vec::new();
        for id in reached {
            if let Some(node) = self.get_logic_node(id)? {
                out.push(node);
            }
        }
        out.sort_by_key(|n| (n.start_line, n.end_line, n.id.unwrap_or_default()));
        Ok(out)
    }

    pub fn refresh_symbol_index(&self) -> Result<()> {
        let symbols = self.list_symbols()?;
        self.symbol_index.rebuild(&symbols)
    }

    pub fn tantivy_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String)>> {
        self.symbol_index.search(query, limit)
    }
}

fn symbol_type_to_str(symbol_type: &SymbolType) -> &'static str {
    match symbol_type {
        SymbolType::Function => "function",
        SymbolType::Class => "class",
        SymbolType::Import => "import",
    }
}

fn str_to_symbol_type(value: &str) -> SymbolType {
    match value {
        "class" => SymbolType::Class,
        "import" => SymbolType::Import,
        _ => SymbolType::Function,
    }
}

fn logic_node_type_to_str(node_type: &LogicNodeType) -> &'static str {
    match node_type {
        LogicNodeType::Loop => "loop",
        LogicNodeType::Conditional => "conditional",
        LogicNodeType::Try => "try",
        LogicNodeType::Catch => "catch",
        LogicNodeType::Finally => "finally",
        LogicNodeType::Return => "return",
        LogicNodeType::Call => "call",
        LogicNodeType::Await => "await",
        LogicNodeType::Assignment => "assignment",
        LogicNodeType::Throw => "throw",
        LogicNodeType::Switch => "switch",
        LogicNodeType::Case => "case",
    }
}

fn str_to_logic_node_type(value: &str) -> LogicNodeType {
    match value {
        "loop" => LogicNodeType::Loop,
        "conditional" => LogicNodeType::Conditional,
        "try" => LogicNodeType::Try,
        "catch" => LogicNodeType::Catch,
        "finally" => LogicNodeType::Finally,
        "return" => LogicNodeType::Return,
        "call" => LogicNodeType::Call,
        "await" => LogicNodeType::Await,
        "assignment" => LogicNodeType::Assignment,
        "throw" => LogicNodeType::Throw,
        "switch" => LogicNodeType::Switch,
        "case" => LogicNodeType::Case,
        _ => LogicNodeType::Call,
    }
}

fn flow_edge_kind_to_str(kind: &FlowEdgeKind) -> &'static str {
    match kind {
        FlowEdgeKind::Next => "next",
        FlowEdgeKind::Branch => "branch",
        FlowEdgeKind::LoopBack => "loop_back",
        FlowEdgeKind::Exception => "exception",
        FlowEdgeKind::AssignmentToUse => "assignment_to_use",
        FlowEdgeKind::AssignmentToReturn => "assignment_to_return",
        FlowEdgeKind::CallResult => "call_result",
    }
}

fn str_to_flow_edge_kind(value: &str) -> FlowEdgeKind {
    match value {
        "next" => FlowEdgeKind::Next,
        "branch" => FlowEdgeKind::Branch,
        "loop_back" => FlowEdgeKind::LoopBack,
        "exception" => FlowEdgeKind::Exception,
        "assignment_to_return" => FlowEdgeKind::AssignmentToReturn,
        "call_result" => FlowEdgeKind::CallResult,
        _ => FlowEdgeKind::AssignmentToUse,
    }
}

fn ensure_runtime_migrations(conn: &Connection) -> Result<()> {
    let _ = conn.execute(
        "ALTER TABLE logic_nodes ADD COLUMN semantic_label TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE control_flow_edges ADD COLUMN variable_name TEXT",
        [],
    );
    let _ = conn.execute("ALTER TABLE symbols ADD COLUMN signature TEXT", []);
    let _ = conn.execute("ALTER TABLE dependencies ADD COLUMN callee_file TEXT", []);
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS rust_symbol_metadata (
            symbol_id INTEGER PRIMARY KEY,
            kind TEXT NOT NULL,
            owner_name TEXT,
            trait_name TEXT,
            module_path TEXT,
            crate_name TEXT,
            crate_root TEXT,
            FOREIGN KEY(symbol_id) REFERENCES symbols(id)
        )",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE rust_symbol_metadata ADD COLUMN crate_name TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE rust_symbol_metadata ADD COLUMN crate_root TEXT",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_meta_kind ON rust_symbol_metadata(kind)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_meta_owner ON rust_symbol_metadata(owner_name)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_meta_trait ON rust_symbol_metadata(trait_name)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_meta_module ON rust_symbol_metadata(module_path)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_meta_crate_name ON rust_symbol_metadata(crate_name)",
        [],
    );
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS rust_imports (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file TEXT NOT NULL,
            path TEXT NOT NULL,
            alias TEXT,
            is_glob INTEGER NOT NULL DEFAULT 0,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            crate_name TEXT
        )",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_import_file ON rust_imports(file)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_import_path ON rust_imports(path)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_import_alias ON rust_imports(alias)",
        [],
    );
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS rust_module_decls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file TEXT NOT NULL,
            module_name TEXT NOT NULL,
            resolved_path TEXT,
            is_inline INTEGER NOT NULL DEFAULT 0,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            crate_name TEXT
        )",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_module_decl_file ON rust_module_decls(file)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_rust_module_decl_name ON rust_module_decls(module_name)",
        [],
    );
    Ok(())
}

fn insert_flow_edges_tx(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    edges: &[FlowEdgeRecord],
    inserted_nodes: &[LogicNodeRecord],
) -> Result<()> {
    let mut node_map = HashMap::new();
    for node in inserted_nodes {
        node_map.insert(
            provisional_node_id(node.symbol_id, node.start_line, node.end_line),
            node.id.unwrap_or_default(),
        );
    }

    let sql = format!(
        "INSERT INTO {table}(symbol_id, from_node_id, to_node_id, kind, variable_name)
         VALUES(?1, ?2, ?3, ?4, ?5)"
    );
    let mut stmt = tx.prepare(&sql)?;
    for edge in edges {
        let symbol_id = remap_symbol_id(edge.symbol_id, inserted_nodes);
        let Some(from_node_id) = node_map.get(&edge.from_node_id).copied() else {
            continue;
        };
        let Some(to_node_id) = node_map.get(&edge.to_node_id).copied() else {
            continue;
        };
        stmt.execute(params![
            symbol_id,
            from_node_id,
            to_node_id,
            flow_edge_kind_to_str(&edge.kind),
            edge.variable_name,
        ])?;
    }
    Ok(())
}

fn insert_logic_clusters_tx(
    tx: &rusqlite::Transaction<'_>,
    clusters: &[LogicClusterRecord],
    inserted_symbol_ids: &[i64],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO logic_clusters(symbol_id, label, start_line, end_line, node_count)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for cluster in clusters {
        let symbol_idx = (cluster.symbol_id - 1).max(0) as usize;
        let Some(symbol_id) = inserted_symbol_ids.get(symbol_idx).copied() else {
            continue;
        };
        stmt.execute(params![
            symbol_id,
            cluster.label,
            cluster.start_line as i64,
            cluster.end_line as i64,
            cluster.node_count as i64,
        ])?;
    }
    Ok(())
}

fn insert_rust_metadata_tx(
    tx: &rusqlite::Transaction<'_>,
    symbols: &[SymbolRecord],
    inserted_symbol_ids: &[i64],
    rust_metadata: &[RustSymbolMetadataRecord],
) -> Result<()> {
    if rust_metadata.is_empty() {
        return Ok(());
    }

    let mut symbol_lookup = HashMap::new();
    for (symbol, inserted_id) in symbols.iter().zip(inserted_symbol_ids.iter().copied()) {
        symbol_lookup.insert(
            (
                symbol.name.clone(),
                symbol.file.clone(),
                symbol.start_line,
                symbol.end_line,
            ),
            inserted_id,
        );
    }

    let mut stmt = tx.prepare(
        "INSERT INTO rust_symbol_metadata(symbol_id, kind, owner_name, trait_name, module_path, crate_name, crate_root)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for record in rust_metadata {
        let key = (
            record.symbol_name.clone(),
            record.file.clone(),
            record.start_line,
            record.end_line,
        );
        let Some(symbol_id) = symbol_lookup.get(&key).copied() else {
            continue;
        };
        stmt.execute(params![
            symbol_id,
            record.kind,
            record.owner_name,
            record.trait_name,
            record.module_path,
            record.crate_name,
            record.crate_root,
        ])?;
    }
    Ok(())
}

fn insert_rust_imports_tx(
    tx: &rusqlite::Transaction<'_>,
    rust_imports: &[RustImportRecord],
) -> Result<()> {
    if rust_imports.is_empty() {
        return Ok(());
    }

    let mut stmt = tx.prepare(
        "INSERT INTO rust_imports(file, path, alias, is_glob, start_line, end_line, crate_name)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for record in rust_imports {
        stmt.execute(params![
            record.file,
            record.path,
            record.alias,
            record.is_glob,
            record.start_line,
            record.end_line,
            record.crate_name,
        ])?;
    }
    Ok(())
}

fn insert_rust_module_decls_tx(
    tx: &rusqlite::Transaction<'_>,
    rust_module_decls: &[RustModuleDeclRecord],
) -> Result<()> {
    if rust_module_decls.is_empty() {
        return Ok(());
    }

    let mut stmt = tx.prepare(
        "INSERT INTO rust_module_decls(file, module_name, resolved_path, is_inline, start_line, end_line, crate_name)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for record in rust_module_decls {
        stmt.execute(params![
            record.file,
            record.module_name,
            record.resolved_path,
            record.is_inline,
            record.start_line,
            record.end_line,
            record.crate_name,
        ])?;
    }
    Ok(())
}

fn provisional_node_id(symbol_id: i64, start_line: usize, end_line: usize) -> i64 {
    symbol_id * 1_000_000 + (start_line as i64 * 1_000) + end_line as i64
}

fn remap_symbol_id(temp_symbol_id: i64, inserted_nodes: &[LogicNodeRecord]) -> i64 {
    inserted_nodes
        .iter()
        .find(|node| {
            let provisional = provisional_node_id(temp_symbol_id, node.start_line, node.end_line);
            provisional / 1_000_000 == temp_symbol_id
        })
        .map(|node| node.symbol_id)
        .unwrap_or_default()
}

impl Storage {
    // ── error log ─────────────────────────────────────────────────────────

    pub fn ensure_error_log_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS error_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                error_hash TEXT UNIQUE NOT NULL,
                error_kind TEXT NOT NULL,
                message TEXT NOT NULL,
                file_hint TEXT NOT NULL DEFAULT '',
                symbol_hint TEXT NOT NULL DEFAULT '',
                first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL,
                hit_count INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_error_kind ON error_patterns(error_kind);
            CREATE INDEX IF NOT EXISTS idx_error_hash ON error_patterns(error_hash);
            CREATE TABLE IF NOT EXISTS error_solutions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_id INTEGER NOT NULL REFERENCES error_patterns(id),
                solution TEXT NOT NULL,
                outcome TEXT NOT NULL,
                applied_at INTEGER NOT NULL,
                token_cost INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_solution_pattern ON error_solutions(pattern_id);",
        )?;
        Ok(())
    }

    /// Upsert an error pattern. Returns the pattern id.
    pub fn upsert_error_pattern(
        &self,
        hash: &str,
        kind: &str,
        message: &str,
        file_hint: &str,
        symbol_hint: &str,
        now: u64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO error_patterns (error_hash, error_kind, message, file_hint, symbol_hint, first_seen, last_seen, hit_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1)
             ON CONFLICT(error_hash) DO UPDATE SET
               last_seen = excluded.last_seen,
               hit_count = hit_count + 1",
            rusqlite::params![hash, kind, message, file_hint, symbol_hint, now as i64],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM error_patterns WHERE error_hash = ?1",
            rusqlite::params![hash],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(id)
    }

    pub fn insert_error_solution(
        &self,
        pattern_id: i64,
        solution: &str,
        outcome: &str,
        applied_at: u64,
        token_cost: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO error_solutions (pattern_id, solution, outcome, applied_at, token_cost) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![pattern_id, solution, outcome, applied_at as i64, token_cost],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn find_error_patterns_by_hash(&self, hash: &str) -> Result<Vec<engine::ErrorPattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, error_hash, error_kind, message, file_hint, symbol_hint, first_seen, last_seen, hit_count
             FROM error_patterns WHERE error_hash = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![hash], map_error_pattern)?;
        let collected: rusqlite::Result<Vec<engine::ErrorPattern>> = rows.collect();
        Ok(collected.unwrap_or_default())
    }

    pub fn find_error_patterns_by_kind(
        &self,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<engine::ErrorPattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, error_hash, error_kind, message, file_hint, symbol_hint, first_seen, last_seen, hit_count
             FROM error_patterns WHERE error_kind = ?1 ORDER BY hit_count DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![kind, limit as i64], map_error_pattern)?;
        let collected: rusqlite::Result<Vec<engine::ErrorPattern>> = rows.collect();
        Ok(collected.unwrap_or_default())
    }

    pub fn get_error_solutions(&self, pattern_id: i64) -> Result<Vec<engine::ErrorSolution>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pattern_id, solution, outcome, applied_at, token_cost
             FROM error_solutions WHERE pattern_id = ?1 ORDER BY applied_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern_id], |row: &rusqlite::Row<'_>| {
            Ok(engine::ErrorSolution {
                id: row.get(0)?,
                pattern_id: row.get(1)?,
                solution: row.get(2)?,
                outcome: row.get(3)?,
                applied_at: row.get::<_, i64>(4)? as u64,
                token_cost: row.get(5)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<engine::ErrorSolution>> = rows.collect();
        Ok(collected.unwrap_or_default())
    }

    /// Prune oldest error patterns when count exceeds max.
    pub fn prune_error_patterns(&self, max: usize) -> Result<()> {
        self.conn.execute(
            "DELETE FROM error_patterns WHERE id IN (
                SELECT id FROM error_patterns ORDER BY last_seen ASC
                LIMIT MAX(0, (SELECT COUNT(*) FROM error_patterns) - ?1)
             )",
            rusqlite::params![max as i64],
        )?;
        Ok(())
    }

    /// List all error patterns ordered by hit_count desc.
    pub fn list_error_patterns(&self, limit: usize) -> Result<Vec<engine::ErrorPattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, error_hash, error_kind, message, file_hint, symbol_hint, first_seen, last_seen, hit_count
             FROM error_patterns ORDER BY hit_count DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], map_error_pattern)?;
        let collected: rusqlite::Result<Vec<engine::ErrorPattern>> = rows.collect();
        Ok(collected.unwrap_or_default())
    }
} // impl Storage (error log)

fn map_error_pattern(row: &rusqlite::Row<'_>) -> rusqlite::Result<engine::ErrorPattern> {
    Ok(engine::ErrorPattern {
        id: row.get(0)?,
        error_hash: row.get(1)?,
        error_kind: row.get(2)?,
        message: row.get(3)?,
        file_hint: row.get(4)?,
        symbol_hint: row.get(5)?,
        first_seen: row.get::<_, i64>(6)? as u64,
        last_seen: row.get::<_, i64>(7)? as u64,
        hit_count: row.get(8)?,
    })
}

pub fn default_paths(base: &Path) -> (PathBuf, PathBuf) {
    (
        base.join(".semantic").join("semantic.db"),
        base.join(".semantic").join("tantivy"),
    )
}

#[cfg(test)]
mod tests {
    use super::{RetrievalCacheEntry, Storage};
    use engine::{DependencyRecord, LogicNodeRecord, LogicNodeType, SymbolRecord, SymbolType};

    #[test]
    fn writes_and_reads_symbols() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("db.sqlite");
        let idx = tmp.path().join("idx");
        let mut storage = Storage::open(&db, &idx).expect("storage open");

        let ids = storage
            .insert_symbols(&[SymbolRecord {
                id: None,
                repo_id: 0,
                name: "retryRequest".to_string(),
                symbol_type: SymbolType::Function,
                file: "src/a.ts".to_string(),
                start_line: 1,
                end_line: 8,
                language: "typescript".to_string(),
                summary: "Function retryRequest".to_string(),
                signature: None,
            }])
            .expect("insert symbols");

        let symbols = storage
            .search_symbol_by_name("retry", 10)
            .expect("query symbols");
        assert_eq!(symbols.len(), 1);

        let logic_ids = storage
            .insert_logic_nodes(
                ids[0],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: ids[0],
                    node_type: LogicNodeType::Return,
                    start_line: 3,
                    end_line: 3,
                    semantic_label: "result_exit".to_string(),
                }],
            )
            .expect("insert logic nodes");
        assert_eq!(logic_ids.len(), 1);

        let nodes = storage.get_logic_nodes(ids[0]).expect("get logic nodes");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn stores_retrieval_cache_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("db.sqlite");
        let idx = tmp.path().join("idx");
        let storage = Storage::open(&db, &idx).expect("storage open");

        storage
            .upsert_retrieval_cache_entry(&RetrievalCacheEntry {
                cache_key: "planned::abc".to_string(),
                cache_kind: "planned_context".to_string(),
                value_json: Some("{\"ok\":true}".to_string()),
                prompt_text: None,
                cached_at_epoch_s: 10,
                source_revision: 20,
            })
            .expect("cache upsert");

        let entry = storage
            .get_retrieval_cache_entry("planned::abc", "planned_context")
            .expect("cache get")
            .expect("entry");
        assert_eq!(entry.source_revision, 20);
        assert_eq!(entry.value_json.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn bfs_dependency_neighbors_is_deterministic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("db.sqlite");
        let idx = tmp.path().join("idx");
        let mut storage = Storage::open(&db, &idx).expect("storage open");

        storage
            .upsert_file("src/flow.ts", "typescript", "x")
            .expect("upsert file");
        let ids = storage
            .insert_symbols(&[
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "a".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "src/flow.ts".to_string(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".to_string(),
                    summary: "a".to_string(),
                    signature: None,
                },
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "b".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "src/flow.ts".to_string(),
                    start_line: 2,
                    end_line: 2,
                    language: "typescript".to_string(),
                    summary: "b".to_string(),
                    signature: None,
                },
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "c".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "src/flow.ts".to_string(),
                    start_line: 3,
                    end_line: 3,
                    language: "typescript".to_string(),
                    summary: "c".to_string(),
                    signature: None,
                },
            ])
            .expect("insert symbols");

        storage
            .insert_dependencies(&[
                DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "a".to_string(),
                    callee_symbol: "b".to_string(),
                    file: "src/flow.ts".to_string(),
                    callee_file: None,
                },
                DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "c".to_string(),
                    callee_symbol: "b".to_string(),
                    file: "src/flow.ts".to_string(),
                    callee_file: None,
                },
            ])
            .expect("insert dependencies");

        let neighbors = storage
            .get_dependency_neighbors(ids[1], 2)
            .expect("dependency neighbors");
        assert!(neighbors.iter().any(|s| s.name == "a"));
        assert!(neighbors.iter().any(|s| s.name == "c"));
    }

    #[test]
    fn duplicate_symbol_callers_resolve_within_dependency_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("db.sqlite");
        let idx = tmp.path().join("idx");
        let mut storage = Storage::open(&db, &idx).expect("storage open");

        storage
            .upsert_file("packages/api/src/service.ts", "typescript", "api")
            .expect("upsert api file");
        storage
            .upsert_file("packages/web/src/service.ts", "typescript", "web")
            .expect("upsert web file");

        let ids = storage
            .insert_symbols(&[
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "loadConfig".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "packages/api/src/service.ts".to_string(),
                    start_line: 1,
                    end_line: 3,
                    language: "typescript".to_string(),
                    summary: "api load config".to_string(),
                    signature: Some("loadConfig()".to_string()),
                },
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "buildClient".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "packages/api/src/service.ts".to_string(),
                    start_line: 5,
                    end_line: 7,
                    language: "typescript".to_string(),
                    summary: "build client".to_string(),
                    signature: Some("buildClient()".to_string()),
                },
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "loadConfig".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "packages/web/src/service.ts".to_string(),
                    start_line: 1,
                    end_line: 3,
                    language: "typescript".to_string(),
                    summary: "web load config".to_string(),
                    signature: Some("loadConfig()".to_string()),
                },
                SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "renderApp".to_string(),
                    symbol_type: SymbolType::Function,
                    file: "packages/web/src/service.ts".to_string(),
                    start_line: 5,
                    end_line: 7,
                    language: "typescript".to_string(),
                    summary: "render app".to_string(),
                    signature: Some("renderApp()".to_string()),
                },
            ])
            .expect("insert duplicate-name symbols");

        storage
            .insert_dependencies(&[
                DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "buildClient".to_string(),
                    callee_symbol: "loadConfig".to_string(),
                    file: "packages/api/src/service.ts".to_string(),
                    callee_file: Some("packages/api/src/service.ts".to_string()),
                },
                DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "renderApp".to_string(),
                    callee_symbol: "loadConfig".to_string(),
                    file: "packages/web/src/service.ts".to_string(),
                    callee_file: Some("packages/web/src/service.ts".to_string()),
                },
            ])
            .expect("insert dependencies");

        let api_callers = storage
            .get_symbol_callers(ids[0])
            .expect("api loadConfig callers");
        assert_eq!(api_callers.len(), 1);
        assert_eq!(api_callers[0].name, "buildClient");
        assert_eq!(api_callers[0].file, "packages/api/src/service.ts");

        let web_callers = storage
            .get_symbol_callers(ids[2])
            .expect("web loadConfig callers");
        assert_eq!(web_callers.len(), 1);
        assert_eq!(web_callers[0].name, "renderApp");
        assert_eq!(web_callers[0].file, "packages/web/src/service.ts");
    }
}
