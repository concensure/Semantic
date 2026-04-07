#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
INSTALL_ROOT="${1:-${CARGO_HOME:-$HOME/.cargo}}"

echo "Installing Semantic CLI to ${INSTALL_ROOT}"
cd "${REPO_ROOT}"

cargo install \
  --path semantic_cli \
  --locked \
  --root "${INSTALL_ROOT}" \
  --bin semantic

echo
echo "Installed binary:"
echo "  ${INSTALL_ROOT}/bin/semantic"
echo
echo "Try it on another repo:"
echo "  semantic --repo /path/to/project status"
echo "  semantic --repo /path/to/project route --task \"explain auth flow\""
