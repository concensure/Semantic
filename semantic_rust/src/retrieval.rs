use crate::cargo::{discover_workspace, CargoWorkspaceInfo};
use crate::extractor::{extract_symbols, RustModuleDecl, RustSymbol, RustSymbolKind};
use crate::graph::build_relationships;
use crate::resolver::module_path_for_file;
use anyhow::Result;
use engine::{
    ParsedFile, RustImportRecord, RustModuleDeclRecord, RustSymbolMetadataRecord, SymbolRecord,
    SymbolType,
};
use serde::Serialize;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
pub struct RustSearchMatch {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub summary: String,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RustContextBundle {
    pub symbol: String,
    pub kind: String,
    pub definitions: Vec<RustSearchMatch>,
    pub impl_blocks: Vec<RustSearchMatch>,
    pub associated_items: Vec<RustSearchMatch>,
    pub modules: Vec<String>,
    pub cargo: CargoWorkspaceInfo,
}

pub fn parse_to_engine(path: &str, source: &str) -> Result<ParsedFile> {
    let (symbols, imports, _) = extract_symbols(path, source)?;
    let mut engine_symbols = symbols
        .iter()
        .map(to_engine_symbol)
        .collect::<Vec<_>>();
    engine_symbols.extend(imports.into_iter().enumerate().map(|(idx, import)| SymbolRecord {
        id: None,
        repo_id: 0,
        name: import
            .alias
            .clone()
            .unwrap_or_else(|| format!("use@{}:{idx}", import.start_line)),
        symbol_type: SymbolType::Import,
        file: path.to_string(),
        start_line: import.start_line,
        end_line: import.end_line,
        language: "rust".to_string(),
        summary: format!("Rust import {}", import.path),
        signature: Some(import.path),
    }));
    let dependencies = build_relationships(path, &symbols);
    Ok(ParsedFile {
        file: path.to_string(),
        language: "rust".to_string(),
        symbols: engine_symbols,
        dependencies,
        logic_nodes: Vec::new(),
        control_flow_edges: Vec::new(),
        data_flow_edges: Vec::new(),
        logic_clusters: Vec::new(),
    })
}

pub fn extract_metadata(path: &str, source: &str) -> Result<Vec<RustSymbolMetadataRecord>> {
    let (symbols, _, _) = extract_symbols(path, source)?;
    Ok(symbols
        .into_iter()
        .map(|symbol| RustSymbolMetadataRecord {
            symbol_name: symbol.name,
            file: symbol.file,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            kind: rust_kind_label(&symbol.kind).to_string(),
            owner_name: symbol.owner,
            trait_name: symbol.trait_name,
            module_path: symbol.module_path,
            crate_name: None,
            crate_root: None,
        })
        .collect())
}

pub fn extract_import_records(path: &str, source: &str) -> Result<Vec<RustImportRecord>> {
    let (_, imports, _) = extract_symbols(path, source)?;
    Ok(imports
        .into_iter()
        .map(|import| RustImportRecord {
            file: path.to_string(),
            path: import.path,
            alias: import.alias,
            is_glob: import.is_glob,
            start_line: import.start_line,
            end_line: import.end_line,
            crate_name: None,
        })
        .collect())
}

pub fn extract_module_decl_records(path: &str, source: &str) -> Result<Vec<RustModuleDeclRecord>> {
    let (_, _, modules): (Vec<RustSymbol>, Vec<crate::extractor::RustImport>, Vec<RustModuleDecl>) =
        extract_symbols(path, source)?;
    Ok(modules
        .into_iter()
        .map(|module| RustModuleDeclRecord {
            file: path.to_string(),
            module_name: module.module_name,
            resolved_path: module.resolved_path,
            is_inline: module.is_inline,
            start_line: module.start_line,
            end_line: module.end_line,
            crate_name: None,
        })
        .collect())
}

pub fn search_symbol(repo_root: &Path, query: &str, limit: usize) -> Result<Vec<RustSearchMatch>> {
    let mut matches = collect_repo_symbols(repo_root)?
        .into_iter()
        .filter(|symbol| rust_symbol_matches(symbol, query))
        .map(|symbol| to_search_match(&symbol))
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.file.cmp(&b.file)));
    matches.truncate(limit);
    Ok(matches)
}

pub fn get_context(repo_root: &Path, query: &str, max_items: usize) -> Result<RustContextBundle> {
    let symbols = collect_repo_symbols(repo_root)?;
    let normalized = query.trim();
    let definitions = symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.kind,
                RustSymbolKind::Struct | RustSymbolKind::Enum | RustSymbolKind::Trait | RustSymbolKind::Function
            ) && rust_symbol_matches(symbol, normalized)
        })
        .take(max_items)
        .map(to_search_match)
        .collect::<Vec<_>>();
    let impl_blocks = symbols
        .iter()
        .filter(|symbol| symbol.kind == RustSymbolKind::ImplBlock)
        .filter(|symbol| {
            symbol.owner.as_deref() == Some(normalized)
                || symbol.trait_name.as_deref() == Some(normalized)
                || symbol.name.contains(normalized)
        })
        .take(max_items)
        .map(to_search_match)
        .collect::<Vec<_>>();
    let associated_items = symbols
        .iter()
        .filter(|symbol| symbol.kind == RustSymbolKind::Method)
        .filter(|symbol| {
            symbol.owner.as_deref() == Some(normalized)
                || symbol.trait_name.as_deref() == Some(normalized)
                || symbol.name.contains(&format!("{normalized}::"))
        })
        .take(max_items)
        .map(to_search_match)
        .collect::<Vec<_>>();
    let mut modules = symbols
        .iter()
        .filter(|symbol| rust_symbol_matches(symbol, normalized))
        .map(|symbol| module_path_for_file(&symbol.file))
        .filter(|module| !module.is_empty())
        .collect::<Vec<_>>();
    modules.sort();
    modules.dedup();
    Ok(RustContextBundle {
        symbol: normalized.to_string(),
        kind: infer_bundle_kind(&definitions, &impl_blocks),
        definitions,
        impl_blocks,
        associated_items,
        modules,
        cargo: discover_workspace(repo_root),
    })
}

fn collect_repo_symbols(repo_root: &Path) -> Result<Vec<RustSymbol>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(repo_root).into_iter().filter_map(|entry| entry.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(repo_root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let source = fs::read_to_string(path)?;
        let (symbols, _, _) = extract_symbols(&relative, &source)?;
        out.extend(symbols);
    }
    Ok(out)
}

fn rust_symbol_matches(symbol: &RustSymbol, query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    let name = symbol.name.to_ascii_lowercase();
    let owner = symbol.owner.as_deref().unwrap_or_default().to_ascii_lowercase();
    let trait_name = symbol
        .trait_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let module_path = symbol
        .module_path
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == query
        || name.contains(&query)
        || owner == query
        || trait_name == query
        || module_path == query
        || module_path.ends_with(&format!("::{query}"))
        || module_path.contains(&query)
}

fn to_engine_symbol(symbol: &RustSymbol) -> SymbolRecord {
    SymbolRecord {
        id: None,
        repo_id: 0,
        name: symbol.name.clone(),
        symbol_type: match symbol.kind {
            RustSymbolKind::Function | RustSymbolKind::Method => SymbolType::Function,
            RustSymbolKind::Struct
            | RustSymbolKind::Enum
            | RustSymbolKind::Trait
            | RustSymbolKind::Module
            | RustSymbolKind::ImplBlock => SymbolType::Class,
        },
        file: symbol.file.clone(),
        start_line: symbol.start_line,
        end_line: symbol.end_line,
        language: "rust".to_string(),
        summary: symbol.summary.clone(),
        signature: symbol.signature.clone(),
    }
}

fn to_search_match(symbol: &RustSymbol) -> RustSearchMatch {
    RustSearchMatch {
        name: symbol.name.clone(),
        kind: rust_kind_label(&symbol.kind).to_string(),
        file: symbol.file.clone(),
        start_line: symbol.start_line,
        end_line: symbol.end_line,
        summary: symbol.summary.clone(),
        signature: symbol.signature.clone(),
    }
}

fn rust_kind_label(kind: &RustSymbolKind) -> &'static str {
    match kind {
        RustSymbolKind::Struct => "struct",
        RustSymbolKind::Enum => "enum",
        RustSymbolKind::Trait => "trait",
        RustSymbolKind::Function => "function",
        RustSymbolKind::Method => "method",
        RustSymbolKind::Module => "module",
        RustSymbolKind::ImplBlock => "impl_block",
    }
}

fn infer_bundle_kind(
    definitions: &[RustSearchMatch],
    impl_blocks: &[RustSearchMatch],
) -> String {
    definitions
        .first()
        .map(|item| item.kind.clone())
        .or_else(|| impl_blocks.first().map(|item| item.kind.clone()))
        .unwrap_or_else(|| "unknown".to_string())
}
