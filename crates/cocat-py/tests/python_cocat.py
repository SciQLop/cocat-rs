"""Where the Python cocat used by the cross-client tests lives.

`COCAT_PYTHON` is an interpreter with Python cocat installed (CI installs the released
package from PyPI); `COCAT_PYTHON_SRC`, when set, is put on `PYTHONPATH` to test a
checkout instead. Without them, the developer's local checkout is used when present.
"""

import os
from pathlib import Path

import pytest

LOCAL_PYTHON = "/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python"
LOCAL_SRC = "/home/jeandet/Documents/prog/cocat/src"


def interpreter() -> tuple[str, dict[str, str]]:
    """The interpreter and the environment to run a Python cocat script with."""
    python = os.environ.get("COCAT_PYTHON")
    source = os.environ.get("COCAT_PYTHON_SRC")
    if python is None and Path(LOCAL_PYTHON).exists():
        python, source = LOCAL_PYTHON, LOCAL_SRC
    if python is None or not Path(python).exists():
        # CI must run these tests; elsewhere they need an interpreter we may not have.
        if os.environ.get("CI"):
            pytest.fail("COCAT_PYTHON does not name an interpreter with Python cocat")
        pytest.skip("no Python cocat interpreter (set COCAT_PYTHON)")
    environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    if source:
        environment["PYTHONPATH"] = source
    return python, environment
