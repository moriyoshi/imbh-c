#!/usr/bin/env bash
#
# Finalize the vcpkg port and conan recipe against a *published* GitHub Release.
#
# The prebuilt-binary packages carry per-archive checksums that can only come from real published
# files. This script downloads the release assets for a tag, computes each archive's SHA512 (vcpkg)
# and SHA256 (conan), and rewrites the placeholder "0" values plus the version strings in:
#
#     ports/imbh-c/vcpkg.json        (version)
#     ports/imbh-c/portfile.cmake    (IMBH_C_VERSION + _sha512_<target>)
#     packaging/conan/conanfile.py   (version + _sha256[<target>])
#     versions/baseline.json         (baseline)
#
# Usage:  packaging/update-hashes.sh v0.1.0
#
# After it runs, review the diff, commit, and register the vcpkg version:
#     packaging/vcpkg-add-version.sh          # runs `vcpkg x-add-version imbh-c`
#
# Requirements: gh (or curl) and sha512sum/sha256sum (or shasum).
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <tag>   e.g. $0 v0.1.0" >&2
    exit 2
fi

TAG="$1"
VERSION="${TAG#v}"
REPO="moriyoshi/imbh-c"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORTFILE="${ROOT}/ports/imbh-c/portfile.cmake"
VCPKG_JSON="${ROOT}/ports/imbh-c/vcpkg.json"
CONANFILE="${ROOT}/packaging/conan/conanfile.py"
BASELINE="${ROOT}/versions/baseline.json"
WORK="${ROOT}/.agents-workspace/tmp/imbh-c-hashes"

# The (rust target, archive extension) pairs the packages actually consume. The release also builds
# musl variants; the vcpkg/conan recipes do not use them, so they are intentionally not hashed here.
TARGETS=(
    "x86_64-unknown-linux-gnu:tar.gz"
    "aarch64-unknown-linux-gnu:tar.gz"
    "x86_64-apple-darwin:tar.gz"
    "aarch64-apple-darwin:tar.gz"
    "x86_64-pc-windows-msvc:zip"
)

sha() { # $1=algo(512|256) $2=file
    if command -v "sha${1}sum" >/dev/null 2>&1; then
        "sha${1}sum" "$2" | awk '{print $1}'
    else
        shasum -a "$1" "$2" | awk '{print $1}'
    fi
}

# In-place sed that works on both GNU and BSD sed.
sed_inplace() { sed -i.bak -E "$1" "$2" && rm -f "$2.bak"; }

rm -rf "$WORK"
mkdir -p "$WORK"

echo ">> Finalizing imbh-c packaging for ${TAG} (version ${VERSION})"

# --- Version strings ------------------------------------------------------------------------------
sed_inplace "s#(\"version\"[[:space:]]*:[[:space:]]*\")[^\"]*(\")#\\1${VERSION}\\2#" "$VCPKG_JSON"
sed_inplace "s#(\"baseline\"[[:space:]]*:[[:space:]]*\")[^\"]*(\")#\\1${VERSION}\\2#" "$BASELINE"
sed_inplace "s#(set\\(IMBH_C_VERSION[[:space:]]+\")[^\"]*(\"\\))#\\1${VERSION}\\2#" "$PORTFILE"
sed_inplace "s#(^[[:space:]]*version[[:space:]]*=[[:space:]]*\")[^\"]*(\")#\\1${VERSION}\\2#" "$CONANFILE"

# --- Per-target checksums -------------------------------------------------------------------------
for entry in "${TARGETS[@]}"; do
    target="${entry%%:*}"
    ext="${entry##*:}"
    asset="imbh-c-${VERSION}-${target}.${ext}"
    dest="${WORK}/${asset}"

    echo ">> Downloading ${asset}"
    if command -v gh >/dev/null 2>&1; then
        gh release download "$TAG" --repo "$REPO" --pattern "$asset" --dir "$WORK" --clobber
    else
        curl -fsSL -o "$dest" \
            "https://github.com/${REPO}/releases/download/${TAG}/${asset}"
    fi

    s512="$(sha 512 "$dest")"
    s256="$(sha 256 "$dest")"
    echo "   sha512 ${s512:0:16}...  sha256 ${s256:0:16}..."

    sed_inplace "s#(set\\(_sha512_${target}[[:space:]]+\")[0-9a-fA-F]*(\"\\))#\\1${s512}\\2#" "$PORTFILE"
    sed_inplace "s#(\"${target}\"[[:space:]]*:[[:space:]]*\")[0-9a-fA-F]*(\")#\\1${s256}\\2#" "$CONANFILE"
done

echo ">> Done. Review 'git diff', commit, then run packaging/vcpkg-add-version.sh"
