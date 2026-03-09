#!/usr/bin/env python3

from __future__ import annotations

import argparse
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Inject a release version into Cargo workspace manifests."
    )
    parser.add_argument("--version", required=True, help="Release version to inject")
    return parser.parse_args()


def replace_once(content: str, old: str, new: str, label: str) -> str:
    count = content.count(old)
    if count != 1:
        raise RuntimeError(f"expected exactly one {label} entry, found {count}")
    return content.replace(old, new, 1)


def main() -> None:
    args = parse_args()
    workspace_manifest = Path(__file__).resolve().parents[1] / "Cargo.toml"
    content = workspace_manifest.read_text(encoding="utf-8")
    content = replace_once(
        content,
        'version = "0.0.0-dev"',
        f'version = "{args.version}"',
        "workspace version",
    )
    content = replace_once(
        content,
        'xurl-core = { version = "=0.0.0-dev", path = "xurl-core" }',
        f'xurl-core = {{ version = "={args.version}", path = "xurl-core" }}',
        "workspace dependency version",
    )
    workspace_manifest.write_text(content, encoding="utf-8")


if __name__ == "__main__":
    main()
