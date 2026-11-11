#!/usr/bin/env python3
"""Assemble the mdBook sources for https://djynnius.github.io/rocinante/.

The site has no copy of the docs of its own: every guide page is cut from
docs/GUIDE.md (the same file Rocinante bundles for its rocinante-help skill),
the changelog page is CHANGELOG.md, and the landing page lives in
docs/site/introduction.md. Run it, then `mdbook build target/docs-site`.

    python3 scripts/build-docs-site.py [--out target/docs-site]
"""

import argparse
import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def slug(title: str) -> str:
    """`Local models (embedded llama.cpp)` -> `local-models`."""
    title = re.sub(r"\(.*?\)", "", title)
    return re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")


def split_guide(text: str):
    """Yield (title, body) per `## ` section, ignoring `#` lines inside
    fenced code blocks (the TOML examples are full of comments)."""
    sections, title, lines, fence = [], None, [], False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            fence = not fence
        if not fence and line.startswith("## "):
            if title is not None:
                sections.append((title, "\n".join(lines).strip() + "\n"))
            title, lines = line[3:].strip(), []
            continue
        if title is not None:
            lines.append(line)
    if title is not None:
        sections.append((title, "\n".join(lines).strip() + "\n"))
    return sections


def demote(body: str) -> str:
    """Each page's own title is `#`; the guide's `###` subsections become
    `##` so the page outline starts at the right level. Code fences are left
    alone."""
    out, fence = [], False
    for line in body.splitlines():
        if line.lstrip().startswith("```"):
            fence = not fence
        if not fence and line.startswith("###"):
            line = line[1:]
        out.append(line)
    return "\n".join(out) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default=str(ROOT / "target" / "docs-site"))
    out = Path(parser.parse_args().out)
    src = out / "src"
    if out.exists():
        shutil.rmtree(out)
    src.mkdir(parents=True)

    shutil.copy(ROOT / "docs" / "site" / "book.toml", out / "book.toml")
    shutil.copy(ROOT / "docs" / "site" / "introduction.md", src / "introduction.md")
    for image in ("rocinante-landing.png", "rocinante-inner.png"):
        shutil.copy(ROOT / image, src / image)

    guide = (ROOT / "docs" / "GUIDE.md").read_text(encoding="utf-8")
    sections = split_guide(guide)
    if len(sections) < 10:
        print(f"only {len(sections)} sections found in docs/GUIDE.md", file=sys.stderr)
        return 1
    summary = ["# Summary", "", "[Introduction](introduction.md)", "", "# User guide", ""]
    seen = set()
    for title, body in sections:
        name = slug(title)
        if name in seen:
            print(f"duplicate page slug `{name}`", file=sys.stderr)
            return 1
        seen.add(name)
        (src / f"{name}.md").write_text(f"# {title}\n\n{demote(body)}", encoding="utf-8")
        summary.append(f"- [{title}]({name}.md)")

    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    (src / "changelog.md").write_text(changelog, encoding="utf-8")
    summary += ["", "---", "", "[Changelog](changelog.md)", ""]
    (src / "SUMMARY.md").write_text("\n".join(summary), encoding="utf-8")

    # Served next to the pages (mdBook copies non-Markdown files from src):
    # the installers (`curl … /install.sh | sh`) and the guide as plain text
    # for language models (the llms.txt convention).
    static = src
    for script in ("install.sh", "install.ps1"):
        shutil.copy(ROOT / script, static / script)
    (static / "llms-full.txt").write_text(guide, encoding="utf-8")
    index = [
        "# Rocinante",
        "",
        "> A terminal coding agent for local and cloud models, written in Rust.",
        "",
        "- [Full user guide](https://djynnius.github.io/rocinante/llms-full.txt)",
    ]
    for title, _ in sections:
        index.append(f"- [{title}](https://djynnius.github.io/rocinante/{slug(title)}.html)")
    (static / "llms.txt").write_text("\n".join(index) + "\n", encoding="utf-8")

    print(f"{len(sections)} guide pages -> {src}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
