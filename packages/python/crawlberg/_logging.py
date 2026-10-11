"""``init_logging``, hand-written and re-exported through alef.toml's ``extra_init_imports``.
See crates/crawlberg-py/src/logging.rs.

Called once on import, so setting ``CRAWLBERG_LOG`` (and optionally ``CRAWLBERG_LOG_FILE``)
before ``import crawlberg`` is enough. Without ``CRAWLBERG_LOG`` it does nothing.
"""

from __future__ import annotations

import crawlberg._crawlberg as _rust


def init_logging() -> bool:
    """Install the core's tracing subscriber if ``CRAWLBERG_LOG`` is set. See logging.rs."""
    return _rust.init_logging()


init_logging()
