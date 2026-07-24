# Packaging imbh-c for vcpkg and Conan

imbh-c ships as **prebuilt, per-target archives** attached to each GitHub Release by
[`.github/workflows/release.yml`](../.github/workflows/release.yml). The vcpkg port and the Conan
recipe here both *download* the archive matching the consumer's platform and install its headers +
library — no Rust toolchain is required to consume the binding. Both expose the identical CMake
target `imbh-c::imbh-c`, so downstream `CMakeLists.txt` is the same regardless of which manager
delivered it:

```cmake
find_package(imbh-c CONFIG REQUIRED)
target_link_libraries(app PRIVATE imbh-c::imbh-c)
```

Each release archive (`imbh-c-<version>-<rust-target>.tar.gz`, `.zip` on Windows) contains both a
self-contained shared library and a static `staticlib`; the packages pick the flavor for the
requested linkage. The shared library needs nothing else at link time; the static archive pulls in
the platform's Rust-std system libraries, which both packages attach to the imported target.

Supported targets: Linux and macOS on x86_64 + arm64, Windows (MSVC) on x86_64.

## Layout

| Path | What |
|------|------|
| `../ports/imbh-c/` | vcpkg port (`vcpkg.json`, `portfile.cmake`, config template, `usage`) |
| `../versions/` | vcpkg versions database making this repo a **git registry** |
| `conan/conanfile.py` | Conan 2.x recipe |
| `conan/test_package/` | Conan test package (build + run a smoke test) |
| `vcpkg-example/` | a standalone consumer using the git registry in manifest mode |
| `bump-version.sh` | retouch the version in Cargo.toml + imbh.pc.in (edits only — no git) |
| `update-hashes.sh` | fill archive checksums + version from a published release |
| `vcpkg-add-version.sh` | register the port version in `../versions/` |

## Releasing

The end-to-end release process — bump → build & publish → finalize — is documented in the root
[README.md](../README.md#releasing). In short: a maintainer bumps + tags locally (which triggers
`release.yml`), then runs `update-hashes.sh` + `vcpkg-add-version.sh` and commits the results, signing
every commit themselves. No CI job writes to this repo, so there is no unsigned-bot-commit problem to
solve.

Until the finalize phase runs, the port and recipe carry the sentinel `"0"` checksums and both
managers refuse to install by design (vcpkg errors on the download hash; Conan raises
`ConanInvalidConfiguration`).

## Consuming via vcpkg

This repo is a vcpkg **git registry** (`ports/` + `versions/` at the root). A consumer adds it in
`vcpkg-configuration.json` and lists `imbh-c` as a dependency — see
[`vcpkg-example/`](vcpkg-example/):

```jsonc
// vcpkg-configuration.json
{
  "default-registry": { "kind": "git", "repository": "https://github.com/microsoft/vcpkg",
                         "baseline": "<a vcpkg commit sha>" },
  "registries": [
    { "kind": "git", "repository": "https://github.com/moriyoshi/imbh-c",
      "baseline": "<an imbh-c commit sha>", "packages": ["imbh-c"] }
  ]
}
```

```jsonc
// vcpkg.json
{ "name": "my-app", "version": "0.0.0", "dependencies": ["imbh-c"] }
```

```sh
cmake -S . -B build -DCMAKE_TOOLCHAIN_FILE=$VCPKG_ROOT/scripts/buildsystems/vcpkg.cmake
cmake --build build
```

Or, without a registry, as an **overlay port**:

```sh
vcpkg install imbh-c --overlay-ports=/path/to/imbh-c/ports
```

## Consuming via Conan

Export the recipe to your local cache, then require it. There is no Conan Center listing; the recipe
lives in this repo.

```sh
# Export + build/run the test package to verify the recipe end-to-end.
conan create packaging/conan --version 0.1.0

# In a consumer project's conanfile.txt / conanfile.py:
#   [requires]
#   imbh-c/0.1.0
conan install . --build=missing
```

The recipe defaults to the shared library (`-o imbh-c/*:shared=True`); pass `shared=False` for the
static archive (which then also links the Rust-std system libraries declared by the recipe).

## Notes & limits

- **musl** Linux archives are built by the release workflow but are **not** consumed by these
  packages; `update-hashes.sh` only hashes the gnu/darwin/msvc targets the recipes use.
- The vcpkg `git-tree` values in `../versions/i-/imbh-c.json` are placeholders until
  `vcpkg-add-version.sh` runs against the committed port.
- Static linkage's system-lib list is the canonical `rustc --print native-static-libs` set; if a
  future upstream dependency adds a new native requirement, extend it in both `portfile.cmake`
  (`IMBH_C_SYSTEM_LIBS`/`IMBH_C_FRAMEWORKS`) and `conanfile.py` (`package_info`).
