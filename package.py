#!/usr/bin/env python3
"""Packs each plugin as dist/<plugin>-<os>-<arch>.zip, the release asset the app's one-click install downloads
(<os>, <arch> as Rust's std::env::consts::OS / ARCH: linux|windows|macos, x86_64|aarch64).
A native plugin (a "library") is packed with its built library; a plugin that is a program (a "command") with its
manifest and its binary. PACKAGE_ARCH overrides the arch of the running machine (a cross-compiled build)."""
import json, os, pathlib, platform, sys, zipfile

OS = {"linux": "linux", "win32": "windows", "darwin": "macos"}[sys.platform]
ARCH = {"amd64": "x86_64", "x86_64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}[(os.environ.get("PACKAGE_ARCH") or platform.machine()).lower()]
root = pathlib.Path(__file__).resolve().parent
dist = root / "dist"
dist.mkdir(exist_ok=True)
for manifest in sorted(root.glob("*/plugin.json")):
    m = json.loads(manifest.read_text())
    out = dist / f"{m['name']}-{OS}-{ARCH}.zip"
    lib = (m.get("library") or {}).get("lib")
    lib = lib.get(OS) if isinstance(lib, dict) else lib
    if lib:
        built = manifest.parent / lib
        if not built.is_file():
            sys.exit(f"{m['name']}: {lib} is not built (run build.sh first)")
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
            z.write(built, lib)
        print(out.name)
        continue
    command = m.get("command") or []
    program = command[0] if command else ""
    if not program or "/" not in program:
        continue
    built = manifest.parent / program
    if not built.is_file():
        built = manifest.parent / (program + ".exe")
    if not built.is_file():
        sys.exit(f"{m['name']}: {program} is not built (run build.sh first)")
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(manifest, "plugin.json")
        z.write(built, str(built.relative_to(manifest.parent)))
    print(out.name)
