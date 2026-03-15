CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT UNIQUE NOT NULL,
    language TEXT NOT NULL,
    checksum TEXT NOT NULL,
    indexed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL DEFAULT 0,
    name TEXT NOT NULL,
    type TEXT NOT NULL,
    file TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    language TEXT NOT NULL,
    summary TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file);

CREATE TABLE IF NOT EXISTS dependencies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL DEFAULT 0,
    caller_symbol TEXT NOT NULL,
    callee_symbol TEXT NOT NULL,
    file TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dependencies_caller ON dependencies(caller_symbol);

CREATE TABLE IF NOT EXISTS logic_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_id INTEGER NOT NULL,
    node_type TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    semantic_label TEXT NOT NULL DEFAULT '',
    FOREIGN KEY(symbol_id) REFERENCES symbols(id)
);
CREATE INDEX IF NOT EXISTS idx_logic_symbol ON logic_nodes(symbol_id);

CREATE TABLE IF NOT EXISTS logic_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_node_id INTEGER NOT NULL,
    to_node_id INTEGER NOT NULL,
    FOREIGN KEY(from_node_id) REFERENCES logic_nodes(id),
    FOREIGN KEY(to_node_id) REFERENCES logic_nodes(id)
);
CREATE INDEX IF NOT EXISTS idx_logic_from ON logic_edges(from_node_id);
CREATE INDEX IF NOT EXISTS idx_logic_to ON logic_edges(to_node_id);

CREATE TABLE IF NOT EXISTS control_flow_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_id INTEGER NOT NULL,
    from_node_id INTEGER NOT NULL,
    to_node_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    variable_name TEXT,
    FOREIGN KEY(symbol_id) REFERENCES symbols(id),
    FOREIGN KEY(from_node_id) REFERENCES logic_nodes(id),
    FOREIGN KEY(to_node_id) REFERENCES logic_nodes(id)
);
CREATE INDEX IF NOT EXISTS idx_cfg_symbol ON control_flow_edges(symbol_id);
CREATE INDEX IF NOT EXISTS idx_cfg_from ON control_flow_edges(from_node_id);

CREATE TABLE IF NOT EXISTS data_flow_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_id INTEGER NOT NULL,
    from_node_id INTEGER NOT NULL,
    to_node_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    variable_name TEXT,
    FOREIGN KEY(symbol_id) REFERENCES symbols(id),
    FOREIGN KEY(from_node_id) REFERENCES logic_nodes(id),
    FOREIGN KEY(to_node_id) REFERENCES logic_nodes(id)
);
CREATE INDEX IF NOT EXISTS idx_dfg_symbol ON data_flow_edges(symbol_id);
CREATE INDEX IF NOT EXISTS idx_dfg_from ON data_flow_edges(from_node_id);
CREATE INDEX IF NOT EXISTS idx_dfg_var ON data_flow_edges(variable_name);

CREATE TABLE IF NOT EXISTS logic_clusters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_id INTEGER NOT NULL,
    label TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    node_count INTEGER NOT NULL,
    FOREIGN KEY(symbol_id) REFERENCES symbols(id)
);
CREATE INDEX IF NOT EXISTS idx_logic_cluster_symbol ON logic_clusters(symbol_id);

CREATE TABLE IF NOT EXISTS modules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    path TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_module_path ON modules(path);

CREATE TABLE IF NOT EXISTS module_files (
    module_id INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    FOREIGN KEY(module_id) REFERENCES modules(id)
);
CREATE INDEX IF NOT EXISTS idx_module_file ON module_files(file_path);

CREATE TABLE IF NOT EXISTS module_dependencies (
    from_module INTEGER NOT NULL,
    to_module INTEGER NOT NULL,
    FOREIGN KEY(from_module) REFERENCES modules(id),
    FOREIGN KEY(to_module) REFERENCES modules(id)
);

CREATE TABLE IF NOT EXISTS repositories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    path TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repo_path ON repositories(path);

CREATE TABLE IF NOT EXISTS repo_dependencies (
    from_repo INTEGER,
    to_repo INTEGER,
    FOREIGN KEY(from_repo) REFERENCES repositories(id),
    FOREIGN KEY(to_repo) REFERENCES repositories(id)
);

CREATE TABLE IF NOT EXISTS rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS skills (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
