//! Submissions to the public catalogues from Python: offering a catalogue, proposing a
//! change on a copy, and the curators' review. Submissions come back as dicts shaped like the
//! server's JSON; refusals raise the typed server errors.

use cocat_client::NewSubmission;
use pyo3::prelude::*;

use crate::Result;
use crate::block_on;
use crate::server_errors::http_error;
use crate::sharing::answer;

/// Offers one of the caller's catalogues, as it is now, as a new public catalogue; the
/// submission's id. The title defaults to the catalogue's name.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, catalogue_uuid, title=None, folder=None, description=None, note=None))]
#[allow(clippy::too_many_arguments)]
fn submit_catalogue(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    catalogue_uuid: String,
    title: Option<String>,
    folder: Option<String>,
    description: Option<String>,
    note: Option<String>,
) -> Result<i64> {
    let new = NewSubmission {
        project_id: &project_id,
        catalogue: &catalogue_uuid,
        title: title.as_deref(),
        folder: folder.as_deref(),
        description: description.as_deref(),
        note: note.as_deref(),
    };
    let made = py
        .detach(|| block_on(cocat_client::submit_catalogue(&url, &cookie, &new)))?
        .map_err(http_error)?;
    Ok(made.id)
}

/// Copies a public catalogue, same uuids, into `project_id` or a new project named `name`,
/// to change it there; a dict with `project_id`, `name` (the project's) and `catalogue`.
#[pyfunction]
#[pyo3(signature = (url, cookie, public_id, project_id=None, name=None))]
fn propose_change(
    py: Python<'_>,
    url: String,
    cookie: String,
    public_id: String,
    project_id: Option<String>,
    name: Option<String>,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::propose_change(
            &url,
            &cookie,
            &public_id,
            project_id.as_deref(),
            name.as_deref(),
        ),
    )
}

/// Copies a public catalogue, same uuids and name, into `project_id` or a new project named
/// after it, to use it there; a dict with `project_id`, `name` (the project's) and
/// `catalogue`. The copy is not linked back, so it cannot be submitted as an update.
#[pyfunction]
#[pyo3(signature = (url, cookie, public_id, project_id=None))]
fn copy_public_catalogue(
    py: Python<'_>,
    url: String,
    cookie: String,
    public_id: String,
    project_id: Option<String>,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::copy_public_catalogue(&url, &cookie, &public_id, project_id.as_deref()),
    )
}

/// Offers a copy made with `propose_change`, as it is now, as the public catalogue's next
/// version; the submission's id.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, catalogue_uuid, note=None))]
fn submit_update(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    catalogue_uuid: String,
    note: Option<String>,
) -> Result<i64> {
    let made = py
        .detach(|| {
            block_on(cocat_client::submit_update(
                &url,
                &cookie,
                &project_id,
                &catalogue_uuid,
                note.as_deref(),
            ))
        })?
        .map_err(http_error)?;
    Ok(made.id)
}

/// The caller's own submissions; a curator gets everyone's unless `mine`. Pending ones
/// first, then the newest.
#[pyfunction]
#[pyo3(signature = (url, cookie, mine=false))]
fn submissions(py: Python<'_>, url: String, cookie: String, mine: bool) -> Result<Py<PyAny>> {
    answer(py, cocat_client::submissions(&url, &cookie, mine))
}

/// A curator accepts (the public catalogue takes it) or declines a pending submission,
/// with an optional note for its author; the submission as decided.
#[pyfunction]
#[pyo3(signature = (url, cookie, id, accept, note=None))]
fn review_submission(
    py: Python<'_>,
    url: String,
    cookie: String,
    id: i64,
    accept: bool,
    note: Option<String>,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::review_submission(&url, &cookie, id, accept, note.as_deref()),
    )
}

/// Its author takes back a pending submission; the submission as withdrawn.
#[pyfunction]
fn withdraw_submission(py: Python<'_>, url: String, cookie: String, id: i64) -> Result<Py<PyAny>> {
    answer(py, cocat_client::withdraw_submission(&url, &cookie, id))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(submit_catalogue, module)?)?;
    module.add_function(wrap_pyfunction!(propose_change, module)?)?;
    module.add_function(wrap_pyfunction!(copy_public_catalogue, module)?)?;
    module.add_function(wrap_pyfunction!(submit_update, module)?)?;
    module.add_function(wrap_pyfunction!(submissions, module)?)?;
    module.add_function(wrap_pyfunction!(review_submission, module)?)?;
    module.add_function(wrap_pyfunction!(withdraw_submission, module)?)?;
    Ok(())
}
