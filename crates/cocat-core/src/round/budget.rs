//! A round's telemetry budget: a cap, and a rate per value of an event attribute.

use serde_json::{Value, json};

use super::units::{Unit, UnitError, parse_unit};
use crate::model::EventRecord;

// simplify: reads one compact JSON-LD shape, no JSON-LD expansion; add a JSON-LD processor if other tools write budgets differently.

#[derive(Debug, Clone, PartialEq)]
pub struct Quantity {
    pub value: f64,
    pub notation: String,
    pub unit: Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub attribute: String,
    pub cap: Quantity,
    pub rates: Vec<(String, Quantity)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    #[error("{0} is missing or has the wrong type")]
    Field(String),
    #[error("{field}: {source}")]
    Unit { field: String, source: UnitError },
    #[error("cocat:cap must be an amount of data, not a rate")]
    CapIsARate,
}

pub fn parse_budget(json: &Value) -> Result<Budget, BudgetError> {
    let attribute = text(json, "cocat:attribute", "cocat:attribute")?;
    let cap = quantity(field(json, "cocat:cap", "cocat:cap")?, "cocat:cap")?;
    if cap.unit.per_seconds.is_some() {
        return Err(BudgetError::CapIsARate);
    }
    let rates = field(json, "cocat:rates", "cocat:rates")?
        .as_array()
        .ok_or_else(|| BudgetError::Field("cocat:rates".into()))?
        .iter()
        .map(rate)
        .collect::<Result<_, _>>()?;
    Ok(Budget {
        attribute,
        cap,
        rates,
    })
}

fn field<'a>(json: &'a Value, key: &str, path: &str) -> Result<&'a Value, BudgetError> {
    json.get(key)
        .ok_or_else(|| BudgetError::Field(path.to_owned()))
}

fn text(json: &Value, key: &str, path: &str) -> Result<String, BudgetError> {
    field(json, key, path)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| BudgetError::Field(path.to_owned()))
}

fn rate(entry: &Value) -> Result<(String, Quantity), BudgetError> {
    let value = text(entry, "cocat:value", "cocat:rates[].cocat:value")?;
    let rate = field(entry, "cocat:rate", "cocat:rates[].cocat:rate")?;
    Ok((value, quantity(rate, "cocat:rates[].cocat:rate")?))
}

fn quantity(json: &Value, path: &str) -> Result<Quantity, BudgetError> {
    let value = field(
        json,
        "qudt:numericValue",
        &format!("{path}.qudt:numericValue"),
    )?
    .as_f64()
    .ok_or_else(|| BudgetError::Field(format!("{path}.qudt:numericValue")))?;
    let unit_path = format!("{path}.qudt:hasUnit.skos:notation");
    let unit = field(json, "qudt:hasUnit", &format!("{path}.qudt:hasUnit"))?;
    let notation = text(unit, "skos:notation", &unit_path)?;
    let unit = parse_unit(&notation).map_err(|source| BudgetError::Unit {
        field: unit_path,
        source,
    })?;
    Ok(Quantity {
        value,
        notation,
        unit,
    })
}

pub fn budget_json(budget: &Budget) -> Value {
    json!({
        "@context": {
            "qudt": "https://qudt.org/schema/qudt/",
            "skos": "http://www.w3.org/2004/02/skos/core#",
            "dcterms": "http://purl.org/dc/terms/",
            "cocat": "https://cocat.sciqlop.fr/ns/round#",
        },
        "@type": "cocat:Budget",
        "cocat:attribute": budget.attribute,
        "cocat:cap": quantity_json(&budget.cap),
        "cocat:rates": budget.rates.iter().map(|(value, rate)| json!({
            "cocat:value": value,
            "cocat:rate": quantity_json(rate),
        })).collect::<Vec<_>>(),
    })
}

fn quantity_json(quantity: &Quantity) -> Value {
    json!({
        "@type": "qudt:QuantityValue",
        "qudt:numericValue": quantity.value,
        "qudt:hasUnit": {
            "@type": "qudt:Unit",
            "skos:notation": quantity.notation,
            "dcterms:conformsTo": { "@id": "https://www.ivoa.net/documents/VOUnits/" },
        },
    })
}

/// What a list of events costs, in the cap's unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub total: f64,
    pub cap: f64,
    pub notation: String,
    /// Uuids of the events whose attribute is missing or has no rate.
    pub unpriced: Vec<String>,
}

pub fn cost(events: &[EventRecord], budget: &Budget) -> Cost {
    let (priced, unpriced): (Vec<_>, Vec<_>) = events
        .iter()
        .map(|event| (event, rate_of(event, budget)))
        .partition(|(_, rate)| rate.is_some());
    let total = priced
        .iter()
        .filter_map(|(event, rate)| rate.map(|rate| price(event, rate)))
        .fold(0.0, |sum, bits| sum + bits)
        / budget.cap.unit.bits;
    Cost {
        total,
        cap: budget.cap.value,
        notation: budget.cap.notation.clone(),
        unpriced: unpriced
            .into_iter()
            .map(|(event, _)| event.uuid.clone())
            .collect(),
    }
}

fn rate_of<'a>(event: &EventRecord, budget: &'a Budget) -> Option<&'a Quantity> {
    let mode = event.attributes.get(&budget.attribute)?.as_str()?;
    budget
        .rates
        .iter()
        .find(|(value, _)| value == mode)
        .map(|(_, rate)| rate)
}

/// Bits one event costs under `rate`; a zero-length or reversed interval costs nothing
/// when the rate is per time.
fn price(event: &EventRecord, rate: &Quantity) -> f64 {
    let bits = rate.value * rate.unit.bits;
    match rate.unit.per_seconds {
        Some(per) => bits * duration_seconds(event) / per,
        None => bits,
    }
}

fn duration_seconds(event: &EventRecord) -> f64 {
    let micros = event.stop.unix_micros() - event.start.unix_micros();
    micros.max(0) as f64 / 1e6
}

impl Cost {
    pub fn text(&self) -> String {
        let used = format!(
            "{} of {} {} used",
            number(self.total),
            number(self.cap),
            self.notation
        );
        match self.unpriced.len() {
            0 => used,
            1 => format!("{used}, 1 interval without a rate"),
            n => format!("{used}, {n} intervals without a rate"),
        }
    }
}

fn number(value: f64) -> String {
    let fixed = format!("{value:.2}");
    fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn spec_budget() -> Value {
        let unit = |notation: &str| {
            json!({ "@type": "qudt:Unit", "skos:notation": notation,
                "dcterms:conformsTo": { "@id": "https://www.ivoa.net/documents/VOUnits/" } })
        };
        let quantity = |value: f64, notation: &str| {
            json!({ "@type": "qudt:QuantityValue", "qudt:numericValue": value,
                "qudt:hasUnit": unit(notation) })
        };
        json!({
            "@context": {
                "qudt": "https://qudt.org/schema/qudt/",
                "skos": "http://www.w3.org/2004/02/skos/core#",
                "dcterms": "http://purl.org/dc/terms/",
                "cocat": "https://cocat.sciqlop.fr/ns/round#",
            },
            "@type": "cocat:Budget",
            "cocat:attribute": "mode",
            "cocat:cap": quantity(500.0, "Mibyte"),
            "cocat:rates": [
                { "cocat:value": "burst", "cocat:rate": quantity(2.0, "Mibyte/min") },
                { "cocat:value": "normal", "cocat:rate": quantity(0.1, "Mibyte/min") },
            ],
        })
    }

    fn budget_with(rates: &[(&str, f64, &str)]) -> Budget {
        let mut json = spec_budget();
        json["cocat:rates"] = rates
            .iter()
            .map(|(value, number, notation)| {
                json!({ "cocat:value": value, "cocat:rate": {
                    "qudt:numericValue": number,
                    "qudt:hasUnit": { "skos:notation": notation } } })
            })
            .collect();
        parse_budget(&json).unwrap()
    }

    fn event(uuid: &str, seconds: i64, mode: Option<&str>) -> EventRecord {
        let at = |s| Timestamp::from_unix_nanos(s * 1_000_000_000).unwrap();
        let mut event = EventRecord::new(uuid, at(0), at(seconds));
        if let Some(mode) = mode {
            event.attributes.insert("mode".into(), json!(mode));
        }
        event
    }

    #[test]
    fn the_spec_budget_parses_and_round_trips() {
        let budget = parse_budget(&spec_budget()).unwrap();
        assert_eq!(budget.attribute, "mode");
        assert_eq!(budget.rates.len(), 2);
        assert_eq!(budget_json(&budget), spec_budget());
        assert_eq!(parse_budget(&budget_json(&budget)).unwrap(), budget);
    }

    #[test]
    fn a_cap_that_is_a_rate_is_refused() {
        let mut json = spec_budget();
        json["cocat:cap"]["qudt:hasUnit"]["skos:notation"] = json!("Mibyte/min");
        let error = parse_budget(&json).unwrap_err();
        assert!(error.to_string().contains("cocat:cap"), "{error}");
    }

    #[test]
    fn a_missing_field_is_named() {
        let mut json = spec_budget();
        json.as_object_mut().unwrap().remove("cocat:attribute");
        let error = parse_budget(&json).unwrap_err();
        assert!(error.to_string().contains("cocat:attribute"), "{error}");
    }

    #[test]
    fn a_bad_unit_is_named_with_its_field() {
        let mut json = spec_budget();
        json["cocat:rates"][0]["cocat:rate"]["qudt:hasUnit"]["skos:notation"] = json!("MiB/min");
        let error = parse_budget(&json).unwrap_err().to_string();
        assert!(error.contains("cocat:rates[].cocat:rate"), "{error}");
        assert!(error.contains("MiB/min"), "{error}");
    }

    #[test]
    fn cost_is_rate_times_duration_in_the_cap_unit() {
        let budget = parse_budget(&spec_budget()).unwrap();
        let events = [
            event("a", 600, Some("burst")),
            event("b", 3600, Some("normal")),
            event("c", 0, Some("burst")),
        ];
        let cost = cost(&events, &budget);
        assert!((cost.total - 26.0).abs() < 1e-9, "{}", cost.total);
        assert_eq!(cost.cap, 500.0);
        assert!(cost.unpriced.is_empty());
    }

    #[test]
    fn rates_in_other_units_convert() {
        let budget = budget_with(&[("burst", 1024.0, "kibyte/s")]);
        let cost = cost(&[event("a", 60, Some("burst"))], &budget);
        assert!((cost.total - 60.0).abs() < 1e-9, "{}", cost.total);
    }

    #[test]
    fn a_rate_without_time_is_a_fixed_cost_per_interval() {
        let budget = budget_with(&[("burst", 5.0, "Mibyte")]);
        let events = [event("a", 10, Some("burst")), event("b", 99, Some("burst"))];
        assert!((cost(&events, &budget).total - 10.0).abs() < 1e-9);
    }

    #[test]
    fn events_without_a_rate_are_listed_not_counted() {
        let budget = parse_budget(&spec_budget()).unwrap();
        let events = [event("a", 60, None), event("b", 60, Some("survey"))];
        let cost = cost(&events, &budget);
        assert_eq!(cost.unpriced, ["a", "b"]);
        assert_eq!(cost.total, 0.0);
        assert_eq!(
            cost.text(),
            "0 of 500 Mibyte used, 2 intervals without a rate"
        );
    }

    #[test]
    fn the_text_says_how_much_is_used() {
        let budget = budget_with(&[("burst", 320.0, "Mibyte")]);
        let priced = cost(&[event("a", 1, Some("burst"))], &budget);
        assert_eq!(priced.text(), "320 of 500 Mibyte used");
        let one = cost(&[event("a", 1, None)], &budget);
        assert!(one.text().ends_with(", 1 interval without a rate"));
    }
}
