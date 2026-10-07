"""The step manifest `make check-python-rest` runs, for the tests that pin what it contains.

`ci/check-python-steps.txt` lists the Python/shell/C gates; `tools/check-parallel.py --steps` runs
it and owns the format, so this reads it through that tool's own parser rather than a second one."""
from __future__ import annotations

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "ci" / "check-python-steps.txt"
RUNNER = ROOT / "tools" / "check-parallel.py"


def runner():
    spec = importlib.util.spec_from_file_location("check_parallel_for_tests", RUNNER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def commands() -> list[str]:
    """The manifest's commands in file order, comments and blank lines dropped."""
    return runner().read_steps(str(MANIFEST))
