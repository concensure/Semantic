use anyhow::{anyhow, Result};
use engine::{ASTEdit, ASTTransformation, CodePatch, PatchRepresentation};

pub struct PatchEngine;

impl PatchEngine {
    pub fn generate_ast_patch(file_path: &str, target_symbol: &str, transformation: ASTTransformation) -> CodePatch {
        CodePatch {
            file_path: file_path.to_string(),
            representation: PatchRepresentation::ASTTransform(ASTEdit {
                target_symbol: target_symbol.to_string(),
                transformation,
            }),
        }
    }

    pub fn ast_to_diff(file_path: &str, ast_edit: &ASTEdit) -> String {
        format!(
            "--- {file}\n+++ {file}\n@@ AST_EDIT @@\n# symbol: {symbol}\n# transformation: {tx:?}\n",
            file = file_path,
            symbol = ast_edit.target_symbol,
            tx = ast_edit.transformation
        )
    }

    pub fn validate_patch(file_path: &str, patch: &CodePatch, existing_code: &str) -> Result<()> {
        let replacement = match &patch.representation {
            PatchRepresentation::UnifiedDiff(diff) => diff.clone(),
            PatchRepresentation::ASTTransform(ast_edit) => Self::ast_to_diff(file_path, ast_edit),
        };

        if replacement.trim().is_empty() {
            return Err(anyhow!("empty patch"));
        }

        let mut parser = parser::CodeParser::new();
        let parse_target = if existing_code.is_empty() { "\n" } else { existing_code };
        let _ = parser.parse(file_path, parse_target)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PatchEngine;
    use engine::{ASTTransformation, PatchRepresentation};

    #[test]
    fn generates_ast_patch_and_preview_diff() {
        let patch = PatchEngine::generate_ast_patch(
            "src/client.ts",
            "retryRequest",
            ASTTransformation::ReplaceFunctionBody,
        );
        match &patch.representation {
            PatchRepresentation::ASTTransform(edit) => {
                let diff = PatchEngine::ast_to_diff(&patch.file_path, edit);
                assert!(diff.contains("retryRequest"));
                assert!(diff.contains("AST_EDIT"));
            }
            PatchRepresentation::UnifiedDiff(_) => panic!("expected ast transform"),
        }
    }
}
