//! Browser sign-in from Python: the request with its link and code, its poll, and signing a
//! token out. `cocat_rs/client.py` drives them.

use pyo3::prelude::*;

use crate::Result;
use crate::sharing::{answer, done};

/// Asks the server for a sign-in request for the app called `name`; a dict with `code`, `url`,
/// `poll`, `expires_in` and `interval`.
#[pyfunction]
fn start_sign_in(py: Python<'_>, url: String, name: String) -> Result<Py<PyAny>> {
    answer(py, cocat_client::start_sign_in(&url, &name))
}

/// Where the request stands: a dict with `status` (`pending`, `denied`, `expired` or
/// `approved`) and, once approved, `token`.
#[pyfunction]
fn poll_sign_in(py: Python<'_>, url: String, poll: String) -> Result<Py<PyAny>> {
    answer(py, cocat_client::poll_sign_in(&url, &poll))
}

/// Ends the session the token (or cookie) carries on the server.
#[pyfunction]
fn sign_out(py: Python<'_>, url: String, token: String) -> Result<()> {
    done(py, cocat_client::sign_out(&url, &token))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(start_sign_in, module)?)?;
    module.add_function(wrap_pyfunction!(poll_sign_in, module)?)?;
    module.add_function(wrap_pyfunction!(sign_out, module)?)?;
    Ok(())
}
