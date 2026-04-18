use crate::parser::parse_rust_file;
use anyhow::Result;
use tree_sitter::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustSymbolKind {
    Struct,
    Enum,
    Trait,
    Function,
    Method,
    Module,
    ImplBlock,
}

#[derive(Debug, Clone)]
pub struct RustSymbol {
    pub name: String,
    pub kind: RustSymbolKind,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub summary: String,
    pub owner: Option<String>,
    pub trait_name: Option<String>,
    pub module_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RustImport {
    pub path: String,
    pub alias: Option<String>,
    pub is_glob: bool,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct RustModuleDecl {
    pub module_name: String,
    pub resolved_path: Option<String>,
    pub is_inline: bool,
    pub start_line: u32,
    pub end_line: u32,
}

pub fn extract_symbols(
    path: &str,
    source: &str,
) -> Result<(Vec<RustSymbol>, Vec<RustImport>, Vec<RustModuleDecl>)> {
    let tree = parse_rust_file(source)?;
    let root = tree.root_node();
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut modules = Vec::new();
    walk_node(
        root,
        source,
        path,
        &crate::resolver::module_scope_for_file(path),
        None,
        None,
        &mut symbols,
        &mut imports,
        &mut modules,
    );
    Ok((symbols, imports, modules))
}

fn walk_node(
    node: Node<'_>,
    source: &str,
    path: &str,
    module_scope: &[String],
    impl_owner: Option<String>,
    impl_trait: Option<String>,
    symbols: &mut Vec<RustSymbol>,
    imports: &mut Vec<RustImport>,
    modules: &mut Vec<RustModuleDecl>,
) {
    match node.kind() {
        "struct_item" => {
            push_named_symbol(node, source, path, module_scope, RustSymbolKind::Struct, symbols)
        }
        "enum_item" => {
            push_named_symbol(node, source, path, module_scope, RustSymbolKind::Enum, symbols)
        }
        "trait_item" => {
            let name = node_name(node, source);
            push_named_symbol(node, source, path, module_scope, RustSymbolKind::Trait, symbols);
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_node(
                    child,
                    source,
                    path,
                    module_scope,
                    name.clone(),
                    None,
                    symbols,
                    imports,
                    modules,
                );
            }
            return;
        }
        "function_item" => {
            push_function_symbol(
                node,
                source,
                path,
                module_scope,
                impl_owner.clone(),
                impl_trait.clone(),
                symbols,
            );
        }
        "mod_item" => {
            let module_name = node_name(node, source);
            push_named_symbol(node, source, path, module_scope, RustSymbolKind::Module, symbols);
            modules.push(extract_module_decl(node, source, path));
            if let Some(module_name) = module_name {
                let mut nested_scope = module_scope.to_vec();
                nested_scope.push(module_name);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(
                        child,
                        source,
                        path,
                        &nested_scope,
                        impl_owner.clone(),
                        impl_trait.clone(),
                        symbols,
                        imports,
                        modules,
                    );
                }
                return;
            }
        }
        "impl_item" => {
            let owner = impl_type_name(node, source);
            let trait_name = impl_trait_name(node, source);
            let impl_name = match (&trait_name, &owner) {
                (Some(trait_name), Some(owner)) => format!("impl {trait_name} for {owner}"),
                (None, Some(owner)) => format!("impl {owner}"),
                _ => "impl".to_string(),
            };
            symbols.push(RustSymbol {
                name: impl_name,
                kind: RustSymbolKind::ImplBlock,
                file: path.to_string(),
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                signature: Some(first_line(node_text(node, source))),
                summary: match (&trait_name, &owner) {
                    (Some(trait_name), Some(owner)) => {
                        format!("Rust impl block for trait {trait_name} on {owner}")
                    }
                    (None, Some(owner)) => format!("Rust inherent impl block for {owner}"),
                    _ => "Rust impl block".to_string(),
                },
                owner: owner.clone(),
                trait_name: trait_name.clone(),
                module_path: module_path_from_scope(module_scope),
            });
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_node(
                    child,
                    source,
                    path,
                    module_scope,
                    owner.clone(),
                    trait_name.clone(),
                    symbols,
                    imports,
                    modules,
                );
            }
            return;
        }
        "use_declaration" => {
            imports.push(extract_import(node, source));
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(
            child,
            source,
            path,
            module_scope,
            impl_owner.clone(),
            impl_trait.clone(),
            symbols,
            imports,
            modules,
        );
    }
}

fn push_named_symbol(
    node: Node<'_>,
    source: &str,
    path: &str,
    module_scope: &[String],
    kind: RustSymbolKind,
    symbols: &mut Vec<RustSymbol>,
) {
    let Some(name) = node_name(node, source) else {
        return;
    };
    let kind_label = match kind {
        RustSymbolKind::Struct => "struct",
        RustSymbolKind::Enum => "enum",
        RustSymbolKind::Trait => "trait",
        RustSymbolKind::Module => "module",
        RustSymbolKind::ImplBlock => "impl",
        RustSymbolKind::Function | RustSymbolKind::Method => "function",
    };
    symbols.push(RustSymbol {
        name: name.clone(),
        kind,
        file: path.to_string(),
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        signature: Some(first_line(node_text(node, source))),
        summary: format!("Rust {kind_label} {name}"),
        owner: None,
        trait_name: None,
        module_path: module_path_from_scope(module_scope),
    });
}

fn push_function_symbol(
    node: Node<'_>,
    source: &str,
    path: &str,
    module_scope: &[String],
    impl_owner: Option<String>,
    impl_trait: Option<String>,
    symbols: &mut Vec<RustSymbol>,
) {
    let Some(raw_name) = node_name(node, source) else {
        return;
    };
    let (name, kind, summary) = match impl_owner.as_deref() {
        Some(owner) => (
            format!("{owner}::{raw_name}"),
            RustSymbolKind::Method,
            if let Some(trait_name) = impl_trait.as_deref() {
                format!("Rust trait method {owner}::{raw_name} implementing {trait_name}")
            } else {
                format!("Rust method {owner}::{raw_name}")
            },
        ),
        None => (
            raw_name.clone(),
            RustSymbolKind::Function,
            format!("Rust function {raw_name}"),
        ),
    };
    symbols.push(RustSymbol {
        name,
        kind,
        file: path.to_string(),
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        signature: Some(first_line(node_text(node, source))),
        summary,
        owner: impl_owner,
        trait_name: impl_trait,
        module_path: module_path_from_scope(module_scope),
    });
}

fn module_path_from_scope(module_scope: &[String]) -> Option<String> {
    if module_scope.is_empty() {
        None
    } else {
        Some(module_scope.join("::"))
    }
}

fn extract_import(node: Node<'_>, source: &str) -> RustImport {
    let text = node_text(node, source);
    let path = text
        .trim()
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim()
        .to_string();
    let alias = path
        .rsplit_once(" as ")
        .map(|pair| pair.1.trim().to_string());
    let is_glob = path.ends_with("::*");
    RustImport {
        path,
        alias,
        is_glob,
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
    }
}

fn extract_module_decl(node: Node<'_>, source: &str, file: &str) -> RustModuleDecl {
    let module_name = node_name(node, source).unwrap_or_else(|| "mod".to_string());
    let text = node_text(node, source);
    let is_inline = text.contains('{');
    RustModuleDecl {
        module_name: module_name.clone(),
        resolved_path: if is_inline {
            None
        } else {
            crate::resolver::resolve_mod_declaration(file, &module_name)
                .into_iter()
                .next()
        },
        is_inline,
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
    }
}

fn node_name(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|child| child.utf8_text(source.as_bytes()).ok())
        .map(ToString::to_string)
}

fn impl_type_name(node: Node<'_>, source: &str) -> Option<String> {
    if let Some(type_node) = node.child_by_field_name("type") {
        return Some(clean_type_name(type_node.utf8_text(source.as_bytes()).ok()?));
    }
    impl_header_parts(node, source).map(|(_, owner)| owner)
}

fn impl_trait_name(node: Node<'_>, source: &str) -> Option<String> {
    if let Some(trait_node) = node.child_by_field_name("trait") {
        return Some(clean_type_name(trait_node.utf8_text(source.as_bytes()).ok()?));
    }
    impl_header_parts(node, source).and_then(|(trait_name, _)| trait_name)
}

fn impl_header_parts(node: Node<'_>, source: &str) -> Option<(Option<String>, String)> {
    let header = first_line(node_text(node, source));
    let header = header.trim().trim_start_matches("unsafe ").trim();
    let header = header.strip_prefix("impl")?.trim();
    let header = header.split(" where ").next().unwrap_or(header).trim();
    let header = header.trim_end_matches('{').trim();
    if let Some((trait_name, owner)) = header.split_once(" for ") {
        Some((
            Some(clean_type_name(trait_name)),
            clean_type_name(owner.trim()),
        ))
    } else {
        Some((None, clean_type_name(header)))
    }
}

fn clean_type_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_end_matches('{')
        .trim()
        .to_string()
}

fn node_text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    source.get(start..end).unwrap_or_default()
}

fn first_line(value: &str) -> String {
    value.lines().next().unwrap_or_default().trim().to_string()
}
