# Third-Party Notices

This repository includes optional integrations or compatibility code that may interoperate with
third-party tools.

## rust-analyzer

- Project: `rust-analyzer`
- Upstream: https://github.com/rust-lang/rust-analyzer
- License: MIT

Current usage in this repository:

- Optional Rust retrieval support may invoke a locally installed `rust-analyzer` binary as an
  internal semantic backend.
- This integration is used only to improve Rust symbol anchoring and grouped retrieval quality.
- The public Semantic tool surface remains unchanged: Rust retrieval still flows through the
  existing `search_rust_symbol` and `get_rust_context` operations.

This notice is informational and does not replace the upstream license text. When redistributing
`rust-analyzer` itself, follow the upstream project's license terms.
