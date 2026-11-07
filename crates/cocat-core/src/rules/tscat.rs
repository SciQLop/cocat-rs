//! tscat's predicate JSON (`tscat/filtering.py`, `Predicate.to_dict`) as a rule, so a tscat
//! dynamic catalogue imports as a smart catalogue.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use super::{Op, Predicate, RuleError, Set, validate};
use crate::time::Timestamp;

#[derive(Debug, Error)]
pub enum TscatError {
    #[error("not a tscat predicate: {0}")]
    Format(#[source] serde_json::Error),
    #[error("{0} is not a datetime between 1677 and 2262")]
    Datetime(Value),
    #[error(transparent)]
    Rule(#[from] RuleError),
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Tscat {
    Comparison {
        op: Op,
        lhs: Member,
        rhs: Value,
        #[serde(default)]
        rhs_type: Option<String>,
    },
    Match {
        lhs: Member,
        rhs: String,
    },
    Not {
        operand: Box<Tscat>,
    },
    Has {
        operand: Member,
    },
    All {
        predicates: Vec<Tscat>,
    },
    Any {
        predicates: Vec<Tscat>,
    },
    In {
        lhs: Value,
        rhs: Member,
    },
    #[serde(rename = "UUID")]
    Uuid {
        uuid: String,
    },
    InCatalogue {
        catalogue_uuid: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Member {
    Field { name: String },
    Attribute { name: String },
}

/// The rule `{"filter": {"all": true}, "where": ...}` of a tscat predicate. tscat's `rating`
/// (1–10) becomes `confidence` (0–1) and its datetimes become nanoseconds.
pub fn rule_from_tscat(predicate: &Value) -> Result<Value, TscatError> {
    let tscat = Tscat::deserialize(predicate).map_err(TscatError::Format)?;
    let rule = Set::Filter {
        filter: Box::new(Set::All { all: true }),
        predicate: convert(tscat)?,
    };
    validate(&rule)?;
    serde_json::to_value(&rule).map_err(TscatError::Format)
}

fn convert(tscat: Tscat) -> Result<Predicate, TscatError> {
    Ok(match tscat {
        Tscat::Comparison {
            op,
            lhs,
            rhs,
            rhs_type,
        } => comparison(op, &lhs, rhs, rhs_type.as_deref())?,
        Tscat::Match { lhs, rhs } => Predicate::Match(field(&lhs), rhs),
        Tscat::Not { operand } => Predicate::Not(Box::new(convert(*operand)?)),
        Tscat::Has { operand } => Predicate::Has(field(&operand)),
        Tscat::All { predicates } => Predicate::And(convert_all(predicates)?),
        Tscat::Any { predicates } => Predicate::Or(convert_all(predicates)?),
        Tscat::In { lhs, rhs } => Predicate::Contains(field(&rhs), lhs),
        Tscat::Uuid { uuid } => Predicate::Cmp("uuid".to_owned(), Op::Eq, uuid.into()),
        Tscat::InCatalogue { catalogue_uuid } => Predicate::InCatalogue(catalogue_uuid),
    })
}

fn convert_all(predicates: Vec<Tscat>) -> Result<Vec<Predicate>, TscatError> {
    predicates.into_iter().map(convert).collect()
}

fn comparison(
    op: Op,
    lhs: &Member,
    rhs: Value,
    rhs_type: Option<&str>,
) -> Result<Predicate, TscatError> {
    let rhs = match rhs_type {
        Some("datetime") => nanoseconds(&rhs)?,
        _ => rhs,
    };
    let rhs = match (lhs, rhs.as_f64()) {
        (Member::Field { name }, Some(rating)) if name == "rating" => (rating / 10.0).into(),
        _ => rhs,
    };
    Ok(Predicate::Cmp(field(lhs), op, rhs))
}

fn field(member: &Member) -> String {
    match member {
        Member::Field { name } if name == "rating" => "confidence".to_owned(),
        Member::Field { name } => name.clone(),
        Member::Attribute { name } => format!("#{name}"),
    }
}

/// `datetime.isoformat()` text as nanoseconds since 1970; a naive datetime is UTC.
fn nanoseconds(text: &Value) -> Result<Value, TscatError> {
    text.as_str()
        .and_then(Timestamp::parse)
        .and_then(Timestamp::unix_nanos)
        .map(Value::from)
        .ok_or_else(|| TscatError::Datetime(text.clone()))
}
