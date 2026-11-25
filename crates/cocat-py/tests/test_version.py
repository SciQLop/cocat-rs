"""The release number is written once, in the workspace Cargo.toml; the package takes it."""

import importlib.metadata
import pathlib
import re

import cocat_rs

ROOT = pathlib.Path(__file__).resolve().parents[3]


def table(path, name):
    """One TOML table's lines, read as text: tomllib needs Python 3.11, cocat-rs runs on 3.10."""
    text = path.read_text(encoding="utf-8")
    start = text.index(f"[{name}]\n") + len(name) + 3
    end = re.search(r"^\[", text[start:], re.MULTILINE)
    return text[start : start + end.start()] if end else text[start:]


def release_version():
    package = table(ROOT / "Cargo.toml", "workspace.package")
    return re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE).group(1)


def test_the_pyproject_takes_its_version_from_cargo():
    project = table(ROOT / "crates" / "cocat-py" / "pyproject.toml", "project")
    assert not re.search(r"^version\s*=", project, re.MULTILINE), "no second place to bump"
    assert re.search(r'^dynamic\s*=\s*\[[^\]]*"version"', project, re.MULTILINE)


def test_the_installed_package_carries_the_release_version():
    assert release_version() != "0.0.0"
    assert importlib.metadata.version("cocat-rs") == release_version()
    assert cocat_rs.__version__ == release_version()
