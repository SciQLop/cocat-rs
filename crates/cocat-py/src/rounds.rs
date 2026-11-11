//! Selection rounds from Python: their settings, members, state, proposals, votes and verdicts,
//! the cost of a list of events against a budget, and the rules of reviewing. Answers are
//! dicts shaped like the server's JSON.

use cocat_core::model::EventRecord;
use cocat_core::round::review::{self, ReviewRow, Verdict};
use cocat_core::round::{cost, parse_budget};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{Value, json};

use crate::sharing::{answer, done};
use crate::{Result, to_json, to_python, value_error};

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
#[pyo3(signature = (url, cookie, project, round, state, final_catalogue=None, note=None))]
#[allow(clippy::too_many_arguments)]
fn move_round(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    state: String,
    final_catalogue: Option<String>,
    note: Option<String>,
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
            note.as_deref(),
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

/// Sets (`accept`, `reject` or `unsure`) or clears (`None`) your verdict on one event.
#[pyfunction]
fn set_verdict(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    event: String,
    verdict: Option<String>,
) -> Result<()> {
    done(
        py,
        cocat_client::set_verdict(&url, &cookie, &project, &round, &event, verdict.as_deref()),
    )
}

/// Merges your verdicts: `{event uuid: "accept" | "reject" | "unsure" | None}`.
#[pyfunction]
fn set_verdicts(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
    verdicts: &Bound<'_, PyAny>,
) -> Result<()> {
    let verdicts = to_json(verdicts)?;
    done(
        py,
        cocat_client::set_verdicts(&url, &cookie, &project, &round, &verdicts),
    )
}

#[pyfunction]
fn verdicts(
    py: Python<'_>,
    url: String,
    cookie: String,
    project: String,
    round: String,
) -> Result<Py<PyAny>> {
    answer(py, cocat_client::verdicts(&url, &cookie, &project, &round))
}

/// An event dict holding at least `uuid`, `start` and `stop`; any other field of an event
/// (`tags`, `status`, `confidence`, `shape`, `attributes`…) is kept.
fn record_of(item: &Bound<'_, PyAny>) -> Result<EventRecord> {
    serde_json::from_value(to_json(item)?).map_err(value_error)
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

fn event_dict(py: Python<'_>, record: &EventRecord) -> Result<Py<PyAny>> {
    to_python(py, &serde_json::to_value(record).map_err(value_error)?)
}

fn verdict_name(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Accept => "accept",
        Verdict::Reject => "reject",
        Verdict::Unsure => "unsure",
    }
}

fn verdict_of(name: &str) -> Result<Verdict> {
    serde_json::from_value(Value::from(name)).map_err(value_error)
}

fn row_dict<'py>(py: Python<'py>, row: &ReviewRow) -> Result<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("event", event_dict(py, &row.event)?)?;
    dict.set_item("proposal", &row.proposal)?;
    dict.set_item("accepts", row.accepts)?;
    dict.set_item("rejects", row.rejects)?;
    dict.set_item("unsure", row.unsure)?;
    dict.set_item("mine", row.mine.map(verdict_name))?;
    dict.set_item("set_aside", row.set_aside)?;
    Ok(dict)
}

fn row_of(item: &Bound<'_, PyAny>) -> Result<ReviewRow> {
    let dict = item.cast::<PyDict>()?;
    let required = |key: &str| {
        dict.get_item(key)?
            .ok_or_else(|| value_error(format!("a review row needs a {key}")))
    };
    let mine = match required("mine")? {
        value if value.is_none() => None,
        value => Some(verdict_of(&value.extract::<String>()?)?),
    };
    Ok(ReviewRow {
        event: record_of(&required("event")?)?,
        proposal: required("proposal")?.extract()?,
        accepts: required("accepts")?.extract()?,
        rejects: required("rejects")?.extract()?,
        unsure: required("unsure")?.extract()?,
        mine,
        set_aside: required("set_aside")?.extract()?,
    })
}

fn rows_of(items: &[Bound<'_, PyAny>]) -> Result<Vec<ReviewRow>> {
    items.iter().map(row_of).collect()
}

/// The review rows of `proposals` (a list of `(author email or None for the candidates,
/// [event dicts])`) under `verdicts` (what `verdicts()` returned), for the caller `me`.
#[pyfunction]
fn review_rows<'py>(
    py: Python<'py>,
    proposals: Vec<(Option<String>, Vec<Bound<'py, PyAny>>)>,
    verdicts: &Bound<'py, PyAny>,
    me: &str,
) -> Result<Vec<Bound<'py, PyDict>>> {
    let proposals = proposals
        .iter()
        .map(|(author, events)| {
            let records = events.iter().map(record_of).collect::<Result<Vec<_>>>()?;
            Ok((author.clone(), records))
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = review::rows(&proposals, &to_json(verdicts)?, me).map_err(value_error)?;
    rows.iter().map(|row| row_dict(py, row)).collect()
}

/// `(done, total, set_aside)` of the review rows.
#[pyfunction]
fn review_progress(rows: Vec<Bound<'_, PyAny>>) -> Result<(usize, usize, usize)> {
    Ok(review::progress(&rows_of(&rows)?))
}

/// How many current rows any reviewer gave a verdict on.
#[pyfunction]
fn review_reviewed(rows: Vec<Bound<'_, PyAny>>) -> Result<usize> {
    Ok(review::reviewed(&rows_of(&rows)?))
}

/// The first row from `start` on, wrapping round, you have not reviewed.
#[pyfunction]
fn review_next_unreviewed(rows: Vec<Bound<'_, PyAny>>, start: usize) -> Result<Option<usize>> {
    Ok(review::next_unreviewed(&rows_of(&rows)?, start))
}

/// Which rows more reviewers accepted than rejected.
#[pyfunction]
fn review_majority(rows: Vec<Bound<'_, PyAny>>) -> Result<Vec<bool>> {
    Ok(review::majority(&rows_of(&rows)?))
}

/// The events of the final catalogue for the rows `picked` selects, as event dicts.
#[pyfunction]
fn review_final_events<'py>(
    py: Python<'py>,
    rows: Vec<Bound<'py, PyAny>>,
    picked: Vec<bool>,
) -> Result<Vec<Py<PyAny>>> {
    review::final_events(&rows_of(&rows)?, &picked)
        .iter()
        .map(|record| event_dict(py, record))
        .collect()
}

/// The uuid of the catalogue a round's decision writes.
#[pyfunction]
fn final_catalogue_uuid(round_id: &str) -> String {
    review::final_catalogue_uuid(round_id)
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
    module.add_function(wrap_pyfunction!(set_verdict, module)?)?;
    module.add_function(wrap_pyfunction!(set_verdicts, module)?)?;
    module.add_function(wrap_pyfunction!(verdicts, module)?)?;
    module.add_function(wrap_pyfunction!(review_rows, module)?)?;
    module.add_function(wrap_pyfunction!(review_progress, module)?)?;
    module.add_function(wrap_pyfunction!(review_reviewed, module)?)?;
    module.add_function(wrap_pyfunction!(review_next_unreviewed, module)?)?;
    module.add_function(wrap_pyfunction!(review_majority, module)?)?;
    module.add_function(wrap_pyfunction!(review_final_events, module)?)?;
    module.add_function(wrap_pyfunction!(final_catalogue_uuid, module)?)?;
    Ok(())
}
