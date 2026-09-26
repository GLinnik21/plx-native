#!/usr/bin/env python3
"""Render one of docs/{install-and-verify,troubleshooting}.md into a standalone HTML page
for plxnative.com, wrapped in the site's own header/footer and styled with site/styles.css.

Used by `.github/workflows/pages.yml` ("Stage _site") and locally:

    python3 tools/render-doc-page.py docs/install-and-verify.md > /tmp/x.html

Dependency: markdown-it-py, pinned in tools/requirements-docs.txt.
    pip install -r tools/requirements-docs.txt

Markdown -> HTML uses the "gfm-like" preset (tables, strikethrough, autolink literals — but
linkify is disabled: neither doc leans on bare-URL autolinking, and turning it on needs the
separate, unpinned linkify-it-py dependency for no benefit here). Fenced code blocks are core
CommonMark and need no preset.

Heading ids replicate GitHub's own slugger — lowercase, drop everything that isn't a word
character, hyphen or space, then turn each REMAINING space into its own hyphen (a run of spaces
becomes a run of hyphens, not one hyphen: that is what pins the algorithm down, verified against
two anchors this repo already depends on — docs/install-and-verify.md's "## Important: Developer
Mode expires" is linked from README.md as "#important-developer-mode-expires", and its own
"### Option A — install PlxNative directly" is linked internally as
"#option-a--install-plxnative-directly", double hyphen and all, where the em dash used to be).

Link rewriting: install-and-verify.md and troubleshooting.md are the only two docs rendered as
site pages (see PAGES below), so a relative link from one to the other becomes a site path,
fragment kept. Every other repo-relative link — a link to README.md, to a sibling doc that isn't
rendered (native-video-sandbox.md), to an image — becomes an absolute GitHub blob URL, since
nothing else under docs/ is staged into _site/. Absolute http(s) links and bare `#fragment`
in-page anchors are left alone.
"""

from __future__ import annotations

import html
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit

try:
    from markdown_it import MarkdownIt
    from markdown_it.token import Token
except ImportError:  # pragma: no cover - operator-facing message, not exercised by tests
    sys.exit(
        "render-doc-page.py needs markdown-it-py.\n"
        "    pip install -r tools/requirements-docs.txt"
    )

REPO = "GLinnik21/plx-native"
GITHUB_BLOB = f"https://github.com/{REPO}/blob/main"
SITE_ORIGIN = "https://plxnative.com"

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent

# The only two docs rendered as site pages. Adding a third page means adding it here.
PAGES = {
    "install-and-verify.md": {
        "route": "/install/",
        "title": "Installing PlxNative — PlxNative",
        "description": (
            "Step-by-step guide to installing PlxNative on an LG webOS TV with Developer Mode "
            "or Homebrew Channel. No root required."
        ),
    },
    "troubleshooting.md": {
        "route": "/troubleshooting/",
        "title": "Troubleshooting — PlxNative",
        "description": (
            "Fixes for common PlxNative problems on LG webOS TVs: the app tile doing nothing, "
            "an expired Developer Mode session, sign-in, and playback failures."
        ),
    },
}


def github_slug(text: str) -> str:
    """GitHub's heading-anchor algorithm: lowercase, strip anything that isn't a word
    character, hyphen or space, then map each remaining space to a hyphen one-for-one."""
    text = text.lower()
    text = re.sub(r"[^\w\- ]", "", text, flags=re.UNICODE)
    return text.replace(" ", "-")


def assign_heading_ids(tokens: list[Token]) -> str | None:
    """Walk the top-level token stream, giving every heading_open token a GitHub-compatible
    `id`, deduplicated the same way GitHub does (a repeat gets -1, -2, ...). Returns the id
    assigned to the first heading (the page's own <h1>), or None if there isn't one."""
    seen: dict[str, int] = {}
    first_id: str | None = None
    for i, tok in enumerate(tokens):
        if tok.type != "heading_open":
            continue
        inline = tokens[i + 1]
        assert inline.type == "inline"
        base = github_slug(inline.content)
        count = seen.get(base, 0)
        seen[base] = count + 1
        slug = base if count == 0 else f"{base}-{count}"
        tok.attrSet("id", slug)
        if first_id is None:
            first_id = slug
    return first_id


def resolve_repo_path(doc_dir: Path, href_path: str) -> str:
    """Resolve a link's path component against the rendered doc's own directory (both
    posix-style, both relative to the repo root) and return the repo-relative result."""
    combined = (doc_dir / href_path) if href_path else doc_dir
    # normalize "docs/../README.md" -> "README.md" without touching the filesystem
    parts: list[str] = []
    for part in combined.as_posix().split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if parts:
                parts.pop()
            continue
        parts.append(part)
    return "/".join(parts)


def rewrite_href(href: str, doc_dir: Path) -> str:
    href = href.strip()
    if not href or href.startswith("#"):
        return href
    scheme = urlsplit(href).scheme
    if scheme in ("http", "https", "mailto", "tel"):
        return href
    path, _, frag = href.partition("#")
    repo_path = resolve_repo_path(doc_dir, path)
    basename = repo_path.rsplit("/", 1)[-1]
    page = PAGES.get(basename)
    if page is not None and repo_path == f"docs/{basename}":
        target = page["route"]
    else:
        target = f"{GITHUB_BLOB}/{repo_path}"
    return f"{target}#{frag}" if frag else target


def rewrite_links(tokens: list[Token], doc_dir: Path) -> None:
    """Rewrite href/src attributes on link_open and image tokens, recursing into inline
    token children (that's where markdown-it actually puts them)."""
    for tok in tokens:
        if tok.type in ("link_open", "image"):
            href = tok.attrGet("href" if tok.type == "link_open" else "src")
            if href is not None:
                tok.attrSet(
                    "href" if tok.type == "link_open" else "src",
                    rewrite_href(href, doc_dir),
                )
        if tok.children:
            rewrite_links(tok.children, doc_dir)


def render_markdown(doc_path: Path, doc_repo_rel: str) -> tuple[str, str | None]:
    # "gfm-like" for GFM tables + fenced code; linkify off (neither doc needs bare-URL
    # autolinking, and it would pull in the separate linkify-it-py dependency); raw HTML
    # off (nothing in these docs needs it, and it should stay that way without review).
    md = MarkdownIt("gfm-like", {"html": False}).disable("linkify")
    src = doc_path.read_text(encoding="utf-8")
    tokens = md.parse(src)
    doc_dir = Path(doc_repo_rel).parent
    rewrite_links(tokens, doc_dir)
    first_heading_id = assign_heading_ids(tokens)
    body = md.renderer.render(tokens, md.options, {})
    # Table wrapper for horizontal scroll on narrow viewports — plain string
    # substitution is safe here because we render our own docs, not arbitrary input.
    body = body.replace("<table>", '<div class="doc-table-wrap">\n<table>').replace(
        "</table>", "</table>\n</div>"
    )
    return body, first_heading_id


PAGE_TEMPLATE = """<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="description" content="{description}" />
    <title>{title}</title>
    <link rel="canonical" href="{canonical}" />
    <link rel="icon" type="image/png" href="{root}assets/logo-master.png" />
    <meta name="theme-color" content="#202022" />
    <link rel="stylesheet" href="{root}styles.css" />
  </head>
  <body>
    <div class="page-shell">
      <div class="ambient-ground" aria-hidden="true"></div>
      <main class="page-root">
        <header class="site-header">
          <a class="brand" href="{root}" aria-label="PlxNative home">
            <span class="brand-mark"><img src="{root}assets/logo-master.png" alt="" /></span>
            <span class="brand-name">PlxNative</span>
          </a>
          <div class="header-actions">
            <a class="header-back" href="{root}">&larr; Back to the site</a>
          </div>
        </header>

        <section class="doc-page"{aria_labelledby}>
          <div class="doc-body">
{body}
          </div>
        </section>

        <footer class="site-footer credits-footer">
          <p class="footer-meta">
            <span>PlxNative is an independent, unofficial client for Plex Media Server.</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span><a href="{edit_url}">Edit this page on GitHub</a></span>
          </p>
        </footer>
      </main>
    </div>
  </body>
</html>
"""


def render_page(md_path: str) -> str:
    doc_path = Path(md_path).resolve()
    doc_repo_rel = doc_path.relative_to(REPO_ROOT).as_posix()
    basename = doc_path.name
    page = PAGES.get(basename)
    if page is None:
        sys.exit(
            f"render-doc-page.py doesn't know how to render {basename!r}. "
            f"Add it to PAGES in {Path(__file__).name} first (route, title, description)."
        )

    body, first_heading_id = render_markdown(doc_path, doc_repo_rel)
    aria_labelledby = f' aria-labelledby="{first_heading_id}"' if first_heading_id else ""

    return PAGE_TEMPLATE.format(
        description=html.escape(page["description"], quote=True),
        title=html.escape(page["title"]),
        canonical=f"{SITE_ORIGIN}{page['route']}",
        root="/",
        aria_labelledby=aria_labelledby,
        body=body.rstrip("\n"),
        edit_url=f"{GITHUB_BLOB}/{doc_repo_rel}",
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} docs/<file>.md", file=sys.stderr)
        return 2
    sys.stdout.write(render_page(argv[1]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
