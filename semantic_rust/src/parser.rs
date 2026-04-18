use anyhow::{anyhow, Context, Result};
use tree_sitter::{Parser, Tree};

pub fn parse_rust_file(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::language())
        .context("failed to set tree-sitter rust language")?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("failed to parse rust source"))
}
