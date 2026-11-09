//! The colour a person is drawn in, the same in every view: their face in the header, the
//! rows they are editing, the history list and the Share dialog.

/// Each carries white text and shows on the light and dark pages; no two look alike (the
/// server's presence tests check both).
pub const COLORS: [&str; 8] = [
    "#2a5bd7", "#bf5300", "#1b7f72", "#8e3bd0", "#49700f", "#835a3c", "#b8246f", "#555d72",
];

/// A colour picked from the name (an email, or a group's name), stable across sessions.
pub fn color(name: &str) -> &'static str {
    let hash = xxhash_rust::xxh3::xxh3_64(name.as_bytes());
    COLORS[(hash % COLORS.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_its_colour() {
        assert_eq!(color("a@example.org"), color("a@example.org"));
        assert!(COLORS.contains(&color("b@example.org")));
    }
}
