"""``shutdown_engine``, hand-written and re-exported through alef.toml's
``extra_init_imports``: alef skips the Rust function because it consumes the handle.
See crates/crawlberg-py/src/shutdown.rs.
"""

from __future__ import annotations

import crawlberg._crawlberg as _rust
from ._crawlberg import CrawlEngineHandle


async def shutdown_engine(engine: CrawlEngineHandle) -> None:
    """Close the browser this engine launched and wait until Chrome has exited.

    Shuts down every handle sharing the engine's browser. Dropping the last reference to an
    engine closes its browser too, but only when the garbage collector gets to it.
    """
    await _rust.shutdown_engine(engine=engine)
