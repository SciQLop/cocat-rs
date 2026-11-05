//! A catalogue as a VOTable, the shape cocat's own importer reads back.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::io;

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use serde_json::Value;

use crate::model::{CatalogueSummary, EventRecord};

#[derive(Debug, thiserror::Error)]
pub enum VotableError {
    #[error("Export VOTable: not all attributes are present in all events {0:?}")]
    MissingAttributes(Vec<String>),
    #[error("Export VOTable: not all value types are identical for all events for attribute {0}")]
    MixedTypes(String),
    #[error("Export VOTable: cannot write the document: {0}")]
    Encoding(#[from] io::Error),
}

const START_FIELD: [(&str, &str); 6] = [
    ("ID", "TimeIntervalStart"),
    ("ucd", "time.start"),
    ("datatype", "char"),
    ("arraysize", "*"),
    ("xtype", "dateTime"),
    ("utype", ""),
];
const STOP_FIELD: [(&str, &str); 6] = [
    ("ID", "TimeIntervalStop"),
    ("ucd", "time.end"),
    ("datatype", "char"),
    ("arraysize", "*"),
    ("xtype", "dateTime"),
    ("utype", ""),
];
const CHAR: [(&str, &str); 2] = [("datatype", "char"), ("arraysize", "*")];

pub fn export(
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
) -> Result<String, VotableError> {
    let attributes = attribute_kinds(events)?;
    let mut ordered: Vec<&EventRecord> = events.iter().collect();
    ordered.sort_by(|left, right| {
        (left.start, left.uuid.as_str()).cmp(&(right.start, right.uuid.as_str()))
    });

    let mut writer = Writer::new(Vec::with_capacity(2048));
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;
    open(
        &mut writer,
        "VOTABLE",
        &[
            ("version", "1.4"),
            ("xmlns", "http://www.ivoa.net/xml/VOTable/v1.3"),
        ],
    )?;
    text(
        &mut writer,
        "DESCRIPTION",
        &format!("Contact:{};Name:{}", catalogue.author, catalogue.name),
    )?;
    open(&mut writer, "RESOURCE", &[])?;
    open(
        &mut writer,
        "TABLE",
        &[("name", &catalogue.name.replace(' ', "_"))],
    )?;
    write_fields(&mut writer, &attributes)?;
    open(&mut writer, "DATA", &[])?;
    open(&mut writer, "TABLEDATA", &[])?;
    for event in &ordered {
        write_row(&mut writer, event, &attributes)?;
    }
    close(&mut writer, "TABLEDATA")?;
    close(&mut writer, "DATA")?;
    close(&mut writer, "TABLE")?;
    close(&mut writer, "RESOURCE")?;
    close(&mut writer, "VOTABLE")?;

    let bytes = writer.into_inner();
    String::from_utf8(bytes).map_err(|error| VotableError::Encoding(io::Error::other(error)))
}

fn open(writer: &mut Writer<Vec<u8>>, name: &str, attributes: &[(&str, &str)]) -> io::Result<()> {
    let mut element = BytesStart::new(name);
    for (key, value) in attributes {
        element.push_attribute((*key, sanitise(value).as_ref()));
    }
    writer.write_event(Event::Start(element))
}

fn close(writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
    writer.write_event(Event::End(BytesEnd::new(name)))
}

fn text(writer: &mut Writer<Vec<u8>>, name: &str, content: &str) -> io::Result<()> {
    open(writer, name, &[])?;
    writer.write_event(Event::Text(BytesText::new(&sanitise(content))))?;
    close(writer, name)
}

fn field(writer: &mut Writer<Vec<u8>>, name: &str, attributes: &[(&str, &str)]) -> io::Result<()> {
    let mut element = BytesStart::new("FIELD");
    element.push_attribute(("name", sanitise(name).as_ref()));
    for (key, value) in attributes {
        element.push_attribute((*key, *value));
    }
    writer.write_event(Event::Empty(element))
}

fn write_fields(writer: &mut Writer<Vec<u8>>, attributes: &[(String, Kind)]) -> io::Result<()> {
    field(writer, "Start Time", &START_FIELD)?;
    field(writer, "Stop Time", &STOP_FIELD)?;
    field(writer, "author", &CHAR)?;
    field(writer, "uuid", &CHAR)?;
    field(writer, "tags", &CHAR)?;
    field(writer, "products", &CHAR)?;
    field(writer, "rating", &[("datatype", "long")])?;
    for (name, kind) in attributes {
        kind.write_field(writer, name)?;
    }
    Ok(())
}

fn write_row(
    writer: &mut Writer<Vec<u8>>,
    event: &EventRecord,
    attributes: &[(String, Kind)],
) -> io::Result<()> {
    let fixed = [
        event.start.to_string(),
        event.stop.to_string(),
        event.author.clone(),
        event.uuid.clone(),
        json_list(&event.tags),
        json_list(&event.products),
        event.rating.unwrap_or_default().to_string(),
    ];
    let drawn: Vec<String> = attributes
        .iter()
        .map(|(name, kind)| kind.cell(&event.attributes[name]))
        .collect();
    open(writer, "TR", &[])?;
    for cell in fixed.iter().chain(&drawn) {
        text(writer, "TD", cell)?;
    }
    close(writer, "TR")
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
    /// Every JSON number, integer or not, is one `double` column: Python's own export
    /// contains `double` for an integer attribute (astropy widens the column), and its
    /// importer matches a `long` field against `rating` first, so a `long` attribute
    /// overwrites the rating on the way back in.
    Number,
    Boolean,
    List,
}

impl Kind {
    fn of(value: &Value) -> Option<Self> {
        match value {
            Value::String(_) => Some(Self::Text),
            Value::Bool(_) => Some(Self::Boolean),
            Value::Array(_) => Some(Self::List),
            Value::Number(_) => Some(Self::Number),
            Value::Null | Value::Object(_) => None,
        }
    }

    fn write_field(self, writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
        match self {
            Self::Text => field(writer, name, &CHAR),
            Self::List => field(
                writer,
                name,
                &[("datatype", "char"), ("arraysize", "*"), ("utype", "json")],
            ),
            Self::Number => field(writer, name, &[("datatype", "double")]),
            Self::Boolean => field(writer, name, &[("datatype", "boolean")]),
        }
    }

    fn cell(self, value: &Value) -> String {
        match (self, value) {
            (Self::Text, Value::String(text)) => text.clone(),
            _ => value.to_string(),
        }
    }
}

fn json_list(items: &[String]) -> String {
    let values: Vec<Value> = items
        .iter()
        .map(|item| Value::from(item.as_str()))
        .collect();
    Value::Array(values).to_string()
}

/// XML 1.0 has no way to represent a control character, not even as a character reference,
/// so the data is cleaned before the writer escapes it.
fn sanitise(text: &str) -> Cow<'_, str> {
    let unrepresentable =
        |character: char| character < ' ' && !matches!(character, '\t' | '\n' | '\r');
    if !text.chars().any(unrepresentable) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|character| {
                if unrepresentable(character) {
                    '\u{FFFD}'
                } else {
                    character
                }
            })
            .collect(),
    )
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
            xml.contains(r#"<FIELD name="i" datatype="double"/>"#),
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

    #[test]
    fn tag_with_markup_is_escaped() {
        let mut event = event("2025-01-31 10:00:00", None);
        event.tags = vec!["a<b".to_owned(), "x&y".to_owned()];
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"[&quot;a&lt;b&quot;,&quot;x&amp;y&quot;]"#),
            "{xml}"
        );
        assert!(!xml.contains("a<b"), "{xml}");
    }

    #[test]
    fn list_attribute_with_markup_is_escaped() {
        let event = with_attributes(json!({"l": ["a<b", "x&y"]}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"[&quot;a&lt;b&quot;,&quot;x&amp;y&quot;]"#),
            "{xml}"
        );
        assert!(!xml.contains("a<b"), "{xml}");
    }

    #[test]
    fn control_character_is_replaced() {
        let event = with_attributes(json!({"note": "a\u{1}b"}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(xml.contains("<TD>a\u{FFFD}b</TD>"), "{xml}");
        assert!(!xml.contains('\u{1}'), "{xml}");
    }

    #[test]
    fn control_character_in_a_name_is_replaced() {
        let event = with_attributes(json!({"n\u{1}te": "x"}));
        let xml = export(&catalogue("al\u{2}pha"), &[event]).expect("a votable");

        assert!(xml.contains("<TABLE name=\"al\u{FFFD}pha\">"), "{xml}");
        assert!(xml.contains("<FIELD name=\"n\u{FFFD}te\""), "{xml}");
        assert!(!xml.contains(['\u{1}', '\u{2}']), "{xml}");
    }

    #[test]
    fn carriage_return_is_written_as_a_reference() {
        let event = with_attributes(json!({"note": "a\rb"}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(xml.contains("<TD>a&#13;b</TD>"), "{xml}");
    }

    #[test]
    fn integer_and_float_attribute_is_one_double_field() {
        let mut whole = with_attributes(json!({"n": 2}));
        whole.uuid = "00000000-0000-4000-8000-000000000001".to_owned();
        let mut fractional = with_attributes(json!({"n": 2.5}));
        fractional.uuid = "00000000-0000-4000-8000-000000000002".to_owned();
        let xml = export(&catalogue("alpha"), &[whole, fractional]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="n" datatype="double"/>"#),
            "{xml}"
        );
        assert!(xml.contains("<TD>2</TD></TR>"), "{xml}");
        assert!(xml.contains("<TD>2.5</TD></TR>"), "{xml}");
    }
}
