//! What an event table's search box needs, the web app's filter box: what its text is, its
//! colours, completions and preset searches, and the rows to test against a search.

use cocat_core::model::Probe;
use cocat_core::query::search_box::{
    self, Completion, Piece, Query, Suggestion, Vocabulary, presets as preset_searches,
};
use numpy::PyArray1;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use crate::{Event, Result, to_timestamp, value_error};

/// `"empty"`, `"words"` (plain words: the plain filter), `"rule"` or `"broken"`; a
/// `catalogue:` name reads against `catalogues`, the project's `(uuid, name)` pairs.
pub fn kind_of(text: &str, catalogues: &[(String, String)]) -> &'static str {
    match search_box::read(text, catalogues) {
        Query::Empty => "empty",
        Query::Words => "words",
        Query::Rule(_) => "rule",
        Query::Broken(_) => "broken",
    }
}

/// What the text in a search box is: `"empty"`, `"words"`, `"rule"` or `"broken"`. A
/// catalogue named in it reads as broken here: `Project.query_kind` knows the names.
#[pyfunction]
fn query_kind(text: &str) -> &'static str {
    kind_of(text, &[])
}

/// The text cut into the parts a box colours, which joined give it back exactly: dicts
/// with `kind` (`space`, `keyword`, `bracket`, `word` or `term`) and `text`; a term also
/// has `field`, `op` (`:` and any comparison) and `value`.
#[pyfunction]
fn pieces(py: Python<'_>, text: &str) -> Result<Py<PyList>> {
    let parts = PyList::empty(py);
    for piece in search_box::pieces(text) {
        let part = PyDict::new(py);
        let kind = match &piece {
            Piece::Space(_) => "space",
            Piece::Keyword(_) => "keyword",
            Piece::Bracket(_) => "bracket",
            Piece::Word(_) => "word",
            Piece::Term { field, op, value } => {
                part.set_item("field", field)?;
                part.set_item("op", op)?;
                part.set_item("value", value)?;
                "term"
            }
        };
        part.set_item("kind", kind)?;
        part.set_item("text", piece.text())?;
        parts.append(part)?;
    }
    Ok(parts.unbind())
}

/// The suggestions for the word at `cursor` (a character index): a dict with `from` and
/// `to`, the characters they replace, and `suggestions`, each an `insert` and its `kind`;
/// `None` when there are none. `vocabulary` is `Project.vocabulary()`'s dict.
#[pyfunction]
fn complete(
    py: Python<'_>,
    text: &str,
    cursor: usize,
    vocabulary: &Bound<'_, PyAny>,
) -> Result<Py<PyAny>> {
    let vocabulary = Vocabulary {
        tags: words(vocabulary, "tags")?,
        authors: words(vocabulary, "authors")?,
        catalogues: words(vocabulary, "catalogues")?,
        attributes: words(vocabulary, "attributes")?,
    };
    let Some(found) = search_box::complete(text, cursor, &vocabulary) else {
        return Ok(py.None());
    };
    let suggestions = PyList::empty(py);
    for suggestion in &found.suggestions {
        let entry = PyDict::new(py);
        entry.set_item("insert", &suggestion.insert)?;
        entry.set_item("kind", suggestion.kind)?;
        suggestions.append(entry)?;
    }
    let completion = PyDict::new(py);
    completion.set_item("from", found.from)?;
    completion.set_item("to", found.to)?;
    completion.set_item("suggestions", suggestions)?;
    Ok(completion.into_any().unbind())
}

/// The list under `key` of a vocabulary dict; a missing key is an empty list.
fn words(vocabulary: &Bound<'_, PyAny>, key: &str) -> Result<Vec<String>> {
    match vocabulary.get_item(key) {
        Ok(found) => found.extract(),
        Err(_) => Ok(Vec::new()),
    }
}

/// The text with a suggestion of `completion` taken, and where the cursor goes after it.
#[pyfunction]
fn accept(
    py: Python<'_>,
    text: &str,
    completion: &Bound<'_, PyAny>,
    suggestion: &Bound<'_, PyAny>,
) -> Result<Py<PyTuple>> {
    let completion = Completion {
        from: completion.get_item("from")?.extract()?,
        to: completion.get_item("to")?.extract()?,
        suggestions: Vec::new(),
    };
    let chosen = Suggestion {
        insert: suggestion.get_item("insert")?.extract()?,
        kind: "",
    };
    let (text, cursor) = search_box::accept(text, &completion, &chosen);
    Ok(PyTuple::new(
        py,
        [
            text.into_pyobject(py)?.into_any(),
            cursor.into_pyobject(py)?.into_any(),
        ],
    )?
    .unbind())
}

/// The preset smart-catalogue searches the web app offers, for the person `me` (an
/// email): dicts with `name` and `query`.
#[pyfunction]
fn presets(py: Python<'_>, me: &str) -> Result<Py<PyList>> {
    let list = PyList::empty(py);
    for (name, query) in preset_searches(me) {
        let entry = PyDict::new(py);
        entry.set_item("name", name)?;
        entry.set_item("query", query)?;
        list.append(entry)?;
    }
    Ok(list.unbind())
}

/// The rows a table shows, to test: uuids or `Event`s, and `(start, stop)` for an interval
/// a rule derived, which has no uuid.
pub fn probes(items: &Bound<'_, PyAny>) -> Result<Vec<Probe>> {
    let nanos = |time: &Bound<'_, PyAny>| {
        to_timestamp(time)?
            .unix_nanos()
            .ok_or_else(|| value_error("a time out of range"))
    };
    items
        .try_iter()?
        .map(|item| {
            let item = item?;
            if let Ok(event) = item.cast::<Event>() {
                return Ok(Probe::Event(event.get().uuid.clone()));
            }
            if let Ok(uuid) = item.extract::<String>() {
                return Ok(Probe::Event(uuid));
            }
            let (start, stop) = item
                .extract::<(Bound<'_, PyAny>, Bound<'_, PyAny>)>()
                .map_err(|_| {
                    value_error(
                        "expected a uuid, an Event, or (start, stop) for an interval with no uuid",
                    )
                })?;
            Ok(Probe::Interval {
                start: nanos(&start)?,
                stop: nanos(&stop)?,
            })
        })
        .collect()
}

/// A numpy bool array.
pub fn mask_array(py: Python<'_>, mask: Vec<bool>) -> Py<PyAny> {
    PyArray1::from_vec(py, mask).into_any().unbind()
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(query_kind, module)?)?;
    module.add_function(wrap_pyfunction!(pieces, module)?)?;
    module.add_function(wrap_pyfunction!(complete, module)?)?;
    module.add_function(wrap_pyfunction!(accept, module)?)?;
    module.add_function(wrap_pyfunction!(presets, module)?)?;
    Ok(())
}
