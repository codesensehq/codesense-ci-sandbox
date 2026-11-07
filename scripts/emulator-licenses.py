#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regenerate the licence texts at the end of src-tauri/resources/emulator/NOTICE.md.

The emulator worker is a compiled Bun, and Bun links in native libraries, Rust
crates and bundled npm polyfills. Each of their licences asks for its text to
ship with the binary. This script collects every one of those texts from the
exact revisions Bun (BUN_TAG) builds against and writes them between the
GENERATED markers in NOTICE.md. Run it again whenever build-emulator.sh moves to
another Bun, and commit the result.

  python3 scripts/emulator-licenses.py

Needs git, cargo and network access. Downloads go to target/bun-src.
"""

from __future__ import annotations

import io
import json
import os
import re
import subprocess
import tarfile
import urllib.error
import urllib.request
from pathlib import Path

BUN_TAG = "bun-v1.4.2"
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "bun-src"
BUN = WORK / "bun"
NOTICE = ROOT / "src-tauri" / "resources" / "emulator" / "NOTICE.md"
BEGIN = "<!-- GENERATED: scripts/emulator-licenses.py; do not edit below -->"
END = "<!-- END GENERATED -->"

# The platforms the worker ships for; crates that only build elsewhere
# (Windows, Android, wasm) are not in these binaries.
PLATFORMS = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]

WEBKIT = "oven-sh/WebKit@2e2aa2290fac856d6f451ceacb58f7f5b44dd057"

# Native code Bun links, with the licence files at the revision it pins
# (scripts/build/deps/*.ts at BUN_TAG, WebKit at WEBKIT). Windows-only
# libraries (libuv, parts of tigerbeetle's I/O) are not in these binaries.
NATIVE: list[tuple[str, str, list[str]]] = [
    ("Bun (uSockets fork)", f"oven-sh/bun@{BUN_TAG}", ["packages/bun-usockets/LICENSE"]),
    ("Bun (uWebSockets fork)", f"oven-sh/bun@{BUN_TAG}", ["packages/bun-uws/LICENSE"]),
    ("Bun (clap)", f"oven-sh/bun@{BUN_TAG}", ["src/clap/LICENSE"]),
    ("WebKit: JavaScriptCore", WEBKIT, ["Source/JavaScriptCore/COPYING.LIB"]),
    (
        "WebKit: WebCore",
        WEBKIT,
        ["Source/WebCore/LICENSE-APPLE", "Source/WebCore/LICENSE-LGPL-2", "Source/WebCore/LICENSE-LGPL-2.1"],
    ),
    (
        "WebKit: WTF",
        WEBKIT,
        [
            "Source/WTF/LICENSE-LLVM.txt",
            "Source/WTF/LICENSE-dragonbox.txt",
            "Source/WTF/LICENSE-libc++.txt",
            "Source/WTF/LICENSE-simde.txt",
            "Source/WTF/wtf/simdutf/LICENSE-simdutf.txt",
        ],
    ),
    ("ICU", "unicode-org/icu@release-78.1", ["icu4c/LICENSE"]),
    ("BoringSSL", "oven-sh/boringssl@41bf9b59c2ebf277a7aa427e1ecad5cc80dd4d4f", ["LICENSE"]),
    ("brotli", "google/brotli@v1.1.0", ["LICENSE"]),
    ("c-ares", "c-ares/c-ares@c7a3138dcfe3bb0eaaf10c0c24c36dc66dc790ab", ["LICENSE.md"]),
    ("HdrHistogram_c", "HdrHistogram/HdrHistogram_c@be60a9987ee48d0abf0d7b6a175bad8d6c1585d1", ["LICENSE.txt", "COPYING.txt"]),
    ("highway", "google/highway@2607d3b5b0113992fe84d3848859eae13b3b52c1", ["LICENSE"]),
    ("libarchive", "libarchive/libarchive@ded82291ab41d5e355831b96b0e1ff49e24d8939", ["COPYING"]),
    ("libdeflate", "ebiggers/libdeflate@c8c56a20f8f621e6a966b716b31f1dedab6a41e3", ["COPYING"]),
    ("libjpeg-turbo", "libjpeg-turbo/libjpeg-turbo@e352b02f794f701407b39af08576035ba3360d60", ["LICENSE.md", "README.ijg"]),
    ("libspng", "randy408/libspng@fb768002d4288590083a476af628e51c3f1d47cd", ["LICENSE"]),
    ("libwebp", "webmproject/libwebp@b7e29b9d75bd31422b00c2a446d49d7af06c328d", ["COPYING", "PATENTS"]),
    ("ls-hpack", "litespeedtech/ls-hpack@8905c024b6d052f083a3d11d0a169b3c2735c8a1", ["LICENSE"]),
    ("ls-qpack", "litespeedtech/ls-qpack@1e9c5b8e59f8161c54f168a570c8bfdc59ded0c3", ["LICENSE"]),
    ("lsquic", "litespeedtech/lsquic@3181911301b1aa4f54c1ed690901abc674ee08fb", ["LICENSE", "LICENSE.chrome"]),
    ("mimalloc", "oven-sh/mimalloc@6a64e1ba7f5b2130d4efccb67ec87fd0003f0f6a", ["LICENSE"]),
    ("picohttpparser", "h2o/picohttpparser@066d2b1e9ab820703db0837a7255d92d30f0c9f5", ["README.md"]),
    ("tinycc", "oven-sh/tinycc@05f0fafaa3be31e31d7b4b5c17dc60f62c991171", ["COPYING"]),
    ("zlib-ng", "zlib-ng/zlib-ng@12731092979c6d07f42da27da673a9f6c7b13586", ["LICENSE.md"]),
    ("zstd", "facebook/zstd@f8745da6ff1ad1e7bab384bd1f9d742439278e99", ["LICENSE"]),
    ("libbase64", "aklomp/base64@master", ["LICENSE"]),
    ("LLVM libc++abi (__cxa_thread_atexit fallback)", "llvm/llvm-project@llvmorg-19.1.0", ["libcxxabi/LICENSE.TXT"]),
    ("esbuild (ported into Bun's transpiler and resolver)", "evanw/esbuild@main", ["LICENSE.md"]),
]

# Only the fields a licence needs: picohttpparser states its terms in its README.
EXCERPT = {"h2o/picohttpparser": r"(?s)(## License.*)"}


def fetch(url: str, accept: str | None = None) -> bytes:
    headers = {"User-Agent": "codesense-emulator-licenses"}
    if accept:
        headers["Accept"] = accept
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        raise SystemExit(f"{url}: {error}") from error


def github_file(repo_ref: str, path: str) -> str:
    repo, ref = repo_ref.split("@")
    # The contents API also serves renamed and mirrored repositories that
    # raw.githubusercontent.com answers 404 for.
    text = fetch(
        f"https://api.github.com/repos/{repo}/contents/{path}?ref={ref}",
        accept="application/vnd.github.raw",
    ).decode("utf-8", "replace")
    pattern = EXCERPT.get(repo)
    if pattern:
        match = re.search(pattern, text)
        text = match.group(1) if match else text
    return text


def checkout_bun() -> None:
    """Bun's manifests at BUN_TAG, plus the vendored crates its workspace names."""
    if not (BUN / "Cargo.lock").exists():
        WORK.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["git", "clone", "-q", "--depth", "1", "--branch", BUN_TAG, "--filter=blob:none",
             "--no-checkout", "https://github.com/oven-sh/bun.git", str(BUN)],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(BUN), "sparse-checkout", "set", "--no-cone", "/Cargo.toml",
             "/Cargo.lock", "**/Cargo.toml", "**/*.rs", "/src/node-fallbacks/bun.lock"],
            check=True,
        )
        subprocess.run(["git", "-C", str(BUN), "checkout", "-q", BUN_TAG], check=True)
    vendored = {
        "lolhtml": "oven-sh/lol-html@725ce499aa9b71e38b7a2d0a9fbb6d7294a4079e",
        "rust-argon2": "sru-systems/rust-argon2@ed81866f163f0c7026aa6fd8388adf37242eb32a",
    }
    for name, repo_ref in vendored.items():
        target = BUN / "vendor" / name
        if target.exists():
            continue
        repo, ref = repo_ref.split("@")
        subprocess.run(["git", "init", "-q", str(target)], check=True)
        subprocess.run(["git", "-C", str(target), "fetch", "-q", "--depth", "1",
                        f"https://github.com/{repo}.git", ref], check=True)
        subprocess.run(["git", "-C", str(target), "checkout", "-q", "FETCH_HEAD"], check=True)


def linked_crates() -> list[dict]:
    """Every crate bun_runtime links on PLATFORMS: normal dependencies only
    (build scripts and proc macros run at build time and ship nothing)."""
    env = {"RUSTUP_TOOLCHAIN": "stable", "CARGO_HOME": str(WORK / "cargo-home")}

    crates: dict[str, dict] = {}
    for platform in PLATFORMS:
        out = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", platform],
            cwd=BUN, env={**os.environ, **env}, check=True, capture_output=True,
        ).stdout
        meta = json.loads(out)
        packages = {p["id"]: p for p in meta["packages"]}
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        root = next(p["id"] for p in meta["packages"] if p["name"] == "bun_runtime")
        stack, seen = [root], set()
        while stack:
            node = nodes[stack.pop()]
            if node["id"] in seen:
                continue
            seen.add(node["id"])
            for dep in node["deps"]:
                if any(k["kind"] is None for k in dep["dep_kinds"]):
                    stack.append(dep["pkg"])
        workspace = set(meta["workspace_members"])
        for pid in seen:
            package = packages[pid]
            kinds = {k for t in package["targets"] for k in t["kind"]}
            if pid in workspace or kinds == {"proc-macro"} or kinds <= {"proc-macro", "custom-build"}:
                continue
            crates[f"{package['name']} {package['version']}"] = package
    return [crates[key] for key in sorted(crates)]


LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-._].*)?$", re.I)


def spdx_text(identifier: str) -> str:
    cache = WORK / "spdx" / f"{identifier}.txt"
    if not cache.exists():
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_bytes(fetch(
            f"https://raw.githubusercontent.com/spdx/license-list-data/main/text/{identifier}.txt"))
    return cache.read_text()


def crate_licence(package: dict) -> str:
    directory = Path(package["manifest_path"]).parent
    files = sorted(p for p in directory.iterdir() if p.is_file() and LICENSE_FILE.match(p.name))
    if files:
        return "\n\n".join(f"--- {p.name} ---\n{p.read_text(errors='replace').strip()}" for p in files)
    # No licence file in the crate: its SPDX expression's standard texts.
    expression = package.get("license") or ""
    ids = sorted(set(re.findall(r"[A-Za-z0-9.+-]+", expression)) - {"OR", "AND", "WITH"})
    if not ids:
        raise SystemExit(f"{package['name']} {package['version']}: no licence file and no licence field")
    return "\n\n".join(f"--- {i} (SPDX standard text) ---\n{spdx_text(i).strip()}" for i in ids)


def polyfills() -> list[tuple[str, str]]:
    """The npm packages Bun bundles as Node polyfills (src/node-fallbacks)."""
    lock = (BUN / "src" / "node-fallbacks" / "bun.lock").read_text()
    lock = json.loads(re.sub(r",(\s*[}\]])", r"\1", lock))
    specs = sorted({entry[0] for entry in lock["packages"].values()})
    result = []
    for spec in specs:
        name, version = spec.rsplit("@", 1)
        cache = WORK / "npm" / f"{name.replace('/', '__')}@{version}.txt"
        if not cache.exists():
            cache.parent.mkdir(parents=True, exist_ok=True)
            base = name.split("/")[-1]
            tarball = fetch(f"https://registry.npmjs.org/{name}/-/{base}-{version}.tgz")
            texts = []
            with tarfile.open(fileobj=io.BytesIO(tarball), mode="r:gz") as archive:
                for member in archive.getmembers():
                    parts = member.name.split("/")
                    if member.isfile() and len(parts) == 2 and LICENSE_FILE.match(parts[1]):
                        texts.append(archive.extractfile(member).read().decode("utf-8", "replace").strip())
                if not texts:
                    package_json = json.loads(archive.extractfile("package/package.json").read())
                    licence = package_json.get("license") or "UNKNOWN"
                    texts.append(f"(no licence file; package.json declares {licence})\n\n{spdx_text(licence).strip()}")
            cache.write_text("\n\n".join(texts))
        result.append((spec, cache.read_text()))
    return result


def fenced(text: str) -> str:
    fence = "````" if "```" in text else "```"
    return f"{fence}\n{text.strip()}\n{fence}"


def main() -> None:
    checkout_bun()
    parts = [
        BEGIN,
        "",
        f"## Licences of everything linked into Bun ({BUN_TAG})",
        "",
        "Collected from the exact revisions Bun builds against. The LGPL texts are "
        "also above.",
        "",
        "### Native libraries",
    ]
    for name, repo_ref, paths in NATIVE:
        parts += ["", f"#### {name}", "", f"Source: `{repo_ref}`"]
        for path in paths:
            parts += ["", f"`{path}`:", "", fenced(github_file(repo_ref, path))]
    parts += ["", "### Rust crates", ""]
    for package in linked_crates():
        parts += [
            f"#### {package['name']} {package['version']}",
            "",
            f"License: {package.get('license') or 'see text'}",
            "",
            fenced(crate_licence(package)),
            "",
        ]
    parts += ["### Bundled npm polyfills (src/node-fallbacks)", ""]
    for spec, text in polyfills():
        parts += [f"#### {spec}", "", fenced(text), ""]
    parts.append(END)

    notice = NOTICE.read_text()
    head = notice.split(BEGIN)[0].rstrip()
    NOTICE.write_text(head + "\n\n" + "\n".join(parts) + "\n")


if __name__ == "__main__":
    main()
