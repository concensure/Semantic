use anyhow::{anyhow, Context, Result};
use ast_cache::AstCache;
use engine::{
    DependencyRecord, LogicNodeRecord, LogicNodeType, ParsedFile, SymbolRecord, SymbolType,
};
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

        Ok(ParsedFile {
            file: path.to_string(),
            language: lang.name().to_string(),
            symbols,
            dependencies,
            logic_nodes,
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
            });
            let symbol_ref = symbols.len() as i64;
            collect_call_edges(node, src, file, &name, deps);
            collect_logic_nodes_in_symbol(node, symbol_ref, logic_nodes);
        }
    } else if is_class_node(kind) {
        if let Some(name) = extract_definition_name(node, src) {
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

fn collect_logic_nodes_in_symbol(root: Node, symbol_ref: i64, logic_nodes: &mut Vec<LogicNodeRecord>) {
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
            });
        }

        let mut cursor = next.walk();
        for child in next.children(&mut cursor) {
            stack.push(child);
        }
    }
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
