//! The one rule every new password meets: at least 8 characters, and not guessable by
//! zxcvbn's measure (score 3 or more), with the account's address counted as known to an
//! attacker. No composition rules, as NIST 800-63B advises. Stored passwords are never
//! re-checked: the rule applies when a password is set.

use thiserror::Error;
use zxcvbn::{Score, zxcvbn};

pub const MIN_LENGTH: usize = 8;
const MIN_SCORE: Score = Score::Three;

/// Why a password is refused, in words a person can act on.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct WeakPassword(pub String);

/// Accept `password` for the account at `email`, or say why not.
pub fn check(password: &str, email: &str) -> Result<(), WeakPassword> {
    if password.chars().count() < MIN_LENGTH {
        return Err(WeakPassword(format!(
            "Use at least {MIN_LENGTH} characters."
        )));
    }
    let entropy = zxcvbn(password, &known_words(email));
    if entropy.score() >= MIN_SCORE {
        return Ok(());
    }
    let hint = entropy
        .feedback()
        .map(ToString::to_string)
        .unwrap_or_default();
    Err(WeakPassword(
        format!("This password is too easy to guess. {hint}")
            .trim_end()
            .to_owned(),
    ))
}

/// The address, its local part and that part's words: a name such as `marie.curie` is the first thing tried.
fn known_words(email: &str) -> Vec<&str> {
    let local_part = email.split('@').next().unwrap_or_default();
    [email, local_part]
        .into_iter()
        .chain(local_part.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMAIL: &str = "marie.curie@example.org";

    #[test]
    fn a_short_password_is_refused_for_its_length() {
        let refusal = check("Xk9#q", EMAIL).unwrap_err();
        assert_eq!(refusal.0, "Use at least 8 characters.");
    }

    #[test]
    fn a_common_password_is_refused_with_a_hint() {
        let refusal = check("password123", EMAIL).unwrap_err();
        assert!(refusal.0.starts_with("This password is too easy to guess."));
        assert!(
            refusal.0.len() > "This password is too easy to guess.".len(),
            "zxcvbn's suggestion follows: {refusal}"
        );
    }

    #[test]
    fn the_address_counts_as_known() {
        let password = "curiemarie7";
        assert!(check(password, "someone@example.org").is_ok());
        assert!(check(password, EMAIL).is_err());
    }

    #[test]
    fn the_local_part_counts_as_known() {
        assert!(check("marie.curie", "someone@example.org").is_ok());
        assert!(check("marie.curie", EMAIL).is_err());
    }

    #[test]
    fn a_few_unrelated_words_pass() {
        assert_eq!(check("correct horse battery staple", EMAIL), Ok(()));
    }
}
