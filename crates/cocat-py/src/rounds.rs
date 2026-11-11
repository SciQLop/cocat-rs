//! Selection rounds from Python: their settings, members, state, proposals and votes, and the
//! cost of a list of events against a budget. Answers are dicts shaped like the server's JSON.

use cocat_core::model::EventRecord;
use cocat_core::round::{cost, parse_budget};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

use crate::sharing::{answer, done};
use crate::{Result, to_json, to_json_map, to_python, to_timestamp, value_error};

#[pyfunction]
fn rounds(py: Python<'_>, url: String, cookie: String, project: String) -> Result<Py<PyAny>> {
    answer(py, cocat_client::rounds(&url, &cookie, &project))
}

/// `body` holds `name`, `visibility` and optionally `deadline` and `budget`.
#[pyfunction]
fn create_round(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    body: &Bound<'_, PyAny>,
) -> Result<Py<PyAny>> {
    let body = to_json(body)?;
    answer(
        py,
        cocat_client::create_round(&url, &cookie, &project, &body),
    )
}

#[pyfunction]
fn round(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
) -> Result<Py<PyAny>> {
    answer(py, cocat_client::round(&url, &cookie, &project, &round))
}

/// `changes` holds only the settings to change; `None` for a deadline or budget clears it.
#[pyfunction]
fn update_round(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    changes: &Bound<'_, PyAny>,
) -> Result<Py<PyAny>> {
    let changes = to_json(changes)?;
    answer(
        py,
        cocat_client::update_round(&url, &cookie, &project, &round, &changes),
    )
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn set_round_members(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    proposers: Vec<String>,
    voters: Vec<String>,
    deciders: Vec<String>,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::set_round_members(
            &url, &cookie, &project, &round, &proposers, &voters, &deciders,
        ),
    )
}

#[pyfunction]
#[pyo3(signature = (url, cookie, project, round, state, final_catalogue=None))]
fn move_round(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    state: String,
    final_catalogue: Option<String>,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::move_round(
            &url,
            &cookie,
            &project,
            &round,
            &state,
            final_catalogue.as_deref(),
        ),
    )
}

/// The caller's proposal document of the round, made on first use; its uuid.
#[pyfunction]
fn my_proposal(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
) -> Result<Py<PyAny>> {
    answer(
        py,
        cocat_client::my_proposal(&url, &cookie, &project, &round),
    )
}

#[pyfunction]
fn set_votes(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    events: Vec<String>,
) -> Result<()> {
    done(
        py,
        cocat_client::set_votes(&url, &cookie, &project, &round, &events),
    )
}

#[pyfunction]
fn votes(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
) -> Result<Py<PyAny>> {
    answer(py, cocat_client::votes(&url, &cookie, &project, &round))
}

fn record_of(item: &Bound<'_, PyAny>) -> Result<EventRecord> {
    let dict = item.cast::<PyDict>()?;
    let required = |key: &str| {
        dict.get_item(key)?
            .ok_or_else(|| value_error(format!("an event needs a {key}")))
    };
    let mut record = EventRecord::new(
        required("uuid")?.extract::<String>()?,
        to_timestamp(&required("start")?)?,
        to_timestamp(&required("stop")?)?,
    );
    if let Some(attributes) = dict.get_item("attributes")? {
        record.attributes = to_json_map(attributes.cast::<PyDict>()?)?;
    }
    Ok(record)
}

/// What the events (dicts with `uuid`, `start`, `stop` and `attributes`) cost under the
/// budget (the round's, as JSON-LD): `total`, `cap`, `unit`, `unpriced` and `text`.
#[pyfunction]
fn round_cost(
    py: Python<'_>,
    events: Vec<Bound<'_, PyAny>>,
    budget: &Bound<'_, PyAny>,
) -> Result<Py<PyAny>> {
    let records = events.iter().map(record_of).collect::<Result<Vec<_>>>()?;
    let budget = parse_budget(&to_json(budget)?).map_err(value_error)?;
    let cost = cost(&records, &budget);
    to_python(
        py,
        &json!({
            "total": cost.total,
            "cap": cost.cap,
            "unit": cost.notation,
            "unpriced": cost.unpriced,
            "text": cost.text(),
        }),
    )
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(rounds, module)?)?;
    module.add_function(wrap_pyfunction!(create_round, module)?)?;
    module.add_function(wrap_pyfunction!(round, module)?)?;
    module.add_function(wrap_pyfunction!(update_round, module)?)?;
    module.add_function(wrap_pyfunction!(set_round_members, module)?)?;
    module.add_function(wrap_pyfunction!(move_round, module)?)?;
    module.add_function(wrap_pyfunction!(my_proposal, module)?)?;
    module.add_function(wrap_pyfunction!(set_votes, module)?)?;
    module.add_function(wrap_pyfunction!(votes, module)?)?;
    module.add_function(wrap_pyfunction!(round_cost, module)?)?;
    Ok(())
}
