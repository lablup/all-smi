"""Regression tests for documentation moves, anchors, and screenshot links."""

import importlib.util
from pathlib import Path

import pytest

SPEC = importlib.util.spec_from_file_location(
    "doc_links", Path(__file__).resolve().parents[1] / "check-doc-links.py"
)
assert SPEC is not None and SPEC.loader is not None
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


@pytest.mark.parametrize(
    "heading,anchor",
    [
        ("GET `/-/ready`", "get--ready"),
        ("Filtering & Alerts", "filtering--alerts"),
        ("Hello (world)", "hello-world"),
        ("설정", "설정"),
    ],
)
def test_heading_anchors(heading: str, anchor: str) -> None:
    assert anchor in checker.Document(f"## {heading}").anchors


def test_duplicate_headings_and_code_blocks() -> None:
    doc = checker.Document("# A\n\n# A\n\n# A-1\n\n```md\n# False\n[x](missing)\n```\n")
    assert doc.anchors == {"a", "a-1", "a-1-1"}
    assert doc.links == []


def test_links_and_images(tmp_path: Path) -> None:
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs/guide.md").write_text("# API\n\n## `GET /-/ready`\n")
    (tmp_path / "shot.png").write_bytes(b"image")
    readme = tmp_path / "README.md"
    readme.write_text(
        "[API](docs/guide.md#get--ready)\n\n![screen](shot.png)\n\n"
        "[reference][ref]\n\n[ref]: docs/guide.md#api\n\n"
        '<a id="custom"></a>\n\n[custom](#custom)\n\n'
        "[external](https://example.invalid/missing)\n"
    )
    assert checker.check([readme], tmp_path) == []


@pytest.mark.parametrize(
    "link,message",
    [
        ("missing.md", "missing target"),
        ("README.md#absent", "missing heading anchor"),
        ("../outside", "escapes repository"),
    ],
)
def test_broken_links(tmp_path: Path, link: str, message: str) -> None:
    readme = tmp_path / "README.md"
    readme.write_text(f"# Home\n\n[broken]({link})\n")
    errors = checker.check([readme], tmp_path)
    assert len(errors) == 1 and message in errors[0]


def test_encoded_paths_and_root_relative_links(tmp_path: Path) -> None:
    (tmp_path / "a b.md").write_text("# A B")
    readme = tmp_path / "README.md"
    readme.write_text("[space](a%20b.md#a-b)\n\n[root](/a%20b.md#a-b)")
    assert checker.check([readme], tmp_path) == []


def test_missing_root(tmp_path: Path) -> None:
    assert (
        "missing documentation" in checker.check([tmp_path / "missing.md"], tmp_path)[0]
    )


@pytest.mark.parametrize("valid", [True, False])
def test_cli_exit_code(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    valid: bool,
) -> None:
    readme = tmp_path / "README.md"
    readme.write_text("# Home" if valid else "[broken](missing.md)")
    monkeypatch.setattr(checker, "ROOT", tmp_path)
    # check's default root is bound at definition time; wrap it for this fixture.
    original = checker.check
    monkeypatch.setattr(checker, "check", lambda paths: original(paths, tmp_path))
    monkeypatch.setattr("sys.argv", ["check-doc-links.py", str(readme)])
    assert checker.main() == (0 if valid else 1)
    assert "Checked 1 documents" in capsys.readouterr().out


def test_rendered_configuration_table() -> None:
    source = (checker.ROOT / "docs/configuration.md").read_text()
    rendered = checker.markdown.markdown(source, extensions=["tables"])
    assert "<td><code>compress</code></td>" in rendered


def test_local_ignored_file_is_not_a_valid_target(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".git").mkdir()
    (tmp_path / "ignored.md").write_text("# Locally present only")
    readme = tmp_path / "README.md"
    readme.write_text("[ignored](ignored.md)")
    monkeypatch.setattr(
        checker.subprocess, "check_output", lambda *args, **kwargs: "README.md\0"
    )
    assert "not tracked by git" in checker.check([readme], tmp_path)[0]
