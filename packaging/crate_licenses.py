#!/usr/bin/env python3
"""Licences of the Rust crates compiled into a package of the workspace.

Usage: crate_licenses.py <package>   (run in the workspace; prints to stdout)

Follows the normal (not dev or build) dependencies of <package> for
x86_64 Linux and prints, for every external crate, its name, version,
licence expression and repository, followed by the licence and notice
files it ships with.
"""

import json
import subprocess
import sys
from pathlib import Path

LICENCE_FILES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE")


def main(root_name: str) -> None:
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked",
             "--filter-platform", "x86_64-unknown-linux-gnu"]
        )
    )
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])

    root = next(i for i in workspace if packages[i]["name"] == root_name)
    seen, todo = set(), [root]
    while todo:
        node = nodes[todo.pop()]
        for dep in node["deps"]:
            normal = any(k["kind"] is None for k in dep["dep_kinds"])
            if normal and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])

    crates = sorted((packages[i] for i in seen - workspace), key=lambda p: (p["name"], p["version"]))
    out = sys.stdout
    out.write(f"Biblioteki Rusta wkompilowane w {root_name} ({len(crates)}).\n\n")
    for p in crates:
        out.write(f"{p['name']} {p['version']}: {p.get('license') or p.get('license_file') or '?'}")
        out.write(f"  {p.get('repository') or ''}\n")
    for p in crates:
        crate_dir = Path(p["manifest_path"]).parent
        files = sorted(f for f in crate_dir.iterdir() if f.is_file() and f.name.upper().startswith(LICENCE_FILES))
        for f in files:
            out.write(f"\n{'=' * 78}\n{p['name']} {p['version']}: {f.name}\n{'=' * 78}\n\n")
            out.write(f.read_text(errors="replace").strip() + "\n")


if __name__ == "__main__":
    main(sys.argv[1])
