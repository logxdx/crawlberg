//! `shutdown_engine` for Python, hand-written and declared through alef.toml's
//! `custom_modules` / `custom_registrations`, so `alef generate` keeps it.
//!
//! alef skips `crawlberg::shutdown_engine` because it consumes the handle, which a Python object
//! cannot give up. The core handle is `Clone` and a shutdown through any clone closes the pool
//! they share, so a clone is shut down instead. Dropping the last reference to an engine closes
//! its browser as well; this is the deterministic form of that.

use pyo3::prelude::*;

use crate::CrawlEngineHandle;

/// Close the browser `create_engine` launched for this engine, and wait for Chrome to exit.
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
#[pyo3(signature = (engine))]
pub fn shutdown_engine<'py>(py: Python<'py>, engine: CrawlEngineHandle) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        crawlberg::shutdown_engine((*engine.inner).clone()).await;
        Ok(())
    })
}
