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
import json
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

# Same card every other page reuses for link previews (site/media/og-card.jpg, staged to
# _site/media/og-card.jpg by pages.yml); these two docs have no screenshot of their own.
OG_IMAGE = f"{SITE_ORIGIN}/media/og-card.jpg"
OG_IMAGE_ALT = "PlxNative home screen on an LG TV, next to the headline: Plex that feels fast on LG TVs."

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


def assign_heading_ids(tokens: list[Token]) -> tuple[str | None, list[tuple[int, str, str]]]:
    """Walk the top-level token stream, giving every heading_open token a GitHub-compatible
    `id`, deduplicated the same way GitHub does (a repeat gets -1, -2, ...). Returns the id
    assigned to the first heading (the page's own <h1>), or None if there isn't one, plus every
    heading as (level, id, text) in document order — the level-2 ones are how the HowTo JSON-LD
    below finds install-and-verify.md's numbered steps without hard-coding their text twice."""
    seen: dict[str, int] = {}
    first_id: str | None = None
    headings: list[tuple[int, str, str]] = []
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
        headings.append((int(tok.tag[1:]), slug, inline.content))
    return first_id, headings


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


def render_markdown(doc_path: Path, doc_repo_rel: str) -> tuple[str, str | None, list[tuple[int, str, str]]]:
    # "gfm-like" for GFM tables + fenced code; linkify off (neither doc needs bare-URL
    # autolinking, and it would pull in the separate linkify-it-py dependency); raw HTML
    # off (nothing in these docs needs it, and it should stay that way without review).
    md = MarkdownIt("gfm-like", {"html": False}).disable("linkify")
    src = doc_path.read_text(encoding="utf-8")
    tokens = md.parse(src)
    doc_dir = Path(doc_repo_rel).parent
    rewrite_links(tokens, doc_dir)
    first_heading_id, headings = assign_heading_ids(tokens)
    body = md.renderer.render(tokens, md.options, {})
    # Table wrapper for horizontal scroll on narrow viewports — plain string
    # substitution is safe here because we render our own docs, not arbitrary input.
    body = body.replace("<table>", '<div class="doc-table-wrap">\n<table>').replace(
        "</table>", "</table>\n</div>"
    )
    return body, first_heading_id, headings


PAGE_TEMPLATE = """<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="description" content="{description}" />
    <meta name="robots" content="index,follow,max-image-preview:large" />
    <title>{title}</title>
    <link rel="canonical" href="{canonical}" />
    <link rel="icon" type="image/png" href="{root}assets/logo-master.png" />
    <meta name="theme-color" content="#202022" />
    <meta property="og:type" content="article" />
    <meta property="og:site_name" content="PlxNative" />
    <meta property="og:url" content="{canonical}" />
    <meta property="og:title" content="{title}" />
    <meta property="og:description" content="{description}" />
    <meta property="og:image" content="{og_image}" />
    <meta property="og:image:type" content="image/jpeg" />
    <meta property="og:image:width" content="1200" />
    <meta property="og:image:height" content="630" />
    <meta property="og:image:alt" content="{og_image_alt}" />
    <meta property="og:locale" content="en_US" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:title" content="{title}" />
    <meta name="twitter:description" content="{description}" />
    <meta name="twitter:image" content="{og_image}" />
    <meta name="twitter:image:alt" content="{og_image_alt}" />
    <link rel="stylesheet" href="{root}styles.css" />
    <script type="application/ld+json">
{schema_json}
    </script>
    <script type="application/ld+json">
{breadcrumb_json}
    </script>
  </head>
  <body>
    <div class="page-shell">
      <div class="ambient-ground" aria-hidden="true"></div>
      <main class="page-root">
        <header class="site-header">
          <div class="header-bar">
            <a class="brand" href="{root}" aria-label="Back to PlxNative">
              <span class="brand-mark"><img src="{root}assets/logo-master.png" alt="" /></span>
              <span class="brand-name"><span class="back-arrow" aria-hidden="true">&larr;</span> PlxNative</span>
            </a>
            <nav class="site-nav" aria-label="Primary navigation">
              <a href="{root}#feel">Demo</a>
              <a href="{root}#why">Why</a>
              <a href="{root}#showcase">Features</a>
            </nav>
            <div class="header-actions">
              <a class="header-github" href="https://github.com/GLinnik21/plx-native" aria-label="PlxNative on GitHub">
                <svg class="gh-icon" viewBox="0 0 24 24" width="21" height="21" fill="currentColor" aria-hidden="true"><path d="M12 1.5a10.5 10.5 0 0 0-3.32 20.46c.53.1.72-.23.72-.5v-1.76c-2.92.64-3.54-1.4-3.54-1.4-.48-1.22-1.17-1.55-1.17-1.55-.95-.65.07-.64.07-.64 1.06.08 1.61 1.09 1.61 1.09.94 1.6 2.46 1.14 3.06.87.1-.68.37-1.14.67-1.4-2.33-.27-4.78-1.17-4.78-5.2 0-1.15.41-2.09 1.08-2.83-.11-.27-.47-1.34.1-2.79 0 0 .88-.28 2.88 1.08a9.98 9.98 0 0 1 5.24 0c2-1.36 2.88-1.08 2.88-1.08.57 1.45.21 2.52.1 2.79.67.74 1.08 1.68 1.08 2.83 0 4.04-2.46 4.93-4.8 5.19.38.33.72.97.72 1.96v2.9c0 .28.19.62.73.51A10.5 10.5 0 0 0 12 1.5Z"></path></svg>
              </a>
              <a class="header-cta" href="{root}#install">Install</a>
            </div>
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


def breadcrumb_json(page: dict, canonical: str) -> str:
    """Home -> this page, for the rich-result breadcrumb trail. Page titles are all
    "<name> — PlxNative"; the crumb wants just <name>."""
    name = page["title"].split(" — ", 1)[0]
    data = {
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Home", "item": f"{SITE_ORIGIN}/"},
            {"@type": "ListItem", "position": 2, "name": name, "item": canonical},
        ],
    }
    return json.dumps(data, indent=2)


_STEP_HEADING = re.compile(r"^\d+\.\s*(.+)$")


def article_json(basename: str, page: dict, canonical: str, headings: list[tuple[int, str, str]]) -> str:
    """TechArticle for most docs; install-and-verify.md really is a numbered walkthrough (its
    own top-level headings are "1. ...", "2. ..."), so it gets HowTo with those as steps instead
    — Google's own guidance is HowTo only for content that is actually sequential steps."""
    name = page["title"].split(" — ", 1)[0]
    steps = [
        {"@type": "HowToStep", "name": m.group(1), "url": f"{canonical}#{slug}"}
        for level, slug, text in headings
        if level == 2
        for m in [_STEP_HEADING.match(text)]
        if m
    ]
    if basename == "install-and-verify.md" and steps:
        data = {
            "@context": "https://schema.org",
            "@type": "HowTo",
            "name": name,
            "description": page["description"],
            "url": canonical,
            "image": OG_IMAGE,
            "step": steps,
        }
    else:
        data = {
            "@context": "https://schema.org",
            "@type": "TechArticle",
            "headline": name,
            "description": page["description"],
            "url": canonical,
            "mainEntityOfPage": canonical,
            "image": OG_IMAGE,
            "author": {"@type": "Person", "name": "Gleb Linnik"},
        }
    return json.dumps(data, indent=2)


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

    body, first_heading_id, headings = render_markdown(doc_path, doc_repo_rel)
    aria_labelledby = f' aria-labelledby="{first_heading_id}"' if first_heading_id else ""
    canonical = f"{SITE_ORIGIN}{page['route']}"

    return PAGE_TEMPLATE.format(
        description=html.escape(page["description"], quote=True),
        title=html.escape(page["title"]),
        canonical=canonical,
        root="/",
        og_image=OG_IMAGE,
        og_image_alt=html.escape(OG_IMAGE_ALT, quote=True),
        schema_json=article_json(basename, page, canonical, headings),
        breadcrumb_json=breadcrumb_json(page, canonical),
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
