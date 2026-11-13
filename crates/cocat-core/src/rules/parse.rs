//! Reading a rule's JSON into a `Set`, validating it on the way. Each node is told apart by
//! its key, so an error says where in the rule it is and what is wrong there.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::relative::{is_offset, is_relative, offset_of};
use super::{FIELDS, Op, Predicate, Problem, RuleError, Set, regex};

type Result<T> = std::result::Result<T, RuleError>;
type SetReader = fn(&Node) -> Result<Set>;
type PredicateReader = fn(&Node) -> Result<Predicate>;

/// The forms of a set, each with the options it takes besides its own key.
const SETS: [(&str, &[&str], SetReader); 12] = [
    ("all", &[], all),
    ("public", &[], public),
    ("catalogue", &[], catalogue),
    ("filter", &["where"], filter),
    ("merge", &["gap"], merge),
    ("intersect", &[], intersect),
    ("subtract", &[], subtract),
    ("union", &[], union),
    ("shift", &["by"], shift),
    ("tile", &["width"], tile),
    ("window", &["within"], window),
    ("closest", &[], closest),
];

const PREDICATES: [(&str, PredicateReader); 9] = [
    ("cmp", cmp),
    ("match", matches),
    ("has", has),
    ("contains", contains),
    ("not", not),
    ("and", and),
    ("or", or),
    ("in_catalogue", in_catalogue),
    ("text", text),
];

/// Reads and validates a rule.
pub fn parse(rule: &Value) -> Result<Set> {
    set(&Node {
        value: rule,
        at: "rule".to_owned(),
    })
}

/// A JSON value of a rule and its path from the rule's root.
struct Node<'v> {
    value: &'v Value,
    at: String,
}

impl<'v> Node<'v> {
    fn invalid(&self, problem: Problem) -> RuleError {
        RuleError::Invalid {
            at: self.at.clone(),
            problem,
        }
    }

    fn format(&self, message: String) -> RuleError {
        self.invalid(Problem::Format(message))
    }

    fn object(&self, what: &str) -> Result<&'v Map<String, Value>> {
        let not = || self.format(format!("{what} is a JSON object, not {}", self.value));
        self.value.as_object().ok_or_else(not)
    }

    fn key(&self, key: &str) -> Option<Node<'v>> {
        self.value.get(key).map(|value| Node {
            value,
            at: format!("{}.{key}", self.at),
        })
    }

    /// The value under `key` of the `form` this node is.
    fn needs(&self, form: &str, key: &str) -> Result<Node<'v>> {
        self.key(key)
            .ok_or_else(|| self.format(format!("'{form}' needs '{key}'")))
    }

    fn items(&self) -> Result<Vec<Node<'v>>> {
        let not = || self.format(format!("expected a list, not {}", self.value));
        let items = self.value.as_array().ok_or_else(not)?;
        Ok(items
            .iter()
            .enumerate()
            .map(|(index, value)| Node {
                value,
                at: format!("{}[{index}]", self.at),
            })
            .collect())
    }

    fn exactly<const N: usize>(&self) -> Result<[Node<'v>; N]> {
        <[Node<'v>; N]>::try_from(self.items()?)
            .map_err(|items| self.format(format!("expected {N} items, got {}", items.len())))
    }

    fn text(&self) -> Result<&'v str> {
        let not = || self.format(format!("expected text, not {}", self.value));
        self.value.as_str().ok_or_else(not)
    }

    /// A length in seconds, finite and `valid`.
    fn seconds(&self, valid: fn(f64) -> bool) -> Result<f64> {
        let not = || self.format(format!("expected seconds, not {}", self.value));
        let value = self.value.as_f64().ok_or_else(not)?;
        if value.is_finite() && valid(value) {
            Ok(value)
        } else {
            Err(self.invalid(Problem::Length(value)))
        }
    }
}

fn set(node: &Node) -> Result<Set> {
    let object = node.object("a set")?;
    let Some((form, options, read)) = SETS.iter().find(|(form, ..)| object.contains_key(*form))
    else {
        let forms = SETS.map(|(form, ..)| form).join(", ");
        return Err(node.format(format!("not a set: expected one of {forms}")));
    };
    let unknown = |key: &&String| key.as_str() != *form && !options.contains(&key.as_str());
    if let Some(key) = object.keys().find(unknown) {
        return Err(node.format(format!("unknown key '{key}' for '{form}'")));
    }
    read(node)
}

/// The set under the form's own key.
fn inner(node: &Node, form: &str) -> Result<Box<Set>> {
    Ok(Box::new(set(&node.needs(form, form)?)?))
}

fn pair(node: &Node, form: &str) -> Result<(Box<Set>, Box<Set>)> {
    let [a, b] = node.needs(form, form)?.exactly()?;
    Ok((Box::new(set(&a)?), Box::new(set(&b)?)))
}

fn all(node: &Node) -> Result<Set> {
    let all = node.needs("all", "all")?;
    match all.value {
        Value::Bool(true) => Ok(Set::All { all: true }),
        _ => Err(all.invalid(Problem::All)),
    }
}

fn public(node: &Node) -> Result<Set> {
    let public = node.needs("public", "public")?;
    match public.value {
        Value::Bool(true) => Ok(Set::Public { public: true }),
        _ => Err(public.invalid(Problem::Public)),
    }
}

fn catalogue(node: &Node) -> Result<Set> {
    let catalogue = node.needs("catalogue", "catalogue")?.text()?.to_owned();
    Ok(Set::Catalogue { catalogue })
}

fn filter(node: &Node) -> Result<Set> {
    Ok(Set::Filter {
        filter: inner(node, "filter")?,
        predicate: predicate(&node.needs("filter", "where")?)?,
    })
}

fn merge(node: &Node) -> Result<Set> {
    let gap = match node.key("gap") {
        Some(gap) => gap.seconds(|gap| gap >= 0.0)?,
        None => 0.0,
    };
    Ok(Set::Merge {
        merge: inner(node, "merge")?,
        gap,
    })
}

fn intersect(node: &Node) -> Result<Set> {
    Ok(Set::Intersect {
        intersect: pair(node, "intersect")?,
    })
}

fn subtract(node: &Node) -> Result<Set> {
    Ok(Set::Subtract {
        subtract: pair(node, "subtract")?,
    })
}

fn closest(node: &Node) -> Result<Set> {
    Ok(Set::Closest {
        closest: pair(node, "closest")?,
    })
}

fn union(node: &Node) -> Result<Set> {
    let sets = node.needs("union", "union")?.items()?;
    Ok(Set::Union {
        union: sets.iter().map(set).collect::<Result<_>>()?,
    })
}

fn shift(node: &Node) -> Result<Set> {
    Ok(Set::Shift {
        shift: inner(node, "shift")?,
        by: node.needs("shift", "by")?.seconds(|_| true)?,
    })
}

/// A tile of width zero never ends.
fn tile(node: &Node) -> Result<Set> {
    Ok(Set::Tile {
        tile: inner(node, "tile")?,
        width: node.needs("tile", "width")?.seconds(|width| width > 0.0)?,
    })
}

fn window(node: &Node) -> Result<Set> {
    Ok(Set::Window {
        window: pair(node, "window")?,
        within: node
            .needs("window", "within")?
            .seconds(|within| within >= 0.0)?,
    })
}

fn predicate(node: &Node) -> Result<Predicate> {
    let names = || PREDICATES.map(|(name, _)| name).join(", ");
    let object = node.object("a predicate")?;
    let mut keys = object.keys();
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return Err(node.format(format!("a predicate has one key, one of {}", names())));
    };
    let Some((_, read)) = PREDICATES.iter().find(|(name, _)| name == key) else {
        let message = format!("unknown predicate '{key}': expected one of {}", names());
        return Err(node.format(message));
    };
    read(&node.needs(key, key)?)
}

fn cmp(node: &Node) -> Result<Predicate> {
    let [name, op, value] = node.exactly()?;
    let op = Op::deserialize(op.value)
        .map_err(|_| node.format(format!("unknown operator '{}'", shown(op.value))))?;
    if is_relative(value.value) && !offset_of(value.value).is_some_and(is_offset) {
        let message = format!(
            "{} is not a time from now: write it like -7d (units s, m, h, d, M, y)",
            value.value.get("now").unwrap_or(value.value)
        );
        return Err(node.format(message));
    }
    Ok(Predicate::Cmp(field(node, &name)?, op, value.value.clone()))
}

fn matches(node: &Node) -> Result<Predicate> {
    let [name, pattern] = node.exactly()?;
    let pattern = pattern.text()?;
    regex(pattern).map_err(|problem| node.invalid(problem))?;
    Ok(Predicate::Match(field(node, &name)?, pattern.to_owned()))
}

fn has(node: &Node) -> Result<Predicate> {
    Ok(Predicate::Has(field(node, node)?))
}

fn contains(node: &Node) -> Result<Predicate> {
    let [name, value] = node.exactly()?;
    Ok(Predicate::Contains(
        field(node, &name)?,
        value.value.clone(),
    ))
}

fn not(node: &Node) -> Result<Predicate> {
    Ok(Predicate::Not(Box::new(predicate(node)?)))
}

fn and(node: &Node) -> Result<Predicate> {
    Ok(Predicate::And(predicates(node)?))
}

fn or(node: &Node) -> Result<Predicate> {
    Ok(Predicate::Or(predicates(node)?))
}

fn predicates(node: &Node) -> Result<Vec<Predicate>> {
    node.items()?.iter().map(predicate).collect()
}

fn in_catalogue(node: &Node) -> Result<Predicate> {
    match node.value {
        Value::Null => Ok(Predicate::InCatalogue(None)),
        Value::String(uuid) => Ok(Predicate::InCatalogue(Some(uuid.clone()))),
        other => Err(node.format(format!("expected a catalogue uuid or null, not {other}"))),
    }
}

fn text(node: &Node) -> Result<Predicate> {
    Ok(Predicate::Text(node.text()?.to_owned()))
}

/// A fixed event field or an attribute `#key`, reported at the predicate `at`.
fn field(at: &Node, name: &Node) -> Result<String> {
    let name = name.text()?;
    let attribute = name.strip_prefix('#').is_some_and(|key| !key.is_empty());
    if attribute || FIELDS.contains(&name) {
        Ok(name.to_owned())
    } else {
        Err(at.invalid(Problem::Field(name.to_owned())))
    }
}

fn shown(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}
