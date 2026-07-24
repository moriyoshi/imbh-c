"""Conan 2.x recipe for imbh-c.

imbh-c is shipped as prebuilt, per-target archives attached to each GitHub Release
(built by .github/workflows/release.yml). This recipe downloads the archive matching the
configured (os, arch) and installs its headers + the requested library flavor. No Rust toolchain
is needed to consume it.

The shared library is fully self-contained; the static archive is a Rust `staticlib` and therefore
needs the platform's Rust-std system libraries at final link time (declared in package_info()).

The per-target SHA256 values in `_sha256` are the sentinel "0" until filled from a real release by
`packaging/update-hashes.sh <tag>`.
"""

from conan import ConanFile
from conan.errors import ConanInvalidConfiguration
from conan.tools.files import get, copy, rename
import os


class ImbhcConan(ConanFile):
    name = "imbh-c"
    version = "0.0.0"
    license = "Apache-2.0"
    homepage = "https://github.com/moriyoshi/imbh-c"
    url = "https://github.com/moriyoshi/imbh-c"
    description = (
        "C/C++ bindings for IMBH, the embeddable observability database: open a Db, ingest OTLP, "
        "run SQL/PromQL/LogQL/TraceQL, and receive results zero-copy as an Arrow C Data Interface "
        "stream."
    )
    topics = ("observability", "database", "otlp", "arrow", "ffi", "imbh", "prebuilt")

    # "library" + the `shared` option lets Conan resolve this to shared-library / static-library.
    package_type = "library"
    settings = "os", "arch", "compiler", "build_type"
    options = {"shared": [True, False]}
    default_options = {"shared": True}

    # (os, arch) -> (rust target triple, archive extension)
    _targets = {
        ("Linux", "x86_64"): ("x86_64-unknown-linux-gnu", "tar.gz"),
        ("Linux", "armv8"): ("aarch64-unknown-linux-gnu", "tar.gz"),
        ("Macos", "x86_64"): ("x86_64-apple-darwin", "tar.gz"),
        ("Macos", "armv8"): ("aarch64-apple-darwin", "tar.gz"),
        ("Windows", "x86_64"): ("x86_64-pc-windows-msvc", "zip"),
    }

    # Per-rust-target archive SHA256; fill with packaging/update-hashes.sh v<version>.
    _sha256 = {
        "x86_64-unknown-linux-gnu": "0",
        "aarch64-unknown-linux-gnu": "0",
        "x86_64-apple-darwin": "0",
        "aarch64-apple-darwin": "0",
        "x86_64-pc-windows-msvc": "0",
    }

    def package_id(self):
        # One binary per (os, arch, shared); the C ABI is compiler/build-type independent.
        del self.info.settings.compiler
        del self.info.settings.build_type

    def validate(self):
        key = (str(self.settings.os), str(self.settings.arch))
        if key not in self._targets:
            raise ConanInvalidConfiguration(
                f"imbh-c has no prebuilt archive for {key[0]}/{key[1]}. "
                "Published targets: Linux/Macos on x86_64+armv8, Windows on x86_64."
            )
        target = self._targets[key][0]
        if self._sha256[target] == "0":
            raise ConanInvalidConfiguration(
                f"imbh-c: no SHA256 recorded for '{target}'. Run "
                f"packaging/update-hashes.sh v{self.version} against a published release."
            )

    def build(self):
        target, ext = self._targets[(str(self.settings.os), str(self.settings.arch))]
        name = f"imbh-c-{self.version}-{target}"
        url = (
            f"https://github.com/moriyoshi/imbh-c/releases/download/"
            f"v{self.version}/{name}.{ext}"
        )
        # strip_root drops the leading "imbh-c-<ver>-<target>/" directory.
        get(self, url, sha256=self._sha256[target], strip_root=True)

    def _lib_names(self):
        """Return (files_to_lib_dir, files_to_bin_dir) for the configured linkage/platform."""
        os_name = str(self.settings.os)
        if os_name == "Windows":
            if self.options.shared:
                return (["imbh_c.dll.lib"], ["imbh_c.dll"])
            return (["imbh_c.lib"], [])
        if os_name == "Macos":
            return ([("libimbh_c.dylib" if self.options.shared else "libimbh_c.a")], [])
        return ([("libimbh_c.so" if self.options.shared else "libimbh_c.a")], [])

    def package(self):
        src = self.build_folder
        copy(self, "*", src=os.path.join(src, "include"),
             dst=os.path.join(self.package_folder, "include"))

        lib_files, bin_files = self._lib_names()
        for f in lib_files:
            copy(self, f, src=os.path.join(src, "lib"),
                 dst=os.path.join(self.package_folder, "lib"))
        for f in bin_files:
            copy(self, f, src=os.path.join(src, "lib"),
                 dst=os.path.join(self.package_folder, "bin"))

        # Normalize the Windows import-lib name so `libs = ["imbh_c"]` resolves it.
        if str(self.settings.os) == "Windows" and self.options.shared:
            rename(self,
                   os.path.join(self.package_folder, "lib", "imbh_c.dll.lib"),
                   os.path.join(self.package_folder, "lib", "imbh_c.lib"))

        lic = os.path.join(src, "LICENSE")
        if os.path.exists(lic):
            copy(self, "LICENSE", src=src,
                 dst=os.path.join(self.package_folder, "licenses"))
        else:
            os.makedirs(os.path.join(self.package_folder, "licenses"), exist_ok=True)
            with open(os.path.join(self.package_folder, "licenses", "LICENSE"),
                      "w", encoding="utf-8") as fh:
                fh.write("imbh-c is licensed under Apache-2.0.\n"
                         "See https://github.com/moriyoshi/imbh-c\n")

    def package_info(self):
        self.cpp_info.libs = ["imbh_c"]

        # Match the vcpkg port's target name so downstream CMake is identical across managers.
        self.cpp_info.set_property("cmake_file_name", "imbh-c")
        self.cpp_info.set_property("cmake_target_name", "imbh-c::imbh-c")
        self.cpp_info.set_property("pkg_config_name", "imbh")

        # The self-contained shared lib needs nothing extra; the static archive needs Rust-std libs.
        if not self.options.shared:
            os_name = str(self.settings.os)
            if os_name == "Linux":
                self.cpp_info.system_libs = ["pthread", "dl", "m", "rt", "util"]
            elif os_name == "Macos":
                self.cpp_info.frameworks = ["CoreFoundation", "Security", "SystemConfiguration"]
            elif os_name == "Windows":
                self.cpp_info.system_libs = [
                    "kernel32", "advapi32", "ntdll", "userenv", "ws2_32",
                    "dbghelp", "bcrypt", "crypt32", "secur32", "ncrypt",
                ]
