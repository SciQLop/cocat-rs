use std::collections::HashMap;
use std::path::Path;

use cocat_server::links::Links;
use cocat_server::mail::{
    MailError, Mailer, invitation_mail, is_placeholder, reset_mail, verification_mail,
};

const PLACEHOLDER: &str = "placeholder.cocat.org";

struct Env(HashMap<&'static str, String>);

impl Env {
    fn new(pairs: &[(&'static str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| (*name, (*value).to_string()))
                .collect(),
        )
    }

    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    fn mailer(&self) -> Result<Option<Mailer>, MailError> {
        Mailer::from_env(&|name| self.get(name))
    }

    fn links(&self) -> Result<Links, cocat_server::links::LinksError> {
        Links::from_env(&|name| self.get(name))
    }
}

fn configured() -> Env {
    Env::new(&[
        ("COCAT_SMTP_HOST", "smtp.example.org"),
        ("COCAT_MAIL_FROM", "cocat@example.org"),
        ("COCAT_PUBLIC_URL", "https://cocat.example.org"),
    ])
}

fn directory(path: &Path) -> Env {
    Env::new(&[
        ("COCAT_MAIL_DIR", &path.to_string_lossy()),
        ("COCAT_MAIL_FROM", "cocat@example.org"),
        ("COCAT_PUBLIC_URL", "https://cocat.example.org"),
    ])
}

fn eml_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("the mail directory exists")
        .map(|entry| entry.expect("a readable entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "eml"))
        .collect();
    files.sort();
    files
}

#[test]
fn no_host_means_no_mailer() {
    assert!(Env::new(&[]).mailer().expect("an empty env").is_none());
    assert!(
        Env::new(&[("COCAT_SMTP_HOST", "   ")])
            .mailer()
            .expect("a blank host")
            .is_none()
    );
}

#[test]
fn a_host_needs_a_sender() {
    let error = Env::new(&[("COCAT_SMTP_HOST", "smtp.example.org")])
        .mailer()
        .expect_err("a host without a sender");

    assert!(error.to_string().contains("COCAT_MAIL_FROM"), "{error}");
}

#[test]
fn username_needs_password() {
    let mut without_password = configured();
    without_password
        .0
        .insert("COCAT_SMTP_USERNAME", "cocat".into());
    let error = without_password
        .mailer()
        .expect_err("a username without a password");
    assert!(error.to_string().contains("COCAT_SMTP_PASSWORD"), "{error}");

    let mut without_username = configured();
    without_username
        .0
        .insert("COCAT_SMTP_PASSWORD", "s3cret".into());
    let error = without_username
        .mailer()
        .expect_err("a password without a username");
    assert!(error.to_string().contains("COCAT_SMTP_USERNAME"), "{error}");
}

#[test]
fn the_port_must_be_a_number() {
    let mut env = configured();
    env.0.insert("COCAT_SMTP_PORT", "smtp".into());

    let error = env.mailer().expect_err("a port that is not a number");

    assert!(error.to_string().contains("COCAT_SMTP_PORT"), "{error}");
}

#[test]
fn starttls_words() {
    for (word, expected) in [
        ("1", true),
        ("true", true),
        ("YES", true),
        ("on", true),
        (" on ", true),
        ("0", false),
        ("false", false),
        ("No", false),
        ("off", false),
    ] {
        let mut env = configured();
        env.0.insert("COCAT_SMTP_STARTTLS", word.into());

        let mailer = env.mailer().expect("a starttls word").expect("a mailer");
        let Mailer::Smtp(smtp) = mailer else {
            panic!("{word} configured a directory mailer")
        };
        assert_eq!(smtp.starttls(), expected, "{word}");
    }

    let mut env = configured();
    env.0.insert("COCAT_SMTP_STARTTLS", "maybe".into());
    let error = env.mailer().expect_err("a word that is neither");
    assert!(error.to_string().contains("COCAT_SMTP_STARTTLS"), "{error}");
}

#[test]
fn a_mailer_needs_a_public_url() {
    let mut smtp = configured();
    smtp.0.remove("COCAT_PUBLIC_URL");
    let error = smtp.mailer().expect_err("a mailer without a public URL");
    assert!(error.to_string().contains("COCAT_PUBLIC_URL"), "{error}");

    let dir = tempfile::tempdir().expect("a temp dir");
    let mut files = directory(dir.path());
    files.0.remove("COCAT_PUBLIC_URL");
    let error = files.mailer().expect_err("a sink without a public URL");
    assert!(error.to_string().contains("COCAT_PUBLIC_URL"), "{error}");
}

#[test]
fn a_host_and_a_mail_dir_are_exclusive() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut env = configured();
    env.0.insert(
        "COCAT_MAIL_DIR",
        dir.path().to_string_lossy().into_owned(),
    );

    let error = env.mailer().expect_err("two sinks");

    assert!(error.to_string().contains("COCAT_MAIL_DIR"), "{error}");
    assert!(error.to_string().contains("COCAT_SMTP_HOST"), "{error}");
}

#[test]
fn a_placeholder_receives_no_mail() {
    assert!(is_placeholder(&format!("someone@{PLACEHOLDER}")));
    assert!(is_placeholder(&format!(
        "0000-0002-1825-0097@orcid@{}",
        PLACEHOLDER.to_uppercase()
    )));
    assert!(!is_placeholder("someone@example.org"));
}

#[tokio::test]
async fn a_placeholder_address_is_refused_by_the_sink() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let env = directory(dir.path());
    let mailer = env.mailer().expect("a sink").expect("a mailer");

    let error = mailer
        .send(&format!("someone@{PLACEHOLDER}"), "Hello", "Body")
        .await
        .expect_err("a placeholder address");

    assert!(error.to_string().contains("placeholder"), "{error}");
    assert!(eml_files(dir.path()).is_empty());
}

#[tokio::test]
async fn a_newline_in_a_header_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let env = directory(dir.path());
    let mailer = env.mailer().expect("a sink").expect("a mailer");

    let subject = mailer
        .send("guest@example.org", "hi\r\nBcc: spy@example.org", "Body")
        .await
        .expect_err("a newline in the subject");
    assert!(subject.to_string().contains("line break"), "{subject}");

    let address = mailer
        .send("guest@example.org\r\nBcc: spy@example.org", "Hello", "Body")
        .await
        .expect_err("a newline in the address");
    assert!(address.to_string().contains("line break"), "{address}");

    assert!(eml_files(dir.path()).is_empty());
}

#[tokio::test]
async fn the_directory_mailer_writes_one_file_per_message() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let env = directory(dir.path());
    let mailer = env.mailer().expect("a sink").expect("a mailer");

    mailer
        .send("guest@example.org", "Hello", "Body")
        .await
        .expect("the first message");
    mailer
        .send("other@example.org", "Second", "Another body")
        .await
        .expect("the second message");

    let files = eml_files(dir.path());
    assert_eq!(files.len(), 2, "{files:?}");
    let written: Vec<String> = files
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("a readable message"))
        .collect();
    let first = written
        .iter()
        .find(|message| message.contains("To: guest@example.org"))
        .unwrap_or_else(|| panic!("no message for the guest: {written:?}"));
    assert!(first.contains("Subject: Hello"), "{first}");
    assert!(first.contains("Body"), "{first}");
    assert!(
        written
            .iter()
            .any(|message| message.contains("To: other@example.org")
                && message.contains("Subject: Second")),
        "{written:?}"
    );
}

#[test]
fn links_ignore_the_host_header() {
    let mut env = Env::new(&[("COCAT_PUBLIC_URL", "https://cocat.example.org")]);
    let without_host = env.links().expect("links").ui("/verify#tok");
    env.0.insert("Host", "evil.example.org".into());
    let with_host = env.links().expect("links").ui("/verify#tok");

    assert_eq!(without_host, "https://cocat.example.org/ui/verify#tok");
    assert_eq!(with_host, without_host);
}

#[test]
fn links_carry_the_proxy_prefix() {
    let mut env = Env::new(&[("COCAT_PUBLIC_URL", "https://cocat.example.org")]);
    assert_eq!(
        env.links().expect("links").ui("/invitation#tok"),
        "https://cocat.example.org/ui/invitation#tok"
    );

    env.0.insert("COCAT_PROXY_PREFIX", "/cocat/".into());
    assert_eq!(
        env.links().expect("links").ui("/invitation#tok"),
        "https://cocat.example.org/cocat/ui/invitation#tok"
    );

    env.0.insert("COCAT_PROXY_PREFIX", "/".into());
    assert_eq!(
        env.links().expect("links").ui("/invitation#tok"),
        "https://cocat.example.org/ui/invitation#tok"
    );
}

#[test]
fn the_public_url_keeps_only_its_origin() {
    let env = Env::new(&[(
        "COCAT_PUBLIC_URL",
        "https://cocat.example.org:8443/cocat/?next=1#top",
    )]);

    assert_eq!(
        env.links().expect("links").ui("/verify#tok"),
        "https://cocat.example.org:8443/ui/verify#tok"
    );
}

#[test]
fn a_public_url_without_a_scheme_is_refused() {
    let env = Env::new(&[("COCAT_PUBLIC_URL", "cocat.example.org")]);

    let error = env.links().expect_err("a relative public URL");

    assert!(error.to_string().contains("COCAT_PUBLIC_URL"), "{error}");
}

#[test]
fn the_mail_texts_name_the_link_and_the_expiry() {
    let link = "https://cocat.example.org/ui/verify#tok";
    let (subject, body) = verification_mail(link);
    assert!(subject.to_lowercase().contains("confirm"), "{subject}");
    assert!(body.contains(link), "{body}");
    assert!(body.contains("hour"), "{body}");

    let link = "https://cocat.example.org/ui/reset-password#tok";
    let (subject, body) = reset_mail(link);
    assert!(subject.to_lowercase().contains("password"), "{subject}");
    assert!(body.contains(link), "{body}");
    assert!(body.contains("hour"), "{body}");

    let link = "https://cocat.example.org/ui/invitation#tok";
    let (subject, body) = invitation_mail("Lab", "owner@example.org", link);
    assert!(subject.contains("Lab"), "{subject}");
    assert!(body.contains("owner@example.org"), "{body}");
    assert!(body.contains(link), "{body}");
    assert!(body.contains("14 days"), "{body}");
}
