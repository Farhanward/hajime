"""Reading brand.toml, with dotted keys and no silent blanks.

`brand("support.url")` returns the string or "". A missing *key* is a typo and
raises; an empty *value* is a deliberate "we do not have one of these", and the
caller drops the line rather than drawing a placeholder.
"""

from __future__ import annotations

import functools
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


@functools.lru_cache(maxsize=1)
def data() -> dict:
    with open(ROOT / "brand.toml", "rb") as fh:
        return tomllib.load(fh)


def brand(dotted: str) -> str:
    node = data()
    for part in dotted.split("."):
        if not isinstance(node, dict) or part not in node:
            raise KeyError(f"no {dotted!r} in brand.toml")
        node = node[part]
    return str(node)
