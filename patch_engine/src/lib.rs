use anyhow::{anyhow, bail, Result};
use engine::{ASTEdit, ASTTransformation, CodePatch, PatchRepresentation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start_line: usize,
    pub end_line: usize,
}

pub struct PatchEngine;

impl PatchEngine {
    pub fn generate_ast_patch(
        file_path: &str,
        target_symbol: &str,
        transformation: ASTTransformation,
    ) -> CodePatch {
        CodePatch {
            file_path: file_path.to_string(),
            representation: PatchRepresentation::ASTTransform(ASTEdit {
                target_symbol: target_symbol.to_string(),
                transformation,
            }),
        }
    }

    pub fn generate_replacement_patch(
        file_path: &str,
        existing_code: &str,
        replacement_range: LineRange,
        replacement_code: &str,
    ) -> Result<CodePatch> {
        let updated_code =
            apply_line_replacement(existing_code, replacement_range, replacement_code)?;
        let diff = build_unified_diff(file_path, existing_code, &updated_code);
        if diff.trim().is_empty() {
            bail!("replacement did not change file content");
        }
        Ok(CodePatch {
            file_path: file_path.to_string(),
            representation: PatchRepresentation::UnifiedDiff(diff),
        })
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
        let updated = match &patch.representation {
            PatchRepresentation::UnifiedDiff(diff) => apply_unified_diff(existing_code, diff)?,
            PatchRepresentation::ASTTransform(ast_edit) => {
                bail!(
                    "cannot validate abstract AST transform for '{}' without a concrete rewrite",
                    ast_edit.target_symbol
                );
            }
        };

        if updated == existing_code {
            bail!("patch does not modify file content");
        }

        let mut parser = parser::CodeParser::new();
        let _ = parser.parse(file_path, &updated)?;
        Ok(())
    }

    pub fn apply_patch(existing_code: &str, patch: &CodePatch) -> Result<String> {
        match &patch.representation {
            PatchRepresentation::UnifiedDiff(diff) => apply_unified_diff(existing_code, diff),
            PatchRepresentation::ASTTransform(ast_edit) => bail!(
                "cannot apply abstract AST transform for '{}' without a concrete rewrite",
                ast_edit.target_symbol
            ),
        }
    }
}

fn apply_line_replacement(
    existing_code: &str,
    replacement_range: LineRange,
    replacement_code: &str,
) -> Result<String> {
    if replacement_range.start_line == 0
        || replacement_range.end_line < replacement_range.start_line
    {
        bail!(
            "invalid replacement range {}..{}",
            replacement_range.start_line,
            replacement_range.end_line
        );
    }

    let mut lines: Vec<&str> = existing_code.split('\n').collect();
    let had_trailing_newline = existing_code.ends_with('\n');
    if had_trailing_newline && lines.last() == Some(&"") {
        lines.pop();
    }

    let line_count = lines.len();
    let normalized = replacement_code.replace("\r\n", "\n");
    if line_count == 0 && replacement_range.start_line == 1 && replacement_range.end_line == 1 {
        let mut updated = normalized;
        if had_trailing_newline && !updated.ends_with('\n') {
            updated.push('\n');
        }
        return Ok(updated);
    }

    if replacement_range.end_line > line_count {
        bail!(
            "replacement range {}..{} exceeds file length {}",
            replacement_range.start_line,
            replacement_range.end_line,
            line_count
        );
    }

    let start_idx = replacement_range.start_line - 1;
    let end_idx = replacement_range.end_line;
    let replacement_lines = split_lines_preserving_empty(&normalized);

    let mut out = Vec::new();
    out.extend_from_slice(&lines[..start_idx]);
    out.extend(replacement_lines.iter().map(|line| line.as_str()));
    out.extend_from_slice(&lines[end_idx..]);

    let mut updated = out.join("\n");
    if had_trailing_newline {
        updated.push('\n');
    }
    Ok(updated)
}

fn build_unified_diff(file_path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }

    let old_lines = split_lines_preserving_empty(old);
    let new_lines = split_lines_preserving_empty(new);

    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_changed_end = old_lines.len().saturating_sub(suffix);
    let new_changed_end = new_lines.len().saturating_sub(suffix);
    let old_changed = &old_lines[prefix..old_changed_end];
    let new_changed = &new_lines[prefix..new_changed_end];

    let old_start = if old_changed.is_empty() {
        prefix
    } else {
        prefix + 1
    };
    let new_start = if new_changed.is_empty() {
        prefix
    } else {
        prefix + 1
    };

    let mut diff = format!(
        "--- {file}\n+++ {file}\n@@ -{old_start},{old_len} +{new_start},{new_len} @@\n",
        file = file_path,
        old_start = old_start,
        old_len = old_changed.len(),
        new_start = new_start,
        new_len = new_changed.len()
    );

    for line in old_changed {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in new_changed {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn apply_unified_diff(existing_code: &str, diff: &str) -> Result<String> {
    let original_lines = split_lines_preserving_empty(existing_code);
    let mut out = Vec::new();
    let mut current_original_index = 0usize;
    let mut in_hunk = false;

    for raw_line in diff.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }
        if let Some((old_start, _old_len, _new_start, _new_len)) = parse_hunk_header(raw_line)? {
            let target_index = old_start.saturating_sub(1);
            if target_index > original_lines.len() {
                bail!("hunk starts past end of file");
            }
            out.extend_from_slice(&original_lines[current_original_index..target_index]);
            current_original_index = target_index;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (prefix, content) = raw_line
            .chars()
            .next()
            .map(|ch| (ch, &raw_line[ch.len_utf8()..]))
            .ok_or_else(|| anyhow!("malformed diff line"))?;
        match prefix {
            ' ' => {
                let original = original_lines
                    .get(current_original_index)
                    .ok_or_else(|| anyhow!("context line exceeds original file"))?;
                if original != &content {
                    bail!("context mismatch while applying patch");
                }
                out.push(content.to_string());
                current_original_index += 1;
            }
            '-' => {
                let original = original_lines
                    .get(current_original_index)
                    .ok_or_else(|| anyhow!("deletion exceeds original file"))?;
                if original != &content {
                    bail!("deletion mismatch while applying patch");
                }
                current_original_index += 1;
            }
            '+' => out.push(content.to_string()),
            _ => bail!("unsupported diff line prefix '{prefix}'"),
        }
    }

    out.extend_from_slice(&original_lines[current_original_index..]);
    Ok(join_lines_like_source(existing_code, &out))
}

fn parse_hunk_header(line: &str) -> Result<Option<(usize, usize, usize, usize)>> {
    if !line.starts_with("@@ ") || !line.ends_with(" @@") {
        return Ok(None);
    }
    let body = &line[3..line.len() - 3];
    let mut parts = body.split_whitespace();
    let old_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing old hunk range"))?;
    let new_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing new hunk range"))?;
    let (old_start, old_len) = parse_hunk_range(old_part, '-')?;
    let (new_start, new_len) = parse_hunk_range(new_part, '+')?;
    Ok(Some((old_start, old_len, new_start, new_len)))
}

fn parse_hunk_range(value: &str, prefix: char) -> Result<(usize, usize)> {
    let trimmed = value
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow!("missing hunk prefix '{prefix}'"))?;
    let mut parts = trimmed.splitn(2, ',');
    let start = parts
        .next()
        .ok_or_else(|| anyhow!("missing hunk start"))?
        .parse::<usize>()?;
    let len = parts
        .next()
        .ok_or_else(|| anyhow!("missing hunk length"))?
        .parse::<usize>()?;
    Ok((start, len))
}

fn split_lines_preserving_empty(input: &str) -> Vec<String> {
    let normalized = input.replace("\r\n", "\n");
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = normalized
        .split('\n')
        .map(|line| line.to_string())
        .collect();
    if normalized.ends_with('\n') && lines.last().map(|line| line.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

fn join_lines_like_source(original: &str, lines: &[String]) -> String {
    let mut out = lines.join("\n");
    if original.ends_with('\n') || lines.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{LineRange, PatchEngine};
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

    #[test]
    fn replacement_patch_round_trips_and_validates() {
        let old = "function retryRequest(){\n  return 1;\n}\n";
        let new_symbol = "function retryRequest(){\n  return 2;\n}\n";
        let patch = PatchEngine::generate_replacement_patch(
            "src/client.ts",
            old,
            LineRange {
                start_line: 1,
                end_line: 3,
            },
            new_symbol,
        )
        .expect("patch");
        let applied = PatchEngine::apply_patch(old, &patch).expect("apply");
        assert_eq!(applied, new_symbol);
        PatchEngine::validate_patch("src/client.ts", &patch, old).expect("validate");
    }

    #[test]
    fn validate_rejects_abstract_ast_edits() {
        let patch = PatchEngine::generate_ast_patch(
            "src/client.ts",
            "retryRequest",
            ASTTransformation::ReplaceFunctionBody,
        );
        let err =
            PatchEngine::validate_patch("src/client.ts", &patch, "function retryRequest(){}\n")
                .expect_err("validation should fail");
        assert!(err.to_string().contains("abstract AST transform"));
    }
}
