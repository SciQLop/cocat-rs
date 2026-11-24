//! What a smart-catalogue editor needs from the search language: the builder's vocabulary
//! (fields, tests, the text a row writes, the rows a rule reads back into), shared with the
//! web app, and errors that say where a search goes wrong.

use cocat_core::query::builder::{self, Row, Test};
use cocat_core::query::{self, FIELDS, KINDS, Kind};
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use crate::{Result, to_json, value_error};

create_exception!(
    cocat_rs,
    QueryError,
    PyValueError,
    "A search that does not read. `position`: where, a character index from 0; `length`: how \
     many characters the wrong part spans, or None; `message`: what is wrong; `suggestion`: \
     the field likely meant, or None."
);

/// The Python `QueryError` of `error`, its parts as attributes.
pub fn query_error(error: query::QueryError) -> PyErr {
    Python::attach(|py| {
        let raised = QueryError::new_err(error.to_string());
        let value = raised.value(py);
        let set = value
            .setattr("position", error.position)
            .and_then(|()| value.setattr("length", error.length))
            .and_then(|()| value.setattr("message", error.message.as_str()))
            .and_then(|()| value.setattr("suggestion", error.suggestion.as_deref()));
        match set {
            Ok(()) => raised,
            Err(failure) => failure,
        }
    })
}

/// The fields a search can name, in menu order: `name`, `kind` and `label`, plus `values`
/// for a `choice`. Attributes (`#key`, kind `attribute`) come on top, from the project.
#[pyfunction]
fn query_fields(py: Python<'_>) -> Result<Py<PyList>> {
    let fields = PyList::empty(py);
    for field in FIELDS {
        let entry = PyDict::new(py);
        entry.set_item("name", field.name)?;
        entry.set_item("kind", field.kind.name())?;
        entry.set_item("label", builder::field_label(field.name))?;
        if let Kind::Enum(values) = field.kind {
            entry.set_item("values", values.to_vec())?;
        }
        fields.append(entry)?;
    }
    Ok(fields.unbind())
}

/// The tests a kind of field offers, the first the default: `test`, its `label` and whether
/// it `needs_value`.
#[pyfunction]
fn query_tests(py: Python<'_>, kind: &str) -> Result<Py<PyList>> {
    let kind = Kind::named(kind).ok_or_else(|| {
        let names: Vec<&str> = KINDS.iter().map(|kind| kind.name()).collect();
        value_error(format!("kind is one of {}", names.join(", ")))
    })?;
    let tests = PyList::empty(py);
    for &test in builder::tests(kind) {
        let entry = PyDict::new(py);
        entry.set_item("test", test.name())?;
        entry.set_item("label", builder::label(kind, test))?;
        entry.set_item("needs_value", test.needs_value())?;
        tests.append(entry)?;
    }
    Ok(tests.unbind())
}

/// One builder row as search text, e.g. `clause("confidence", "at_least", "0.8")` is
/// `confidence:>=0.8`.
#[pyfunction]
#[pyo3(signature = (field, test, value=""))]
fn clause(field: &str, test: &str, value: &str) -> Result<String> {
    let kind = query::kind_of(field).ok_or_else(|| {
        value_error(format!(
            "there is no field “{field}”: see query_fields(), or #key for an attribute"
        ))
    })?;
    let offered = builder::tests(kind);
    let test = Test::named(test)
        .filter(|test| offered.contains(test))
        .ok_or_else(|| {
            let names: Vec<&str> = offered.iter().map(|test| test.name()).collect();
            value_error(format!("{field} takes {}", names.join(", ")))
        })?;
    if test.needs_value() && value.trim().is_empty() {
        return Err(value_error(format!(
            "“{field} {}” needs a value",
            test.name()
        )));
    }
    Ok(builder::clause(field, test, value))
}

/// The rows' clauses as one search: all of them, or with `any=True` any of them.
#[pyfunction]
#[pyo3(signature = (clauses, any=false))]
fn search_text(clauses: Vec<String>, any: bool) -> String {
    builder::search_text(&clauses, any)
}

/// The rows of a rule's search, `(any, rows)`, or `None` when it is not a flat search.
/// Catalogues come back as uuids; `Project.builder_rows` names them.
#[pyfunction]
fn builder_rows(py: Python<'_>, rule: &Bound<'_, PyAny>) -> Result<Py<PyAny>> {
    rows_to_python(py, builder::rule_rows(&to_json(rule)?, &[]))
}

/// `(any, [{"field", "test", "value"}, ...])`, or `None`.
pub fn rows_to_python(py: Python<'_>, found: Option<(bool, Vec<Row>)>) -> Result<Py<PyAny>> {
    let Some((any, rows)) = found else {
        return Ok(py.None());
    };
    let list = PyList::empty(py);
    for row in rows {
        let entry = PyDict::new(py);
        entry.set_item("field", row.field)?;
        entry.set_item("test", row.test.name())?;
        entry.set_item("value", row.value)?;
        list.append(entry)?;
    }
    let pair = PyTuple::new(
        py,
        [
            any.into_pyobject(py)?.to_owned().into_any(),
            list.into_any(),
        ],
    )?;
    Ok(pair.into_any().unbind())
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add("QueryError", py.get_type::<QueryError>())?;
    module.add_function(wrap_pyfunction!(query_fields, module)?)?;
    module.add_function(wrap_pyfunction!(query_tests, module)?)?;
    module.add_function(wrap_pyfunction!(clause, module)?)?;
    module.add_function(wrap_pyfunction!(search_text, module)?)?;
    module.add_function(wrap_pyfunction!(builder_rows, module)?)?;
    Ok(())
}
