use anyhow::Result;
use engine::SymbolRecord;

pub struct SemanticSearcher;

impl SemanticSearcher {
    pub fn search(storage: &storage::Storage, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        let mut symbols = storage.list_symbols()?;
        symbols.sort_by(|a, b| {
            let sa = symbol_similarity::similarity_score(query, &a.name);
            let sb = symbol_similarity::similarity_score(query, &b.name);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        symbols.truncate(limit);
        Ok(symbols)
    }
}
