#!/usr/bin/env python3
"""Check local links/images and GitHub-style heading anchors in user docs.

Install scripts/requirements-docs.txt first. External URLs are deliberately
not fetched. Historical reports and other design documents are not default roots;
pass explicit paths to check them. Linked Markdown targets are parsed for
anchors but their outgoing links are checked only when selected as roots.
"""

from __future__ import annotations

import argparse
import re
import subprocess
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

import markdown

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DOCS = (
    "README.md",
    "CHANGELOG.md",
    "API.md",
    "DEVELOPERS.md",
    "TESTING.md",
    "docs/LIB_mode.md",
    "docs/ARCHITECTURE.md",
    "docs/installation.md",
    "docs/usage.md",
    "docs/configuration.md",
    "docs/services.md",
    "docs/recording-and-scripting.md",
    "docs/troubleshooting.md",
)


def slug(text: str) -> str:
    """Match GitHub heading IDs for the punctuation used in this repository."""
    return re.sub(r"[^\w\- ]", "", text.lower()).replace(" ", "-")


class Document(HTMLParser):
    """Collect rendered links, images, explicit IDs, and heading anchors."""

    def __init__(self, source: str) -> None:
        super().__init__(convert_charrefs=True)
        self.links: list[str] = []
        self.anchors: set[str] = set()
        self._heading: list[str] | None = None
        self._heading_ids: set[str] = set()
        self.feed(markdown.markdown(source, extensions=["fenced_code", "tables"]))

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        for key in ("id", "name") if tag == "a" else ("id",):
            if values.get(key):
                self.anchors.add(values[key])
        if tag in ("a", "img"):
            target = values.get("href" if tag == "a" else "src")
            if target:
                self.links.append(target)
        if re.fullmatch(r"h[1-6]", tag):
            self._heading = []

    def handle_data(self, data: str) -> None:
        if self._heading is not None:
            self._heading.append(data)

    def handle_endtag(self, tag: str) -> None:
        if re.fullmatch(r"h[1-6]", tag) and self._heading is not None:
            base = slug("".join(self._heading))
            anchor = base
            suffix = 0
            while anchor in self._heading_ids:
                suffix += 1
                anchor = f"{base}-{suffix}"
            self._heading_ids.add(anchor)
            self.anchors.add(anchor)
            self._heading = None


def check(paths: list[Path], root: Path = ROOT) -> list[str]:
    """Return actionable failures without making any network requests."""
    errors: list[str] = []
    documents: dict[Path, Document] = {}
    root = root.resolve()
    tracked: set[str] | None = None
    if (root / ".git").exists():
        tracked = set(
            subprocess.check_output(
                ["git", "-C", str(root), "ls-files", "-z"], text=True
            ).split("\0")
        )

    def read(path: Path) -> Document:
        if path not in documents:
            documents[path] = Document(path.read_text(encoding="utf-8"))
        return documents[path]

    for path in paths:
        path = path.resolve()
        if not path.is_file():
            errors.append(f"{path}: missing documentation file")
            continue
        for link in read(path).links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                continue
            relative = unquote(url.path)
            target = (
                (
                    root / relative.lstrip("/")
                    if relative.startswith("/")
                    else path.parent / relative
                )
                if relative
                else path
            )
            target = target.resolve()
            label = f"{path.relative_to(root)}: {link}"
            if not target.is_relative_to(root):
                errors.append(f"{label}: target escapes repository")
            elif not target.exists():
                errors.append(f"{label}: missing target")
            elif (
                target.is_file()
                and tracked is not None
                and target.relative_to(root).as_posix() not in tracked
            ):
                errors.append(f"{label}: target is not tracked by git")
            elif url.fragment and target.suffix.lower() == ".md":
                anchor = unquote(url.fragment)
                if anchor not in read(target).anchors:
                    errors.append(f"{label}: missing heading anchor")
    return errors


def main() -> int:
    """Check default user documentation or explicitly selected paths."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path)
    args = parser.parse_args()
    paths = args.paths or [ROOT / name for name in DEFAULT_DOCS]
    errors = check(paths)
    for error in errors:
        print(error)
    print(f"Checked {len(paths)} documents: {len(errors)} broken local links")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
