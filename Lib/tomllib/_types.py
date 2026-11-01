"""Type aliases used in the annotations of tomllib._parser."""

from typing import Any, Callable, Tuple

# A function that turns the source text of a TOML float into a value.
ParseFloat = Callable[[str], Any]
# A table path inside the document, e.g. ("server", "ports").
Key = Tuple[str, ...]
# An index into the document string.
Pos = int
