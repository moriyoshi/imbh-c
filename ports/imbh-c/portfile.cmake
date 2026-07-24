# imbh-c is distributed as prebuilt, per-target archives attached to each GitHub Release (built by
# .github/workflows/release.yml). This port downloads the archive matching the active triplet and
# installs its headers + the requested library flavor, then emits a CMake package config exposing
# `imbh-c::imbh-c`.
#
# The shared library is fully self-contained (all Rust + bundled C deps linked in); the static
# archive is a Rust `staticlib` and therefore needs the platform's Rust-std system libraries at final
# link time — those are attached to the imported target's INTERFACE_LINK_LIBRARIES below.
#
# The per-target SHA512 values are filled from a real release by packaging/update-hashes.sh <tag>.
# Until then they are the sentinel "0" and vcpkg will refuse the download (by design).

set(IMBH_C_VERSION "0.0.0")
set(IMBH_C_REPO "moriyoshi/imbh-c")

# --- Map the vcpkg triplet to a Rust target triple + archive extension ----------------------------
if(VCPKG_TARGET_ARCHITECTURE STREQUAL "x64")
    set(_arch "x86_64")
elseif(VCPKG_TARGET_ARCHITECTURE STREQUAL "arm64")
    set(_arch "aarch64")
else()
    message(FATAL_ERROR "imbh-c: unsupported architecture '${VCPKG_TARGET_ARCHITECTURE}' (need x64 or arm64).")
endif()

if(VCPKG_TARGET_IS_LINUX)
    set(_rust_target "${_arch}-unknown-linux-gnu")
    set(_ext "tar.gz")
elseif(VCPKG_TARGET_IS_OSX)
    set(_rust_target "${_arch}-apple-darwin")
    set(_ext "tar.gz")
elseif(VCPKG_TARGET_IS_WINDOWS)
    if(NOT _arch STREQUAL "x86_64")
        message(FATAL_ERROR "imbh-c: Windows binaries are published for x64 only.")
    endif()
    set(_rust_target "x86_64-pc-windows-msvc")
    set(_ext "zip")
else()
    message(FATAL_ERROR "imbh-c: unsupported target platform.")
endif()

# --- Per-target archive SHA512 (regenerate with: packaging/update-hashes.sh v${IMBH_C_VERSION}) ----
set(_sha512_x86_64-unknown-linux-gnu  "0")
set(_sha512_aarch64-unknown-linux-gnu "0")
set(_sha512_x86_64-apple-darwin       "0")
set(_sha512_aarch64-apple-darwin      "0")
set(_sha512_x86_64-pc-windows-msvc    "0")
set(_sha512 "${_sha512_${_rust_target}}")
if(_sha512 STREQUAL "0")
    message(FATAL_ERROR
        "imbh-c: no SHA512 recorded for target '${_rust_target}'. "
        "Run packaging/update-hashes.sh v${IMBH_C_VERSION} against a published release, "
        "commit the result, then re-run `vcpkg x-add-version imbh-c`.")
endif()

# --- Download + extract ---------------------------------------------------------------------------
set(_name "imbh-c-${IMBH_C_VERSION}-${_rust_target}")
vcpkg_download_distfile(_archive
    URLS "https://github.com/${IMBH_C_REPO}/releases/download/v${IMBH_C_VERSION}/${_name}.${_ext}"
    FILENAME "${_name}.${_ext}"
    SHA512 "${_sha512}")

vcpkg_extract_source_archive(_src ARCHIVE "${_archive}")

# --- Headers --------------------------------------------------------------------------------------
file(COPY "${_src}/include" DESTINATION "${CURRENT_PACKAGES_DIR}")

# --- Select the library flavor for the requested linkage ------------------------------------------
# Filenames as produced by cargo for each (platform, crate-type); see release.yml's Package step.
if(VCPKG_TARGET_IS_WINDOWS)
    if(VCPKG_LIBRARY_LINKAGE STREQUAL "dynamic")
        set(_lib_files "imbh_c.dll.lib")     # import lib -> lib/
        set(_bin_files "imbh_c.dll")         # runtime    -> bin/
        set(IMBH_C_LIBTYPE SHARED)
        set(IMBH_C_LIB_SUBPATH "bin/imbh_c.dll")
        set(IMBH_C_IMPLIB_SUBPATH "lib/imbh_c.dll.lib")
    else()
        set(_lib_files "imbh_c.lib")
        set(_bin_files "")
        set(IMBH_C_LIBTYPE STATIC)
        set(IMBH_C_LIB_SUBPATH "lib/imbh_c.lib")
        set(IMBH_C_IMPLIB_SUBPATH "")
    endif()
elseif(VCPKG_TARGET_IS_OSX)
    if(VCPKG_LIBRARY_LINKAGE STREQUAL "dynamic")
        set(_lib_files "libimbh_c.dylib")
        set(IMBH_C_LIBTYPE SHARED)
        set(IMBH_C_LIB_SUBPATH "lib/libimbh_c.dylib")
    else()
        set(_lib_files "libimbh_c.a")
        set(IMBH_C_LIBTYPE STATIC)
        set(IMBH_C_LIB_SUBPATH "lib/libimbh_c.a")
    endif()
    set(_bin_files "")
    set(IMBH_C_IMPLIB_SUBPATH "")
else() # Linux
    if(VCPKG_LIBRARY_LINKAGE STREQUAL "dynamic")
        set(_lib_files "libimbh_c.so")
        set(IMBH_C_LIBTYPE SHARED)
        set(IMBH_C_LIB_SUBPATH "lib/libimbh_c.so")
    else()
        set(_lib_files "libimbh_c.a")
        set(IMBH_C_LIBTYPE STATIC)
        set(IMBH_C_LIB_SUBPATH "lib/libimbh_c.a")
    endif()
    set(_bin_files "")
    set(IMBH_C_IMPLIB_SUBPATH "")
endif()

foreach(_dir "${CURRENT_PACKAGES_DIR}/lib" "${CURRENT_PACKAGES_DIR}/debug/lib")
    foreach(_f IN LISTS _lib_files)
        if(NOT EXISTS "${_src}/lib/${_f}")
            message(FATAL_ERROR "imbh-c: expected '${_f}' in the ${_rust_target} archive but it is missing.")
        endif()
        file(INSTALL "${_src}/lib/${_f}" DESTINATION "${_dir}")
    endforeach()
endforeach()

if(_bin_files)
    foreach(_dir "${CURRENT_PACKAGES_DIR}/bin" "${CURRENT_PACKAGES_DIR}/debug/bin")
        foreach(_f IN LISTS _bin_files)
            file(INSTALL "${_src}/lib/${_f}" DESTINATION "${_dir}")
        endforeach()
    endforeach()
endif()

# --- Rust-std system libraries required only when statically linking ------------------------------
# Canonical `rustc --print native-static-libs` sets, trimmed to what a `staticlib` needs at link time.
set(IMBH_C_SYSTEM_LIBS "")
set(IMBH_C_FRAMEWORKS "")
if(VCPKG_LIBRARY_LINKAGE STREQUAL "static")
    if(VCPKG_TARGET_IS_LINUX)
        set(IMBH_C_SYSTEM_LIBS "pthread;dl;m;rt;util")
    elseif(VCPKG_TARGET_IS_OSX)
        set(IMBH_C_SYSTEM_LIBS "System;c;m")
        set(IMBH_C_FRAMEWORKS "CoreFoundation;Security;SystemConfiguration")
    elseif(VCPKG_TARGET_IS_WINDOWS)
        set(IMBH_C_SYSTEM_LIBS
            "kernel32;advapi32;ntdll;userenv;ws2_32;dbghelp;bcrypt;crypt32;secur32;ncrypt")
    endif()
endif()

# --- Emit the CMake package config ----------------------------------------------------------------
configure_file(
    "${CMAKE_CURRENT_LIST_DIR}/imbh-c-config.cmake.in"
    "${CURRENT_PACKAGES_DIR}/share/${PORT}/imbh-c-config.cmake"
    @ONLY)

# --- Copyright ------------------------------------------------------------------------------------
if(EXISTS "${_src}/LICENSE")
    vcpkg_install_copyright(FILE_LIST "${_src}/LICENSE")
else()
    file(WRITE "${CURRENT_PACKAGES_DIR}/share/${PORT}/copyright"
        "imbh-c is licensed under the Apache License 2.0.\nSee https://github.com/moriyoshi/imbh-c\n")
endif()

# --- Usage doc ------------------------------------------------------------------------------------
configure_file("${CMAKE_CURRENT_LIST_DIR}/usage" "${CURRENT_PACKAGES_DIR}/share/${PORT}/usage" COPYONLY)
