//! A catalogue as a VOTable, the shape cocat's own importer reads back.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::Value;

use crate::model::{CatalogueSummary, EventRecord};

#[derive(Debug, thiserror::Error)]
pub enum VotableError {
    #[error("Export VOTable: not all attributes are present in all events {0:?}")]
    MissingAttributes(Vec<String>),
    #[error("Export VOTable: not all value types are identical for all events for attribute {0}")]
    MixedTypes(String),
}

const START_FIELD: &str = r#"<FIELD name="Start Time" ID="TimeIntervalStart" ucd="time.start" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#;
const STOP_FIELD: &str = r#"<FIELD name="Stop Time" ID="TimeIntervalStop" ucd="time.end" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#;

pub fn export(
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
) -> Result<String, VotableError> {
    let attributes = attribute_kinds(events)?;
    let mut ordered: Vec<&EventRecord> = events.iter().collect();
    ordered.sort_by(|left, right| {
        (left.start, left.uuid.as_str()).cmp(&(right.start, right.uuid.as_str()))
    });

    let mut xml = String::with_capacity(2048);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<VOTABLE version=\"1.4\" xmlns=\"http://www.ivoa.net/xml/VOTable/v1.3\">\n");
    let _ = writeln!(
        xml,
        "  <DESCRIPTION>Contact:{};Name:{}</DESCRIPTION>",
        escape(&catalogue.author),
        escape(&catalogue.name)
    );
    xml.push_str("  <RESOURCE>\n");
    let _ = writeln!(
        xml,
        "    <TABLE name=\"{}\">",
        escape(&catalogue.name.replace(' ', "_"))
    );

    let mut fields = vec![
        START_FIELD.to_owned(),
        STOP_FIELD.to_owned(),
        char_field("author"),
        char_field("uuid"),
        char_field("tags"),
        char_field("products"),
        simple_field("rating", "long"),
    ];
    fields.extend(attributes.iter().map(|(name, kind)| kind.field(name)));
    for field in &fields {
        let _ = writeln!(xml, "      {field}");
    }

    xml.push_str("      <DATA>\n        <TABLEDATA>\n");
    for event in ordered {
        let _ = writeln!(xml, "          {}", row(event, &attributes));
    }
    xml.push_str("        </TABLEDATA>\n      </DATA>\n    </TABLE>\n  </RESOURCE>\n</VOTABLE>\n");
    Ok(xml)
}

fn row(event: &EventRecord, attributes: &[(String, Kind)]) -> String {
    let mut cells = vec![
        event.start.to_string(),
        event.stop.to_string(),
        escape(&event.author),
        escape(&event.uuid),
        json_list(&event.tags),
        json_list(&event.products),
        event.rating.unwrap_or_default().to_string(),
    ];
    cells.extend(
        attributes
            .iter()
            .map(|(name, kind)| kind.cell(&event.attributes[name])),
    );
    format!("<TR><TD>{}</TD></TR>", cells.join("</TD><TD>"))
}

/// The attributes every event carries, by name, with the one kind they all share.
fn attribute_kinds(events: &[EventRecord]) -> Result<Vec<(String, Kind)>, VotableError> {
    let names = attribute_names(events);
    let mut common: Option<BTreeSet<&str>> = None;
    for event in events {
        let carried = keys(&event.attributes);
        common = Some(match common {
            None => carried,
            Some(seen) => seen.intersection(&carried).copied().collect(),
        });
    }
    let common = common.unwrap_or_default();

    let missing: Vec<String> = names
        .difference(&common)
        .map(|name| (*name).to_owned())
        .collect();
    if !missing.is_empty() {
        return Err(VotableError::MissingAttributes(missing));
    }

    let mut kinds = Vec::with_capacity(names.len());
    for name in &names {
        let mut kind: Option<Kind> = None;
        for event in events {
            let Some(value) = Kind::of(&event.attributes[*name]) else {
                return Err(VotableError::MixedTypes((*name).to_owned()));
            };
            match kind {
                None => kind = Some(value),
                Some(known) if known == value => {}
                Some(_) => return Err(VotableError::MixedTypes((*name).to_owned())),
            }
        }
        if let Some(kind) = kind {
            kinds.push(((*name).to_owned(), kind));
        }
    }
    Ok(kinds)
}

fn attribute_names(events: &[EventRecord]) -> BTreeSet<&str> {
    events
        .iter()
        .flat_map(|event| keys(&event.attributes))
        .collect()
}

fn keys(attributes: &serde_json::Map<String, Value>) -> BTreeSet<&str> {
    attributes.keys().map(String::as_str).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Integer,
    Float,
    Boolean,
    List,
}

impl Kind {
    fn of(value: &Value) -> Option<Self> {
        match value {
            Value::String(_) => Some(Self::Text),
            Value::Bool(_) => Some(Self::Boolean),
            Value::Array(_) => Some(Self::List),
            Value::Number(number) if number.is_i64() || number.is_u64() => Some(Self::Integer),
            Value::Number(_) => Some(Self::Float),
            Value::Null | Value::Object(_) => None,
        }
    }

    fn field(self, name: &str) -> String {
        match self {
            Self::Text => char_field(name),
            Self::List => json_field(name),
            Self::Integer => simple_field(name, "long"),
            Self::Float => simple_field(name, "double"),
            Self::Boolean => simple_field(name, "boolean"),
        }
    }

    fn cell(self, value: &Value) -> String {
        match (self, value) {
            (Self::Text, Value::String(text)) => escape(text),
            _ => value.to_string(),
        }
    }
}

fn char_field(name: &str) -> String {
    format!(
        r#"<FIELD name="{}" datatype="char" arraysize="*"/>"#,
        escape(name)
    )
}

fn json_field(name: &str) -> String {
    format!(
        r#"<FIELD name="{}" datatype="char" arraysize="*" utype="json"/>"#,
        escape(name)
    )
}

fn simple_field(name: &str, datatype: &str) -> String {
    format!(r#"<FIELD name="{}" datatype="{datatype}"/>"#, escape(name))
}

fn json_list(items: &[String]) -> String {
    let values: Vec<Value> = items
        .iter()
        .map(|item| Value::from(item.as_str()))
        .collect();
    Value::Array(values).to_string()
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::export;
    use crate::model::{CatalogueSummary, EventRecord};
    use crate::time::Timestamp;

    fn catalogue(name: &str) -> CatalogueSummary {
        CatalogueSummary {
            uuid: "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60".to_owned(),
            name: name.to_owned(),
            author: "a@x.org".to_owned(),
            tags: vec!["t1".to_owned()],
            attributes: Map::new(),
            event_count: 0,
        }
    }

    fn event(start: &str, rating: Option<i64>) -> EventRecord {
        EventRecord {
            uuid: "00000000-0000-4000-8000-000000000001".to_owned(),
            start: Timestamp::parse(start).expect("a start"),
            stop: Timestamp::parse(start).expect("a stop"),
            author: "a@x.org".to_owned(),
            tags: vec!["t1".to_owned()],
            products: vec!["p1".to_owned()],
            rating,
            attributes: Map::new(),
        }
    }

    fn with_attributes(values: Value) -> EventRecord {
        let mut event = event("2025-01-31 10:00:00", Some(3));
        event.attributes = values.as_object().cloned().expect("an object");
        event
    }

    fn position(xml: &str, needle: &str) -> usize {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} is missing from\n{xml}"))
    }

    #[test]
    fn fixed_fields_in_python_order() {
        let xml = export(
            &catalogue("my room x"),
            &[event("2025-01-31 10:00:00", Some(3))],
        )
        .expect("a votable");

        assert!(xml.starts_with(r#"<?xml version="1.0" encoding="utf-8"?>"#));
        assert!(
            xml.contains(r#"<VOTABLE version="1.4" xmlns="http://www.ivoa.net/xml/VOTable/v1.3">"#)
        );
        assert!(xml.contains("<DESCRIPTION>Contact:a@x.org;Name:my room x</DESCRIPTION>"));
        assert!(xml.contains(r#"<TABLE name="my_room_x">"#));

        let order = [
            r#"<FIELD name="Start Time" ID="TimeIntervalStart" ucd="time.start" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#,
            r#"<FIELD name="Stop Time" ID="TimeIntervalStop" ucd="time.end" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#,
            r#"<FIELD name="author" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="uuid" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="tags" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="products" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="rating" datatype="long"/>"#,
        ];
        let mut previous = 0;
        for field in order {
            let at = position(&xml, field);
            assert!(at > previous, "{field} is out of order in\n{xml}");
            previous = at;
        }
    }

    #[test]
    fn rating_null_is_zero() {
        let xml = export(
            &catalogue("alpha"),
            &[
                event("2025-01-31 10:00:00", None),
                event("2025-01-31 11:00:00", Some(3)),
            ],
        )
        .expect("a votable");

        assert!(
            xml.contains("<TR><TD>2025-01-31T10:00:00.000000Z</TD>"),
            "{xml}"
        );
        assert!(xml.contains("<TD>0</TD></TR>"), "{xml}");
        assert!(xml.contains("<TD>3</TD></TR>"), "{xml}");
    }

    #[test]
    fn rows_are_in_time_order() {
        let late = event("2025-01-31 12:00:00", None);
        let early = event("2025-01-31 10:00:00", None);
        let xml = export(&catalogue("alpha"), &[late, early]).expect("a votable");

        assert!(
            position(&xml, "2025-01-31T10:00:00") < position(&xml, "2025-01-31T12:00:00"),
            "{xml}"
        );
    }

    #[test]
    fn attribute_types() {
        let event = with_attributes(json!({"s": "x", "i": 1, "f": 1.5, "b": true, "l": [1]}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="b" datatype="boolean"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="f" datatype="double"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="i" datatype="long"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="l" datatype="char" arraysize="*" utype="json"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="s" datatype="char" arraysize="*"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains("<TD>true</TD><TD>1.5</TD><TD>1</TD><TD>[1]</TD><TD>x</TD>"),
            "{xml}"
        );
    }

    #[test]
    fn missing_attribute_is_an_error() {
        let mut without = event("2025-01-31 11:00:00", None);
        without.attributes = json!({"b": 2}).as_object().cloned().expect("an object");

        let error = export(
            &catalogue("alpha"),
            &[with_attributes(json!({"a": 1, "b": 2})), without],
        )
        .expect_err("a missing attribute");

        assert_eq!(
            error.to_string(),
            "Export VOTable: not all attributes are present in all events [\"a\"]"
        );
    }

    #[test]
    fn mixed_types_is_an_error() {
        let error = export(
            &catalogue("alpha"),
            &[
                with_attributes(json!({"a": 1})),
                with_attributes(json!({"a": "one"})),
            ],
        )
        .expect_err("a mixed type");

        assert_eq!(
            error.to_string(),
            "Export VOTable: not all value types are identical for all events for attribute a"
        );
    }

    #[test]
    fn escapes_markup() {
        let mut event = event("2025-01-31 10:00:00", None);
        event.attributes = json!({"note": "a<b & \"c\""})
            .as_object()
            .cloned()
            .expect("an object");
        let xml = export(&catalogue("a<b & \"c\""), &[event]).expect("a votable");

        assert!(
            xml.contains(
                "<DESCRIPTION>Contact:a@x.org;Name:a&lt;b &amp; &quot;c&quot;</DESCRIPTION>"
            ),
            "{xml}"
        );
        assert!(xml.contains("<TD>a&lt;b &amp; &quot;c&quot;</TD>"), "{xml}");
    }

    #[test]
    fn empty_catalogue_has_no_rows() {
        let xml = export(&catalogue("beta"), &[]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="rating" datatype="long"/>"#),
            "{xml}"
        );
        assert!(xml.contains("<TABLEDATA>"), "{xml}");
        assert!(!xml.contains("<TR>"), "{xml}");
    }
}
