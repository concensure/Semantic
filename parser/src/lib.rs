use anyhow::{anyhow, Context, Result};
use ast_cache::AstCache;
use engine::{
    DependencyRecord, FlowEdgeKind, FlowEdgeRecord, LogicClusterRecord, LogicNodeRecord,
    LogicNodeType, ParsedFile, SymbolRecord, SymbolType,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Python,
    JavaScript,
    TypeScript,
}

impl SupportedLanguage {
    pub fn from_path(path: &str) -> Option<Self> {
        if path.ends_with(".py") {
            Some(Self::Python)
        } else if path.ends_with(".js") || path.ends_with(".jsx") {
            Some(Self::JavaScript)
        } else if path.ends_with(".ts") || path.ends_with(".tsx") {
            Some(Self::TypeScript)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
        }
    }

    fn grammar(self) -> Language {
        match self {
            Self::Python => tree_sitter_python::language(),
            Self::JavaScript => tree_sitter_javascript::language(),
            Self::TypeScript => tree_sitter_typescript::language_typescript(),
        }
    }
}

pub struct CodeParser {
    parser: Parser,
    cache: AstCache,
}

impl CodeParser {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            cache: AstCache::default(),
        }
    }

    pub fn parse(&mut self, path: &str, content: &str) -> Result<ParsedFile> {
        let lang = SupportedLanguage::from_path(path)
            .ok_or_else(|| anyhow!("unsupported language for file: {path}"))?;

        self.parser
            .set_language(&lang.grammar())
            .context("failed to set tree-sitter language")?;

        let cache_key = format!("{path}:{}", checksum(content));
        let tree = if let Some(cached) = self.cache.get(&cache_key) {
            cached
        } else {
            let parsed = self
                .parser
                .parse(content, None)
                .ok_or_else(|| anyhow!("failed to parse file: {path}"))?;
            self.cache.set(cache_key, parsed.clone());
            parsed
        };

        let root = tree.root_node();
        let mut symbols = Vec::new();
        let mut dependencies = Vec::new();
        let mut logic_nodes = Vec::new();
        collect_nodes(
            root,
            content,
            path,
            lang.name(),
            &mut symbols,
            &mut dependencies,
            &mut logic_nodes,
        );
        let (control_flow_edges, data_flow_edges, logic_clusters) =
            build_graph_artifacts(&logic_nodes, content);

        Ok(ParsedFile {
            file: path.to_string(),
            language: lang.name().to_string(),
            symbols,
            dependencies,
            logic_nodes,
            control_flow_edges,
            data_flow_edges,
            logic_clusters,
        })
    }
}

fn checksum(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn collect_nodes(
    node: Node,
    src: &str,
    file: &str,
    language: &str,
    symbols: &mut Vec<SymbolRecord>,
    deps: &mut Vec<DependencyRecord>,
    logic_nodes: &mut Vec<LogicNodeRecord>,
) {
    let kind = node.kind();

    if is_function_node(kind) {
        if let Some(name) = extract_definition_name(node, src) {
            let signature = extract_function_signature(node, src, &name);
            symbols.push(SymbolRecord {
                id: None,
                repo_id: 0,
                name: name.clone(),
                symbol_type: SymbolType::Function,
                file: file.to_string(),
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                language: language.to_string(),
                summary: format!("Function {name}"),
                signature,
            });
            let symbol_ref = symbols.len() as i64;
            collect_call_edges(node, src, file, &name, deps);
            collect_logic_nodes_in_symbol(node, src, symbol_ref, logic_nodes);
        }
    } else if is_class_node(kind) {
        if let Some(name) = extract_definition_name(node, src) {
            let signature = extract_class_signature(node, src, &name);
            symbols.push(SymbolRecord {
                id: None,
                repo_id: 0,
                name: name.clone(),
                symbol_type: SymbolType::Class,
                file: file.to_string(),
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                language: language.to_string(),
                summary: format!("Class {name}"),
                signature,
            });
        }
    } else if is_import_node(kind) {
        let import_name = format!("import@{}", node.start_position().row + 1);
        symbols.push(SymbolRecord {
            id: None,
            repo_id: 0,
            name: import_name,
            symbol_type: SymbolType::Import,
            file: file.to_string(),
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            language: language.to_string(),
            summary: "Import statement".to_string(),
            signature: None,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_nodes(child, src, file, language, symbols, deps, logic_nodes);
    }
}

fn collect_call_edges(
    node: Node,
    src: &str,
    file: &str,
    caller_name: &str,
    deps: &mut Vec<DependencyRecord>,
) {
    let mut stack = vec![node];
    while let Some(next) = stack.pop() {
        if is_call_node(next.kind()) {
            if let Some(callee) = extract_call_name(next, src) {
                deps.push(DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: caller_name.to_string(),
                    callee_symbol: callee,
                    file: file.to_string(),
                });
            }
        }

        let mut cursor = next.walk();
        for child in next.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn collect_logic_nodes_in_symbol(
    root: Node,
    src: &str,
    symbol_ref: i64,
    logic_nodes: &mut Vec<LogicNodeRecord>,
) {
    let mut stack = vec![root];

    while let Some(next) = stack.pop() {
        if next.id() != root.id() && is_function_node(next.kind()) {
            continue;
        }

        if let Some(node_type) = map_logic_node_type(next.kind()) {
            logic_nodes.push(LogicNodeRecord {
                id: None,
                symbol_id: symbol_ref,
                node_type,
                start_line: next.start_position().row + 1,
                end_line: next.end_position().row + 1,
                semantic_label: infer_semantic_label(node_type, node_text(next, src)),
            });
        }

        let mut cursor = next.walk();
        for child in next.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn node_text(node: Node, src: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    src.get(start..end).unwrap_or_default().to_string()
}

fn infer_semantic_label(node_type: LogicNodeType, snippet: String) -> String {
    let normalized = snippet.to_ascii_lowercase();
    match node_type {
        LogicNodeType::Conditional => {
            if normalized.contains("return") || normalized.contains("throw") {
                "guard_clause".to_string()
            } else {
                "branch_decision".to_string()
            }
        }
        LogicNodeType::Loop => "iteration".to_string(),
        LogicNodeType::Try => "risky_operation".to_string(),
        LogicNodeType::Catch | LogicNodeType::Finally => "error_recovery".to_string(),
        LogicNodeType::Return => "result_exit".to_string(),
        LogicNodeType::Call => "side_effect_call".to_string(),
        LogicNodeType::Await => "async_wait".to_string(),
        LogicNodeType::Assignment => {
            if normalized.contains("state") || normalized.contains("cache") {
                "state_update".to_string()
            } else {
                "value_assignment".to_string()
            }
        }
        LogicNodeType::Throw => "error_exit".to_string(),
        LogicNodeType::Switch | LogicNodeType::Case => "multi_branch".to_string(),
    }
}

fn build_graph_artifacts(
    logic_nodes: &[LogicNodeRecord],
    content: &str,
) -> (Vec<FlowEdgeRecord>, Vec<FlowEdgeRecord>, Vec<LogicClusterRecord>) {
    let mut per_symbol: HashMap<i64, Vec<LogicNodeRecord>> = HashMap::new();
    for node in logic_nodes {
        per_symbol
            .entry(node.symbol_id)
            .or_default()
            .push(node.clone());
    }

    let mut control_flow_edges = Vec::new();
    let mut data_flow_edges = Vec::new();
    let mut logic_clusters = Vec::new();

    for (symbol_id, mut nodes) in per_symbol {
        nodes.sort_by_key(|n| (n.start_line, n.end_line));
        control_flow_edges.extend(build_control_flow_edges(symbol_id, &nodes));
        data_flow_edges.extend(build_data_flow_edges(symbol_id, &nodes, content));
        logic_clusters.extend(build_logic_clusters(symbol_id, &nodes));
    }

    (control_flow_edges, data_flow_edges, logic_clusters)
}

fn build_control_flow_edges(symbol_id: i64, nodes: &[LogicNodeRecord]) -> Vec<FlowEdgeRecord> {
    let mut edges = Vec::new();
    for pair in nodes.windows(2) {
        let from = &pair[0];
        let to = &pair[1];
        edges.push(FlowEdgeRecord {
            id: None,
            symbol_id,
            from_node_id: provisional_node_id(symbol_id, from),
            to_node_id: provisional_node_id(symbol_id, to),
            kind: FlowEdgeKind::Next,
            variable_name: None,
        });
    }

    for (idx, node) in nodes.iter().enumerate() {
        match node.node_type {
            LogicNodeType::Conditional | LogicNodeType::Switch | LogicNodeType::Case => {
                if let Some(target) = nodes.get(idx + 1) {
                    edges.push(FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: provisional_node_id(symbol_id, node),
                        to_node_id: provisional_node_id(symbol_id, target),
                        kind: FlowEdgeKind::Branch,
                        variable_name: None,
                    });
                }
                if let Some(target) = first_node_after_line(nodes, node.start_line, idx + 2) {
                    edges.push(FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: provisional_node_id(symbol_id, node),
                        to_node_id: provisional_node_id(symbol_id, target),
                        kind: FlowEdgeKind::Branch,
                        variable_name: None,
                    });
                }
            }
            LogicNodeType::Loop => {
                if let Some(last_nested) = last_nested_node(nodes, node, idx) {
                    edges.push(FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: provisional_node_id(symbol_id, last_nested),
                        to_node_id: provisional_node_id(symbol_id, node),
                        kind: FlowEdgeKind::LoopBack,
                        variable_name: None,
                    });
                }
            }
            LogicNodeType::Try => {
                if let Some(handler) = first_matching_nested(
                    nodes,
                    node,
                    idx,
                    &[LogicNodeType::Catch, LogicNodeType::Finally],
                ) {
                    edges.push(FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: provisional_node_id(symbol_id, node),
                        to_node_id: provisional_node_id(symbol_id, handler),
                        kind: FlowEdgeKind::Exception,
                        variable_name: None,
                    });
                }
            }
            _ => {}
        }
    }
    dedupe_edges(edges)
}

fn build_data_flow_edges(
    symbol_id: i64,
    nodes: &[LogicNodeRecord],
    content: &str,
) -> Vec<FlowEdgeRecord> {
    let lines: Vec<&str> = content.lines().collect();
    let mut edges = Vec::new();
    let mut last_assignment: HashMap<String, FlowEdgeRecord> = HashMap::new();

    for node in nodes {
        let snippet = span_snippet(&lines, node.start_line, node.end_line);
        let identifiers = extract_identifiers(&snippet);
        if identifiers.is_empty() {
            continue;
        }

        if matches!(node.node_type, LogicNodeType::Assignment) {
            let (defs, uses) = split_assignment_identifiers(&snippet);
            let defs = if defs.is_empty() { identifiers.clone() } else { defs };

            for name in defs {
                last_assignment.insert(
                    name.clone(),
                    FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: provisional_node_id(symbol_id, node),
                        to_node_id: provisional_node_id(symbol_id, node),
                        kind: FlowEdgeKind::AssignmentToUse,
                        variable_name: Some(name),
                    },
                );
            }

            for name in uses {
                if let Some(source) = last_assignment.get(&name) {
                    edges.push(FlowEdgeRecord {
                        id: None,
                        symbol_id,
                        from_node_id: source.from_node_id,
                        to_node_id: provisional_node_id(symbol_id, node),
                        kind: FlowEdgeKind::AssignmentToUse,
                        variable_name: Some(name),
                    });
                }
            }
            continue;
        }

        for name in identifiers {
            if let Some(source) = last_assignment.get(&name) {
                let kind = match node.node_type {
                    LogicNodeType::Return => FlowEdgeKind::AssignmentToReturn,
                    LogicNodeType::Call | LogicNodeType::Await => FlowEdgeKind::CallResult,
                    _ => FlowEdgeKind::AssignmentToUse,
                };
                edges.push(FlowEdgeRecord {
                    id: None,
                    symbol_id,
                    from_node_id: source.from_node_id,
                    to_node_id: provisional_node_id(symbol_id, node),
                    kind,
                    variable_name: Some(name),
                });
            }
        }
    }

    dedupe_edges(edges)
}

fn build_logic_clusters(symbol_id: i64, nodes: &[LogicNodeRecord]) -> Vec<LogicClusterRecord> {
    if nodes.is_empty() {
        return Vec::new();
    }

    let mut clusters = Vec::new();
    let mut current_label = cluster_label(&nodes[0]);
    let mut start_line = nodes[0].start_line;
    let mut end_line = nodes[0].end_line;
    let mut count = 1usize;

    for pair in nodes.windows(2) {
        let prev = &pair[0];
        let node = &pair[1];
        let label = cluster_label(node);
        let close_in_source = node.start_line <= prev.end_line.saturating_add(4);

        if label == current_label && close_in_source {
            end_line = node.end_line;
            count += 1;
            continue;
        }

        clusters.push(LogicClusterRecord {
            id: None,
            symbol_id,
            label: current_label.clone(),
            start_line,
            end_line,
            node_count: count,
        });
        current_label = label;
        start_line = node.start_line;
        end_line = node.end_line;
        count = 1;
    }

    clusters.push(LogicClusterRecord {
        id: None,
        symbol_id,
        label: current_label,
        start_line,
        end_line,
        node_count: count,
    });
    clusters
}

fn cluster_label(node: &LogicNodeRecord) -> String {
    match node.semantic_label.as_str() {
        "guard_clause" | "branch_decision" | "multi_branch" => "decision_block".to_string(),
        "iteration" => "iteration_block".to_string(),
        "error_recovery" | "error_exit" | "risky_operation" => "error_path".to_string(),
        "async_wait" | "side_effect_call" => "side_effects".to_string(),
        "state_update" | "value_assignment" => "state_changes".to_string(),
        "result_exit" => "result_path".to_string(),
        _ => "logic_block".to_string(),
    }
}

fn first_node_after_line<'a>(
    nodes: &'a [LogicNodeRecord],
    start_line: usize,
    offset: usize,
) -> Option<&'a LogicNodeRecord> {
    nodes.iter().skip(offset).find(|n| n.start_line > start_line)
}

fn last_nested_node<'a>(
    nodes: &'a [LogicNodeRecord],
    parent: &LogicNodeRecord,
    idx: usize,
) -> Option<&'a LogicNodeRecord> {
    nodes.iter()
        .skip(idx + 1)
        .take_while(|n| n.start_line <= parent.end_line)
        .filter(|n| n.end_line <= parent.end_line)
        .last()
}

fn first_matching_nested<'a>(
    nodes: &'a [LogicNodeRecord],
    parent: &LogicNodeRecord,
    idx: usize,
    kinds: &[LogicNodeType],
) -> Option<&'a LogicNodeRecord> {
    nodes.iter()
        .skip(idx + 1)
        .take_while(|n| n.start_line <= parent.end_line)
        .find(|n| kinds.contains(&n.node_type))
}

fn span_snippet(lines: &[&str], start_line: usize, end_line: usize) -> String {
    if start_line == 0 || end_line < start_line {
        return String::new();
    }
    lines.iter()
        .skip(start_line.saturating_sub(1))
        .take(end_line.saturating_sub(start_line) + 1)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_identifiers(snippet: &str) -> Vec<String> {
    let keywords = [
        "return", "await", "throw", "true", "false", "null", "none", "self", "this", "let",
        "const", "var", "if", "else", "for", "while", "switch", "case", "try", "catch",
        "finally", "new",
    ];
    snippet
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| !keywords.contains(&token.as_str()))
        .filter(|token| !token.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

fn split_assignment_identifiers(snippet: &str) -> (Vec<String>, Vec<String>) {
    let mut parts = snippet.splitn(2, '=');
    let lhs = parts.next().unwrap_or_default();
    let rhs = parts.next().unwrap_or_default();
    (extract_identifiers(lhs), extract_identifiers(rhs))
}

fn provisional_node_id(symbol_id: i64, node: &LogicNodeRecord) -> i64 {
    let start = node.start_line as i64;
    let end = node.end_line as i64;
    symbol_id * 1_000_000 + (start * 1_000) + end
}

fn dedupe_edges(edges: Vec<FlowEdgeRecord>) -> Vec<FlowEdgeRecord> {
    let mut seen = HashMap::new();
    for edge in edges {
        let key = (
            edge.symbol_id,
            edge.from_node_id,
            edge.to_node_id,
            edge.kind.clone(),
            edge.variable_name.clone(),
        );
        seen.entry(key).or_insert(edge);
    }
    seen.into_values().collect()
}

fn map_logic_node_type(kind: &str) -> Option<LogicNodeType> {
    match kind {
        "for_statement" | "while_statement" | "for_in_statement" => Some(LogicNodeType::Loop),
        "if_statement" => Some(LogicNodeType::Conditional),
        "try_statement" => Some(LogicNodeType::Try),
        "except_clause" | "catch_clause" => Some(LogicNodeType::Catch),
        "finally_clause" => Some(LogicNodeType::Finally),
        "return_statement" => Some(LogicNodeType::Return),
        "call" | "call_expression" => Some(LogicNodeType::Call),
        "await" | "await_expression" => Some(LogicNodeType::Await),
        "assignment" | "assignment_expression" | "augmented_assignment" => {
            Some(LogicNodeType::Assignment)
        }
        "raise_statement" | "throw_statement" => Some(LogicNodeType::Throw),
        "switch_statement" => Some(LogicNodeType::Switch),
        "switch_case" => Some(LogicNodeType::Case),
        _ => None,
    }
}

fn extract_definition_name(node: Node, src: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .map(ToString::to_string)
}

fn extract_function_signature(node: Node, src: &str, name: &str) -> Option<String> {
    let params = node
        .child_by_field_name("parameters")
        .or_else(|| node.child_by_field_name("formal_parameters"))
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .unwrap_or("()");
    let ret = node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .map(|t| format!(" {t}"))
        .unwrap_or_default();
    Some(format!("{name}{params}{ret}"))
}

fn extract_class_signature(node: Node, src: &str, name: &str) -> Option<String> {
    let bases = node
        .child_by_field_name("superclasses")
        .or_else(|| node.child_by_field_name("class_heritage"))
        .and_then(|n| n.utf8_text(src.as_bytes()).ok());
    match bases {
        Some(b) if !b.trim().is_empty() => Some(format!("{name}({b})")),
        _ => Some(name.to_string()),
    }
}

fn extract_call_name(node: Node, src: &str) -> Option<String> {
    node.child_by_field_name("function")
        .or_else(|| node.child(0))
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .map(ToString::to_string)
}

fn is_function_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_declaration"
            | "method_definition"
            | "generator_function_declaration"
            | "arrow_function"
    )
}

fn is_class_node(kind: &str) -> bool {
    matches!(kind, "class_definition" | "class_declaration")
}

fn is_import_node(kind: &str) -> bool {
    matches!(kind, "import_statement" | "import_from_statement" | "import_declaration")
}

fn is_call_node(kind: &str) -> bool {
    matches!(kind, "call" | "call_expression")
}

#[cfg(test)]
mod tests {
    use super::CodeParser;
    use engine::LogicNodeType;

    #[test]
    fn parses_python_function() {
        let mut parser = CodeParser::new();
        let src = "def retry_request():\n    return 1\n";
        let parsed = parser.parse("x.py", src).expect("parse should succeed");
        assert!(parsed.symbols.iter().any(|s| s.name == "retry_request"));
        assert!(parsed
            .logic_nodes
            .iter()
            .any(|n| n.node_type == LogicNodeType::Return));
    }

    #[test]
    fn extracts_logic_nodes_from_async_ts() {
        let mut parser = CodeParser::new();
        let src = "async function fetchData(token) {\n  if (!token) { throw new Error('missing') }\n  await refreshToken()\n  return request()\n}\n";
        let parsed = parser
            .parse("client.ts", src)
            .expect("parse should succeed");

        let kinds: Vec<LogicNodeType> = parsed.logic_nodes.into_iter().map(|n| n.node_type).collect();
        assert!(kinds.contains(&LogicNodeType::Conditional));
        assert!(kinds.contains(&LogicNodeType::Throw));
        assert!(kinds.contains(&LogicNodeType::Await));
        assert!(kinds.contains(&LogicNodeType::Return));
    }
}
