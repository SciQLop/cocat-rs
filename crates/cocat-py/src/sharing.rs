//! Managing a project from Python: creating one, its members, invitations and groups, as
//! dicts shaped like the server's JSON; refusals raise the typed server errors.

use cocat_client::{INVITATION_DAYS, LoginError, NewInvitation};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::Serialize;

use crate::server_errors::http_error;
use crate::{ProjectInfo, Result, block_on, to_python};

/// Runs a server call off the GIL; its answer becomes dicts and lists, as JSON would.
fn answer<T: Serialize + Send>(
    py: Python<'_>,
    call: impl Future<Output = std::result::Result<T, LoginError>> + Send,
) -> Result<Py<PyAny>> {
    let value = py.detach(|| block_on(call))?.map_err(http_error)?;
    let json =
        serde_json::to_value(value).map_err(|error| PyValueError::new_err(error.to_string()))?;
    to_python(py, &json)
}

fn done(
    py: Python<'_>,
    call: impl Future<Output = std::result::Result<(), LoginError>> + Send,
) -> Result<()> {
    py.detach(|| block_on(call))?.map_err(http_error)
}

/// Makes a project owned by the caller.
#[pyfunction]
fn create_project(
    py: Python<'_>,
    url: String,
    cookie: String,
    name: String,
) -> Result<ProjectInfo> {
    let created = py
        .detach(|| block_on(cocat_client::create_project(&url, &cookie, &name)))?
        .map_err(http_error)?;
    Ok(created.into())
}

/// The members, as dicts with `user_id`, `email` and `role`.
#[pyfunction]
fn members(py: Python<'_>, url: String, cookie: String, project_id: String) -> Result<Py<PyAny>> {
    answer(py, cocat_client::members(&url, &cookie, &project_id))
}

#[pyfunction]
fn set_role(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    user_id: String,
    role: String,
) -> Result<()> {
    done(
        py,
        cocat_client::set_role(&url, &cookie, &project_id, &user_id, &role),
    )
}

#[pyfunction]
fn remove_member(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    user_id: String,
) -> Result<()> {
    done(
        py,
        cocat_client::remove_member(&url, &cookie, &project_id, &user_id),
    )
}

/// A new invitation, as a dict with `id`, `token` and `link`, the page that accepts it.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, role, email=None, expires_in_days=INVITATION_DAYS, max_uses=None))]
#[allow(clippy::too_many_arguments)]
fn invite(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    role: String,
    email: Option<String>,
    expires_in_days: u32,
    max_uses: Option<u32>,
) -> Result<Py<PyAny>> {
    let invitation = NewInvitation {
        email: email.as_deref(),
        role: &role,
        expires_in_days,
        max_uses,
    };
    answer(
        py,
        cocat_client::invite(&url, &cookie, &project_id, &invitation),
    )
}

/// The live invitations, as dicts with `id`, `email`, `role`, `expires_at`, `uses` and
/// `max_uses`.
#[pyfunction]
fn invitations(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
) -> Result<Py<PyAny>> {
    answer(py, cocat_client::invitations(&url, &cookie, &project_id))
}

#[pyfunction]
fn revoke_invitation(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    invitation_id: i64,
) -> Result<()> {
    done(
        py,
        cocat_client::revoke_invitation(&url, &cookie, &project_id, invitation_id),
    )
}

/// The groups granted on the project, as dicts with `id`, `name` and `role`.
#[pyfunction]
fn project_groups(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
) -> Result<Py<PyAny>> {
    answer(py, cocat_client::project_groups(&url, &cookie, &project_id))
}

/// Lets the project's editors share it as its owner does (`True`), or keeps sharing to the
/// owner (`False`). Only the owner may.
#[pyfunction]
fn set_editors_can_share(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    allowed: bool,
) -> Result<()> {
    done(
        py,
        cocat_client::set_editors_can_share(&url, &cookie, &project_id, allowed),
    )
}

/// The project's page in the web app; with `share`, the page opens its Share dialog.
#[pyfunction]
#[pyo3(signature = (url, project_id, share=false))]
fn project_url(url: String, project_id: String, share: bool) -> Result<String> {
    cocat_client::project_url(&url, &project_id, share).map_err(http_error)
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(create_project, module)?)?;
    module.add_function(wrap_pyfunction!(members, module)?)?;
    module.add_function(wrap_pyfunction!(set_role, module)?)?;
    module.add_function(wrap_pyfunction!(remove_member, module)?)?;
    module.add_function(wrap_pyfunction!(invite, module)?)?;
    module.add_function(wrap_pyfunction!(invitations, module)?)?;
    module.add_function(wrap_pyfunction!(revoke_invitation, module)?)?;
    module.add_function(wrap_pyfunction!(project_groups, module)?)?;
    module.add_function(wrap_pyfunction!(set_editors_can_share, module)?)?;
    module.add_function(wrap_pyfunction!(project_url, module)?)?;
    Ok(())
}
