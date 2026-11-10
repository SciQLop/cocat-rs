"""Build ynotebook.bin and ynotebook.json: a notebook as jupyter_ydoc (what JupyterLab and
SciQLop use) writes it, with the notebook it was made from.

The Rust writer is checked against this: both documents must have the same Y types and values.

    python -m venv .venv && .venv/bin/pip install pycrdt jupyter_ydoc==4.2.1
    .venv/bin/python crates/cocat-core/tests/fixtures/make_ynotebook.py <output directory>

Commit both files this script writes.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from jupyter_ydoc import YNotebook

NOTEBOOK = {
    "nbformat": 4,
    "nbformat_minor": 5,
    "metadata": {
        "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"},
        "language_info": {"name": "python", "version": "3.13.1"},
        "widths": [1, 2.5],
    },
    "cells": [
        {
            "id": "md-1",
            "cell_type": "markdown",
            "metadata": {"tags": ["intro"]},
            "source": ["# Title\n", "text ![p](attachment:p.png)"],
            "attachments": {"p.png": {"image/png": "iVBORw0KGgo="}},
        },
        {
            "id": "md-2",
            "cell_type": "markdown",
            "metadata": {},
            "source": "no attachments",
            "attachments": {},
        },
        {
            "id": "code-1",
            "cell_type": "code",
            "metadata": {"collapsed": False, "scale": 2},
            "execution_count": 3,
            "source": ["import numpy as np\n", "print(np.pi)"],
            "outputs": [
                {"output_type": "stream", "name": "stdout", "text": ["3.14\n", "done\n"]},
                {"output_type": "stream", "name": "stderr", "text": "warning\n"},
                {
                    "output_type": "display_data",
                    "data": {"text/plain": "<Figure>", "image/png": "iVBORw0KGgo="},
                    "metadata": {"needs_background": "light", "size": [4, 2.5]},
                },
                {
                    "output_type": "execute_result",
                    "execution_count": 3,
                    "data": {"text/plain": ["1.5"], "text/html": "<b>1.5</b>"},
                    "metadata": {},
                },
                {
                    "output_type": "error",
                    "ename": "ValueError",
                    "evalue": "bad",
                    "traceback": ["\u001b[0;31mValueError\u001b[0m", "bad"],
                },
            ],
        },
        {
            "id": "code-2",
            "cell_type": "code",
            "metadata": {},
            "execution_count": None,
            "source": "",
            "outputs": [],
        },
        {"id": "raw-1", "cell_type": "raw", "metadata": {"format": "text/latex"}, "source": "\\alpha"},
    ],
}


def main(out: Path) -> None:
    notebook = YNotebook()
    notebook.set(NOTEBOOK)
    (out / "ynotebook.bin").write_bytes(notebook.ydoc.get_update())
    (out / "ynotebook.json").write_text(json.dumps(NOTEBOOK, indent=2) + "\n")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
