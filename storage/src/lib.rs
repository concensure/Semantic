use anyhow::Result;
use engine::{
    DependencyRecord, FlowEdgeKind, FlowEdgeRecord, LogicClusterRecord, LogicEdgeRecord,
    LogicNodeRecord, LogicNodeType, ModuleDependency, ModuleFile, ModuleRecord, RepoDependency,
    RepositoryRecord, SymbolRecord, SymbolType,
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
        tx.execute("DELETE FROM symbols WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM dependencies WHERE file = ?1", params![file])?;

        let mut inserted_symbol_ids = Vec::with_capacity(symbols.len());
        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols(repo_id, name, type, file, start_line, end_line, language, summary)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                ])?;
                inserted_symbol_ids.push(tx.last_insert_rowid());
            }
        }

        {
            let mut stmt = tx.prepare(
                "INSERT INTO dependencies(repo_id, caller_symbol, callee_symbol, file)
                 VALUES(?1, ?2, ?3, ?4)",
            )?;
            for d in deps {
                stmt.execute(params![repo_id, d.caller_symbol, d.callee_symbol, d.file])?;
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

        insert_flow_edges_tx(&tx, "control_flow_edges", control_flow_edges, &inserted_logic_nodes)?;
        insert_flow_edges_tx(&tx, "data_flow_edges", data_flow_edges, &inserted_logic_nodes)?;
        insert_logic_clusters_tx(&tx, logic_clusters, &inserted_symbol_ids)?;

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
                "INSERT INTO symbols(repo_id, name, type, file, start_line, end_line, language, summary)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                ])?;
                inserted.push(tx.last_insert_rowid());
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn insert_dependencies(&self, deps: &[DependencyRecord]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO dependencies(repo_id, caller_symbol, callee_symbol, file)
             VALUES(?1, ?2, ?3, ?4)",
        )?;
        for d in deps {
            stmt.execute(params![d.repo_id, d.caller_symbol, d.callee_symbol, d.file])?;
        }
        Ok(())
    }

    pub fn insert_logic_nodes(&self, symbol_id: i64, nodes: &[LogicNodeRecord]) -> Result<Vec<i64>> {
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
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary FROM symbols",
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
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn search_symbol_by_name(&self, name: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary
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
            })
        })?;

        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn get_symbol_exact(&self, name: &str, symbol_type: SymbolType) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary
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
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_symbol_any(&self, name: &str) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary
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
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_symbol_by_id(&self, symbol_id: i64) -> Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary
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
            }))
        } else {
            Ok(None)
        }
    }

    pub fn file_outline(&self, file: &str) -> Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, name, type, file, start_line, end_line, language, summary
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
            "SELECT id, repo_id, caller_symbol, callee_symbol, file
             FROM dependencies WHERE caller_symbol = ?1 ORDER BY callee_symbol",
        )?;
        let rows = stmt.query_map(params![caller], |row| {
            Ok(DependencyRecord {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                caller_symbol: row.get(2)?,
                callee_symbol: row.get(3)?,
                file: row.get(4)?,
            })
        })?;
        let collected: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(collected?)
    }

    pub fn list_all_dependencies(&self) -> Result<Vec<DependencyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, caller_symbol, callee_symbol, file
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
            "SELECT DISTINCT callee_symbol
             FROM dependencies
             WHERE caller_symbol = ?1
             ORDER BY callee_symbol",
        )?;
        let names = stmt.query_map(params![symbol.name], |row| row.get::<_, String>(0))?;
        let names: rusqlite::Result<Vec<_>> = names.collect();

        let mut out = Vec::new();
        for name in names? {
            if let Some(neighbor) = self.get_symbol_any(&name)? {
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
            "SELECT DISTINCT caller_symbol
             FROM dependencies
             WHERE callee_symbol = ?1
             ORDER BY caller_symbol",
        )?;
        let names = stmt.query_map(params![symbol.name], |row| row.get::<_, String>(0))?;
        let names: rusqlite::Result<Vec<_>> = names.collect();

        let mut out = Vec::new();
        for name in names? {
            if let Some(neighbor) = self.get_symbol_any(&name)? {
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

    pub fn get_dependency_neighbors(&self, symbol_id: i64, radius: usize) -> Result<Vec<SymbolRecord>> {
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
            let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
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

    pub fn tantivy_search(&self, query: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
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

pub fn default_paths(base: &Path) -> (PathBuf, PathBuf) {
    (
        base.join(".semantic").join("semantic.db"),
        base.join(".semantic").join("tantivy"),
    )
}

#[cfg(test)]
mod tests {
    use super::Storage;
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
                },
                DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "c".to_string(),
                    callee_symbol: "b".to_string(),
                    file: "src/flow.ts".to_string(),
                },
            ])
            .expect("insert dependencies");

        let neighbors = storage
            .get_dependency_neighbors(ids[1], 2)
            .expect("dependency neighbors");
        assert!(neighbors.iter().any(|s| s.name == "a"));
        assert!(neighbors.iter().any(|s| s.name == "c"));
    }
}


