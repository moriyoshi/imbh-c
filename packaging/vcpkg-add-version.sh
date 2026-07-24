#!/usr/bin/env bash
#
# Register the current imbh-c port in the in-repo vcpkg versions database.
#
# vcpkg identifies each port version by the git-tree hash of ports/imbh-c/ at a committed state, so
# this must run *after* the port files (including the checksums filled by update-hashes.sh) are
# committed. It rewrites versions/baseline.json and versions/i-/imbh-c.json with the real git-tree.
#
# Usage:  packaging/vcpkg-add-version.sh
#
# Requires: vcpkg on PATH (or set VCPKG_ROOT).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

VCPKG_BIN="${VCPKG_ROOT:+$VCPKG_ROOT/vcpkg}"
VCPKG_BIN="${VCPKG_BIN:-$(command -v vcpkg || true)}"
if [[ -z "$VCPKG_BIN" || ! -x "$VCPKG_BIN" ]]; then
    echo "error: vcpkg not found. Install it or set VCPKG_ROOT." >&2
    exit 1
fi

if [[ -n "$(git -C "$ROOT" status --porcelain -- ports/imbh-c versions)" ]]; then
    echo "error: commit the port changes first — x-add-version hashes the committed git tree." >&2
    git -C "$ROOT" status --short -- ports/imbh-c versions >&2
    exit 1
fi

# --overwrite-version replaces the scaffolded placeholder git-tree on the first run.
"$VCPKG_BIN" --x-builtin-ports-root="${ROOT}/ports" \
             --x-builtin-registry-versions-dir="${ROOT}/versions" \
             x-add-version imbh-c --overwrite-version

echo ">> versions/ updated. Review 'git diff' and commit."
