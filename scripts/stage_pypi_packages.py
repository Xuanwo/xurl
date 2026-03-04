#!/usr/bin/env python3
"""Stage platform wheels for xurl from prebuilt release binaries."""

from __future__ import annotations

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent

PACKAGE_NAME = "xuanwo-xurl"
IMPORT_NAME = "xuanwo_xurl"

TARGETS: dict[str, dict[str, str]] = {
    "x86_64-unknown-linux-gnu": {
        "binary": "xurl",
        "plat_name": "manylinux2014_x86_64",
    },
    "aarch64-unknown-linux-gnu": {
        "binary": "xurl",
        "plat_name": "manylinux2014_aarch64",
    },
    "x86_64-apple-darwin": {
        "binary": "xurl",
        "plat_name": "macosx_11_0_x86_64",
    },
    "aarch64-apple-darwin": {
        "binary": "xurl",
        "plat_name": "macosx_11_0_arm64",
    },
    "x86_64-pc-windows-msvc": {
        "binary": "xurl.exe",
        "plat_name": "win_amd64",
    },
    "aarch64-pc-windows-msvc": {
        "binary": "xurl.exe",
        "plat_name": "win_arm64",
    },
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Stage xurl PyPI wheels from release binaries.")
    parser.add_argument(
        "--release-version",
        required=True,
        help="Version to stage, for example 0.0.13.",
    )
    parser.add_argument(
        "--vendor-src",
        type=Path,
        required=True,
        help="Vendor source directory containing target triple trees.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Output directory for staged wheel files.",
    )
    return parser.parse_args()


def render_setup_py(version: str) -> str:
    return f"""from pathlib import Path
from setuptools import setup
from setuptools.dist import Distribution
from wheel.bdist_wheel import bdist_wheel as _bdist_wheel


class BinaryDistribution(Distribution):
    def has_ext_modules(self):
        return True


class Py3NonePlatformWheel(_bdist_wheel):
    def get_tag(self):
        _python, _abi, plat = super().get_tag()
        return ("py3", "none", plat)

readme_path = Path(__file__).resolve().parent / "README.md"
long_description = readme_path.read_text(encoding="utf-8") if readme_path.exists() else ""


setup(
    name="{PACKAGE_NAME}",
    version="{version}",
    description="Locate and read local code-agent thread files",
    long_description=long_description,
    long_description_content_type="text/markdown",
    license="Apache-2.0",
    python_requires=">=3.8",
    packages=["{IMPORT_NAME}"],
    package_data={{"{IMPORT_NAME}": ["bin/*"]}},
    include_package_data=True,
    url="https://github.com/Xuanwo/xurl",
    project_urls={{
        "Homepage": "https://github.com/Xuanwo/xurl",
        "Repository": "https://github.com/Xuanwo/xurl",
        "Issues": "https://github.com/Xuanwo/xurl/issues",
    }},
    entry_points={{"console_scripts": ["xurl={IMPORT_NAME}._runner:main"]}},
    cmdclass={{"bdist_wheel": Py3NonePlatformWheel}},
    distclass=BinaryDistribution,
)
"""


def render_runner_py() -> str:
    return """from __future__ import annotations

import os
import sys
from pathlib import Path


def main() -> None:
    binary_name = "xurl.exe" if os.name == "nt" else "xurl"
    binary_path = Path(__file__).resolve().parent / "bin" / binary_name
    if not binary_path.exists():
        raise FileNotFoundError(f"xurl binary not found: {binary_path}")

    os.execv(str(binary_path), [str(binary_path), *sys.argv[1:]])
"""


def prepare_stage_dir(
    *,
    stage_dir: Path,
    version: str,
    vendor_src: Path,
    target: str,
    binary_name: str,
) -> None:
    package_root = stage_dir / IMPORT_NAME
    bin_root = package_root / "bin"
    bin_root.mkdir(parents=True, exist_ok=True)

    (stage_dir / "setup.py").write_text(render_setup_py(version), encoding="utf-8")
    shutil.copy2(REPO_ROOT / "README.md", stage_dir / "README.md")
    (package_root / "__init__.py").write_text(f'__version__ = "{version}"\n', encoding="utf-8")
    (package_root / "_runner.py").write_text(render_runner_py(), encoding="utf-8")

    source_binary = vendor_src / target / "xurl" / binary_name
    if not source_binary.exists():
        raise FileNotFoundError(f"Missing binary for target {target}: {source_binary}")

    destination_binary = bin_root / binary_name
    shutil.copy2(source_binary, destination_binary)
    if binary_name != "xurl.exe":
        destination_binary.chmod(destination_binary.stat().st_mode | 0o111)


def stage_wheel(
    *,
    version: str,
    vendor_src: Path,
    output_dir: Path,
    target: str,
    binary_name: str,
    plat_name: str,
) -> None:
    with tempfile.TemporaryDirectory(prefix=f"xurl-pypi-stage-{target}-") as stage_dir_str:
        stage_dir = Path(stage_dir_str)
        prepare_stage_dir(
            stage_dir=stage_dir,
            version=version,
            vendor_src=vendor_src,
            target=target,
            binary_name=binary_name,
        )

        subprocess.run(
            [
                "python3",
                "setup.py",
                "bdist_wheel",
                "--plat-name",
                plat_name,
                "--dist-dir",
                str(output_dir.resolve()),
            ],
            cwd=stage_dir,
            check=True,
        )


def main() -> int:
    args = parse_args()
    version = args.release_version
    vendor_src = args.vendor_src.resolve()
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    if not vendor_src.exists():
        raise FileNotFoundError(f"Vendor source directory not found: {vendor_src}")

    for target, config in TARGETS.items():
        stage_wheel(
            version=version,
            vendor_src=vendor_src,
            output_dir=output_dir,
            target=target,
            binary_name=config["binary"],
            plat_name=config["plat_name"],
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
