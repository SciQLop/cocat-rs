//! The classes a refused HTTP call raises: one per status a caller acts on, each carrying
//! the server's `status` and its `detail` text, which is also the message.

use cocat_client::LoginError;
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyTuple, PyType};

create_exception!(
    cocat_rs,
    ServerError,
    PyValueError,
    "The server refused the call; `status` is its HTTP status, `detail` its own text."
);
create_exception!(
    cocat_rs,
    Unauthorized,
    ServerError,
    "401: the session expired or there is none; log in again."
);
create_exception!(
    cocat_rs,
    Forbidden,
    ServerError,
    "403: the account's role does not allow this."
);
create_exception!(
    cocat_rs,
    Conflict,
    ServerError,
    "409: the call clashes with the room's state, e.g. nothing to save."
);

static SERVER_NOT_FOUND: PyOnceLock<Py<PyType>> = PyOnceLock::new();

/// The class for a 404, also a [`crate::not_found`] class so one `except NotFound` covers a
/// missing room and a missing event; `create_exception!` takes a single base, hence `type()`.
fn server_not_found(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    SERVER_NOT_FOUND
        .get_or_try_init(py, || {
            let bases = PyTuple::new(
                py,
                [py.get_type::<ServerError>(), crate::not_found(py)?.clone()],
            )?;
            let namespace = PyDict::new(py);
            namespace.set_item("__doc__", "404: no such room or version on the server.")?;
            namespace.set_item("__module__", "cocat_rs")?;
            let class = py.import("builtins")?.getattr("type")?.call1((
                "ServerNotFound",
                bases,
                namespace,
            ))?;
            Ok(class.cast_into::<PyType>()?.unbind())
        })
        .map(|class| class.bind(py))
}

fn class_for(py: Python<'_>, status: u16) -> PyResult<Bound<'_, PyType>> {
    Ok(match status {
        401 => py.get_type::<Unauthorized>(),
        403 => py.get_type::<Forbidden>(),
        404 => server_not_found(py)?.clone(),
        409 => py.get_type::<Conflict>(),
        _ => py.get_type::<ServerError>(),
    })
}

fn refused(py: Python<'_>, status: u16, detail: String) -> PyResult<PyErr> {
    let error = PyErr::from_type(class_for(py, status)?, (detail.clone(),));
    let value = error.value(py);
    value.setattr("status", status)?;
    value.setattr("detail", detail)?;
    Ok(error)
}

/// A refusal becomes its typed class; anything else (no connection, a bad url) stays a
/// `ValueError`.
pub fn http_error(error: LoginError) -> PyErr {
    match error {
        LoginError::Refused { status, detail } => {
            Python::attach(|py| refused(py, status, detail).unwrap_or_else(|failure| failure))
        }
        other => PyValueError::new_err(other.to_string()),
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add("ServerError", py.get_type::<ServerError>())?;
    module.add("Unauthorized", py.get_type::<Unauthorized>())?;
    module.add("Forbidden", py.get_type::<Forbidden>())?;
    module.add("ServerNotFound", server_not_found(py)?)?;
    module.add("Conflict", py.get_type::<Conflict>())?;
    Ok(())
}
