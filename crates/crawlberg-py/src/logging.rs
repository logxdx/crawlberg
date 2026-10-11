//! `init_logging` for Python, hand-written and declared through alef.toml's
//! `custom_modules` / `custom_registrations`, so `alef generate` keeps it.
//!
//! The CLI installs a tracing subscriber and the Python module installed none, so nothing the
//! core logs - including chromiumoxide's per-message CDP trace - could be seen from Python.
//! This is opt-in through the environment and off by default: with `CRAWLBERG_LOG` unset it
//! does nothing.

use std::fs::OpenOptions;
use std::sync::Mutex;

use pyo3::prelude::*;
use tracing_subscriber::EnvFilter;

/// Install a tracing subscriber when `CRAWLBERG_LOG` is set, filtered by its value
/// (`RUST_LOG` syntax, e.g. `crawlberg=debug,chromiumoxide::conn=trace`). Output goes to the file
/// named by `CRAWLBERG_LOG_FILE`, appended to, or to stderr. Returns whether a subscriber is now
/// installed by this call; a second call, or one after another subscriber, returns `False`.
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
pub fn init_logging() -> PyResult<bool> {
    let Ok(filter) = std::env::var("CRAWLBERG_LOG") else {
        return Ok(false);
    };
    let filter = EnvFilter::try_new(&filter)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("CRAWLBERG_LOG: {e}")))?;
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false);
    let installed = match std::env::var_os("CRAWLBERG_LOG_FILE") {
        Some(path) => {
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            builder.with_writer(Mutex::new(file)).try_init().is_ok()
        }
        None => builder.with_writer(std::io::stderr).try_init().is_ok(),
    };
    Ok(installed)
}
