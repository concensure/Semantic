pub mod cargo;
pub mod extractor;
pub mod graph;
pub mod parser;
pub mod resolver;
pub mod retrieval;
pub mod rust_analyzer;

use anyhow::Result;
use engine::{ParsedFile, RustSymbolMetadataRecord};

pub use cargo::{CargoCrateInfo, CargoWorkspaceInfo};
pub use extractor::{RustImport, RustModuleDecl, RustSymbol, RustSymbolKind};
pub use retrieval::{
    extract_import_records, extract_module_decl_records, RustContextBundle, RustSearchMatch,
};
pub use rust_analyzer::{document_symbol_search, workspace_symbol_search, RustAnalyzerSymbol};

pub fn parse_to_engine(path: &str, source: &str) -> Result<ParsedFile> {
    retrieval::parse_to_engine(path, source)
}

pub fn extract_metadata(path: &str, source: &str) -> Result<Vec<RustSymbolMetadataRecord>> {
    retrieval::extract_metadata(path, source)
}

#[cfg(test)]
mod tests {
    use super::{document_symbol_search, extract_metadata, parse_to_engine};
    use crate::resolver::{module_path_for_file, module_scope_for_file};
    use std::fs;

    #[test]
    fn parses_rust_symbols_and_impls_into_engine_records() {
        let source = r#"
pub struct User;

impl User {
    pub fn new() -> Self {
        Self
    }
}
"#;
        let parsed = parse_to_engine("src/user.rs", source).expect("rust parse");
        assert_eq!(parsed.language, "rust");
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == "User"));
        assert!(parsed
            .symbols
            .iter()
            .any(|symbol| symbol.name == "impl User"));
        assert!(parsed
            .symbols
            .iter()
            .any(|symbol| symbol.name == "User::new"));
        assert!(parsed
            .dependencies
            .iter()
            .any(|dep| dep.caller_symbol == "User" && dep.callee_symbol == "impl User"));
    }

    #[test]
    fn module_scope_skips_source_root_segments() {
        assert_eq!(module_scope_for_file("src/api.rs"), vec!["api".to_string()]);
        assert_eq!(
            module_scope_for_file("worker/src/models/user.rs"),
            vec!["worker".to_string(), "models".to_string(), "user".to_string()]
        );
        assert_eq!(module_path_for_file("src/lib.rs"), "");
    }

    #[test]
    fn metadata_tracks_inline_module_scope_per_symbol() {
        let source = r#"
pub mod api {
    pub trait Serialize {
        fn encode(&self) -> &'static str;
    }
}

pub mod domain {
    pub trait Serialize {
        fn save(&self) -> &'static str;
    }
}
"#;
        let metadata = extract_metadata("src/lib.rs", source).expect("metadata");
        let api_trait = metadata
            .iter()
            .find(|item| item.symbol_name == "Serialize" && item.module_path.as_deref() == Some("api"))
            .expect("api trait");
        assert_eq!(api_trait.kind, "trait");
        let domain_trait = metadata
            .iter()
            .find(|item| item.symbol_name == "Serialize" && item.module_path.as_deref() == Some("domain"))
            .expect("domain trait");
        assert_eq!(domain_trait.kind, "trait");
    }

    #[test]
    fn document_symbol_search_reads_symbols_from_temp_repo() {
        if !crate::rust_analyzer::is_available() {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"mini\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write cargo");
        fs::write(
            tmp.path().join("src").join("lib.rs"),
            "pub struct Foo;\nimpl Foo { pub fn new() -> Self { Self } }\n",
        )
        .expect("write rust");
        let symbols = document_symbol_search(tmp.path(), "src/lib.rs").expect("document symbols");
        assert!(symbols.iter().any(|symbol| symbol.name == "Foo"));
    }
}
