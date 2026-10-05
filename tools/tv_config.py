"""The television's address / Wake-on-LAN MAC for Python callers: a thin shim over
`tools/tv-config.sh`, which is the single source of truth for where those values live (this
checkout's `.tv-host`/`.tv-mac`, the main checkout's, then the per-user config directory). There is
deliberately no second implementation of the lookup here -- see that script's header for the order.

Both functions return "" when nothing is configured (or when the script cannot be run), never raise.
`SCRIPT` is a module attribute so a test can point it at a sandbox copy.
"""
import os
import subprocess

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tv-config.sh")


def _ask(what: str) -> str:
    try:
        out = subprocess.run([SCRIPT, what], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.strip()


def host() -> str:
    """The configured TV address from the files, or ""."""
    return _ask("host")


def mac() -> str:
    """The configured Wake-on-LAN MAC from the files, or ""."""
    return _ask("mac")
