#!/usr/bin/env bash
#
# Retouch the source-of-truth version in place. This ONLY edits files — no git add/commit/tag/push,
# no release side effects. Committing, tagging, and releasing are the caller's job.
#
# "Source of truth" = the two files a human must change *before* a release is built:
#     Cargo.toml   (package version)
#     imbh.pc.in   (pkg-config Version)
#
# The packaging versions (vcpkg / conan / versions DB) are NOT touched here — they are filled from the
# published release archives afterwards by packaging/update-hashes.sh.
#
# Usage:
#     packaging/bump-version.sh <version>
#
# Example:
#     packaging/bump-version.sh 0.1.0
#     git add Cargo.toml imbh.pc.in && git commit -m "Bump to 0.1.0"   # then tag/push yourself
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

case "${1:-}" in
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    "") die "usage: $0 <version>   (see --help)" ;;
esac
VERSION="$1"
[[ $# -eq 1 ]] || die "unexpected argument '$2' — this script only retouches the version"

# Accept semver-ish: 1.2.3 with an optional -prerelease / .suffix.
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
    die "'$VERSION' is not a valid version (expected e.g. 0.1.0)"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Portable in-place sed (GNU + BSD).
sed_inplace() { sed -i.bak -E "$1" "$2" && rm -f "$2.bak"; }

# Only the [package] version line starts with `version = ` at column 0; dependency lines are indented
# and `rust-version = ` starts differently, so this rewrites exactly the package version.
sed_inplace "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$ROOT/Cargo.toml"
sed_inplace "s/^Version: .*/Version: $VERSION/" "$ROOT/imbh.pc.in"

echo "Cargo.toml : $(grep -m1 '^version = ' "$ROOT/Cargo.toml")"
echo "imbh.pc.in : $(grep -m1 '^Version: ' "$ROOT/imbh.pc.in")"
