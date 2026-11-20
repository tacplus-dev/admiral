// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! What the triage advisor may return: a typed issue proposal, never an issue. The core validates a proposal again
//! before anything is done with it, and only the filing core (#5) files, after its own allow-list, secret and
//! duplicate checks (tacplus-dev/roadmap decisions/skipper-and-admiral-2026-11-01.md, 2026-11-19: an LLM may
//! propose; it never holds a token or a write path). The advisor names this module, so the LLM-rule gate
//! (scripts/llm-rule-gate.sh) reads it too: it holds data and checks only, with no client, no token and no
//! re-export.

use serde::{Deserialize, Serialize};

/// The private triage project every report's full record goes to first, and the target of any cause outside
/// tacplus (the 2026-11-19 addenda: GitLab `seabeam/triage`).
pub const PRIVATE_TRIAGE: &str = "seabeam/triage";
/// The organization of the platform's own repositories, where a platform cause is filed in public.
pub const PLATFORM_ORG: &str = "tacplus-dev";
/// The generator's repository.
pub const GENERATOR_REPOSITORY: &str = "tacplus-dev/tacplus";

pub const MAX_TITLE_BYTES: usize = 256;
/// Well under a GitHub issue body's 65536 characters: the filing core (#5) adds its own lines.
pub const MAX_BODY_BYTES: usize = 16384;
pub const MAX_RATIONALE_BYTES: usize = 4096;
pub const MAX_LABELS: usize = 8;
const MAX_REPOSITORY_NAME_BYTES: usize = 100;

/// A typed issue, as the advisor's `propose_issue` tool takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueProposal {
    /// Where the cause lives.
    pub cause: Cause,
    /// 0 to 100.
    pub confidence_percent: u8,
    /// Why, in the terms of the report's evidence.
    pub rationale: String,
    /// `tacplus-dev/<repository>` for a platform cause, `seabeam/triage` otherwise.
    pub target_repository: String,
    /// One line.
    pub title: String,
    /// Sanitized: no secret, no tenant identifier, no user's identity.
    pub body: String,
    /// The advisor's labels; the core adds the one validation sets (`bug` or `not yet reproduced`).
    pub labels: Vec<Label>,
}

/// Where a report's cause lives (proto: admiral.Cause).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The service's own user code: the common case.
    TenantCode,
    /// A tacplus-dev platform service.
    PlatformService,
    /// tacplus's generated code.
    Generator,
    /// Not classified yet.
    Unknown,
}

impl Cause {
    /// Every cause, in the order the `propose_issue` schema lists them.
    pub const ALL: [Cause; 4] = [Cause::TenantCode, Cause::PlatformService, Cause::Generator, Cause::Unknown];

    /// Whether the cause is in tacplus, so the issue is the platform's, in public.
    pub fn is_platform(self) -> bool {
        matches!(self, Cause::PlatformService | Cause::Generator)
    }
}

/// An issue's label. The advisor may propose the first five; the core alone sets the last three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Label {
    Crash,
    WrongAnswer,
    Availability,
    Performance,
    Security,
    /// A reproduced report, or several unreproduced ones of one symptom.
    Bug,
    /// A report the Skipper could not reproduce yet: filed, and watched.
    #[serde(rename = "not yet reproduced")]
    NotYetReproduced,
    /// Several unreproduced reports of one symptom, across tenants.
    SymptomGroup,
}

impl Label {
    /// The labels the advisor may propose, in the order the `propose_issue` schema lists them.
    pub const PROPOSABLE: [Label; 5] =
        [Label::Crash, Label::WrongAnswer, Label::Availability, Label::Performance, Label::Security];

    pub fn is_proposable(self) -> bool {
        Label::PROPOSABLE.contains(&self)
    }

    /// The label as the issue carries it.
    pub fn name(self) -> &'static str {
        match self {
            Label::Crash => "crash",
            Label::WrongAnswer => "wrong-answer",
            Label::Availability => "availability",
            Label::Performance => "performance",
            Label::Security => "security",
            Label::Bug => "bug",
            Label::NotYetReproduced => "not yet reproduced",
            Label::SymptomGroup => "symptom-group",
        }
    }
}

/// What a proposal's text may not hold: the identifiers of the tenant the report came from (its Skipper's token's),
/// which the core passes in. Every other tenant's identifier, every user's identity and every secret shape is
/// refused without being named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Withheld {
    pub tenant_names: Vec<String>,
}

/// What a proposal's text leaked. It never carries the text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leak {
    Secret,
    TenantIdentifier,
    UserIdentity,
}

impl std::fmt::Display for Leak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Leak::Secret => "what looks like a secret",
            Leak::TenantIdentifier => "a tenant's identifier",
            Leak::UserIdentity => "a user's identity",
        })
    }
}

/// Why a proposal is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    ConfidenceOutOfRange(u8),
    Empty(&'static str),
    TooLong {
        field: &'static str,
        bytes: usize,
        max: usize,
    },
    TitleNotOneLine,
    /// The target is not where the cause's issues go.
    Target {
        cause: Cause,
        target: String,
    },
    TooManyLabels(usize),
    DuplicateLabel(Label),
    /// The core sets this label, never the advisor.
    CoreLabel(Label),
    Leaks {
        field: &'static str,
        leak: Leak,
    },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::ConfidenceOutOfRange(n) => write!(f, "confidence_percent is {n}, over 100"),
            Rejection::Empty(field) => write!(f, "{field} is empty"),
            Rejection::TooLong { field, bytes, max } => {
                write!(f, "{field} is {bytes} bytes, over the {max}-byte limit")
            }
            Rejection::TitleNotOneLine => write!(f, "title is more than one line"),
            Rejection::Target { cause, target } => write!(
                f,
                "target_repository {target:?} is not where a {cause:?} cause is filed: a platform service's is \
                 {PLATFORM_ORG}/<repository>, the generator's {GENERATOR_REPOSITORY}, any other {PRIVATE_TRIAGE}"
            ),
            Rejection::TooManyLabels(n) => write!(f, "{n} labels, over the limit of {MAX_LABELS}"),
            Rejection::DuplicateLabel(l) => write!(f, "label {:?} is given twice", l.name()),
            Rejection::CoreLabel(l) => write!(f, "label {:?} is set by the Admiral, not proposed", l.name()),
            Rejection::Leaks { field, leak } => write!(f, "{field} holds {leak}: remove it"),
        }
    }
}

impl std::error::Error for Rejection {}

impl IssueProposal {
    /// Checks the proposal on its own: its bounds, that its target is where its cause is filed, its labels, and
    /// that no text of it leaks a secret, a tenant identifier or a user's identity. What needs more (the target on
    /// the filing core's allow-list, a duplicate) the filing core checks (#5).
    pub fn validate(&self, withheld: &Withheld) -> Result<(), Rejection> {
        if self.confidence_percent > 100 {
            return Err(Rejection::ConfidenceOutOfRange(self.confidence_percent));
        }
        bounded("rationale", &self.rationale, MAX_RATIONALE_BYTES)?;
        bounded("title", &self.title, MAX_TITLE_BYTES)?;
        if self.title.contains(['\n', '\r']) {
            return Err(Rejection::TitleNotOneLine);
        }
        bounded("body", &self.body, MAX_BODY_BYTES)?;
        if !target_fits(self.cause, &self.target_repository) {
            return Err(Rejection::Target { cause: self.cause, target: self.target_repository.clone() });
        }
        if self.labels.len() > MAX_LABELS {
            return Err(Rejection::TooManyLabels(self.labels.len()));
        }
        for (i, label) in self.labels.iter().enumerate() {
            if !label.is_proposable() {
                return Err(Rejection::CoreLabel(*label));
            }
            if self.labels[..i].contains(label) {
                return Err(Rejection::DuplicateLabel(*label));
            }
        }
        for (field, text) in [("title", &self.title), ("body", &self.body), ("rationale", &self.rationale)] {
            if let Some(leak) = leak(text, withheld) {
                return Err(Rejection::Leaks { field, leak });
            }
        }
        Ok(())
    }

    /// The JSON schema of a proposal, the `propose_issue` tool's input.
    pub fn input_schema() -> serde_json::Value {
        let causes: Vec<serde_json::Value> =
            Cause::ALL.iter().map(|c| serde_json::to_value(c).expect("a cause serializes")).collect();
        let labels: Vec<serde_json::Value> =
            Label::PROPOSABLE.iter().map(|l| serde_json::to_value(l).expect("a label serializes")).collect();
        serde_json::json!({
            "type": "object",
            "properties": {
                "cause": {"type": "string", "enum": causes},
                "confidence_percent": {"type": "integer", "minimum": 0, "maximum": 100},
                "rationale": {"type": "string", "minLength": 1, "maxLength": MAX_RATIONALE_BYTES},
                "target_repository": {"type": "string", "minLength": 1},
                "title": {"type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES},
                "body": {"type": "string", "minLength": 1, "maxLength": MAX_BODY_BYTES},
                "labels": {"type": "array", "items": {"type": "string", "enum": labels}, "maxItems": MAX_LABELS,
                           "uniqueItems": true}
            },
            "required": ["cause", "confidence_percent", "rationale", "target_repository", "title", "body", "labels"],
            "additionalProperties": false
        })
    }
}

fn bounded(field: &'static str, text: &str, max: usize) -> Result<(), Rejection> {
    if text.trim().is_empty() {
        return Err(Rejection::Empty(field));
    }
    if text.len() > max {
        return Err(Rejection::TooLong { field, bytes: text.len(), max });
    }
    Ok(())
}

/// Whether `target` is where a `cause`'s issue goes: a platform service's in its tacplus-dev repository, the
/// generator's in tacplus, anything else (the tenant's code, or not known yet) in the private triage project.
pub fn target_fits(cause: Cause, target: &str) -> bool {
    match cause {
        Cause::TenantCode | Cause::Unknown => target == PRIVATE_TRIAGE,
        Cause::Generator => target == GENERATOR_REPOSITORY,
        Cause::PlatformService => {
            target.strip_prefix(PLATFORM_ORG).and_then(|rest| rest.strip_prefix('/')).is_some_and(|name| {
                !name.is_empty()
                    && name.len() <= MAX_REPOSITORY_NAME_BYTES
                    && !name.starts_with('.')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
        }
    }
}

/// The first leak in `text`, if any: a secret's shape, a tenant identifier (any `tenants/<name>` that is not
/// redacted, or a name of the reporting tenant's) or a user's identity (`/users/<name>`). The filing core checks
/// again with its own screening (#5); this is the advisor's side of it, so the model can correct its proposal.
pub fn leak(text: &str, withheld: &Withheld) -> Option<Leak> {
    let lower = text.to_ascii_lowercase();
    if has_secret(text, &lower) {
        return Some(Leak::Secret);
    }
    if named_after(&lower, "tenants/") {
        return Some(Leak::TenantIdentifier);
    }
    if withheld.tenant_names.iter().any(|name| has_word(&lower, &name.to_ascii_lowercase())) {
        return Some(Leak::TenantIdentifier);
    }
    if named_after(&lower, "/users/") {
        return Some(Leak::UserIdentity);
    }
    None
}

const REDACTED: &str = "[redacted]";

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Whether `marker` is followed somewhere by a name that is not `[REDACTED]`.
fn named_after(lower: &str, marker: &str) -> bool {
    lower.match_indices(marker).any(|(i, _)| {
        let rest = &lower[i + marker.len()..];
        rest.chars().next().is_some_and(is_word_char) && !rest.starts_with(REDACTED)
    })
}

/// Whether `word` stands in `lower` as a word of its own.
fn has_word(lower: &str, word: &str) -> bool {
    !word.is_empty()
        && lower.match_indices(word).any(|(i, _)| {
            let before = lower[..i].chars().next_back();
            let after = lower[i + word.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
}

/// Token prefixes of well-known credentials.
const TOKEN_PREFIXES: [&str; 8] = ["ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "glpat-", "sk-ant-", "xoxb-"];

/// Keys whose value is a secret, as `key: value`, `key=value` or `"key": "value"`.
const SECRET_KEYS: [&str; 9] =
    ["password", "passwd", "api_key", "api-key", "apikey", "secret", "private_key", "client_secret", "authorization"];

fn has_secret(text: &str, lower: &str) -> bool {
    if lower.contains("-----begin") && lower.contains("private key") {
        return true;
    }
    let words = || text.split(|c: char| !is_word_char(c)).filter(|w| !w.is_empty());
    // A JWT: three base64url segments, the first a JSON header.
    if words().any(|w| w.starts_with("eyJ") && w.matches('.').count() >= 2 && w.len() >= 32) {
        return true;
    }
    if words().any(|w| TOKEN_PREFIXES.iter().any(|p| w.starts_with(p) && w.len() >= p.len() + 16)) {
        return true;
    }
    // An AWS access key id.
    if words().any(|w| {
        w.len() == 20 && w.starts_with("AKIA") && w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    }) {
        return true;
    }
    if lower.match_indices("bearer ").any(|(i, m)| {
        let token: String = lower[i + m.len()..].chars().take_while(|c| is_word_char(*c)).collect();
        token.len() >= 16
    }) {
        return true;
    }
    SECRET_KEYS.iter().any(|key| {
        lower.match_indices(key).any(|(i, m)| {
            let rest = lower[i + m.len()..].trim_start_matches('"').trim_start();
            let Some(value) = rest.strip_prefix([':', '=']) else { return false };
            let value = value.trim_start().trim_start_matches('"');
            !value.is_empty() && !value.starts_with(REDACTED) && !value.starts_with(['"', ',', '}', '\n'])
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::core::proposal::{
        leak, target_fits, Cause, IssueProposal, Label, Leak, Rejection, Withheld, GENERATOR_REPOSITORY,
        MAX_BODY_BYTES, MAX_LABELS, PRIVATE_TRIAGE,
    };

    fn proposal() -> IssueProposal {
        IssueProposal {
            cause: Cause::PlatformService,
            confidence_percent: 80,
            rationale: "the trace fails in the gateway's auth step".to_string(),
            target_repository: "tacplus-dev/gateway".to_string(),
            title: "gateway: UNAUTHENTICATED on a fresh token after rotation".to_string(),
            body: "A call through the gateway answers UNAUTHENTICATED for a token minted after rotation.".to_string(),
            labels: vec![Label::WrongAnswer],
        }
    }

    fn withheld() -> Withheld {
        Withheld { tenant_names: vec!["acme".to_string()] }
    }

    #[test]
    fn a_proposal_reads_from_its_json() {
        let json = r#"{"cause":"platform_service","confidence_percent":80,
            "rationale":"the trace fails in the gateway's auth step","target_repository":"tacplus-dev/gateway",
            "title":"gateway: UNAUTHENTICATED on a fresh token after rotation",
            "body":"A call through the gateway answers UNAUTHENTICATED for a token minted after rotation.",
            "labels":["wrong-answer"]}"#;
        let parsed: IssueProposal = serde_json::from_str(json).expect("parses");
        assert_eq!(parsed, proposal());
        assert_eq!(parsed.validate(&withheld()), Ok(()));
    }

    #[test]
    fn unknown_fields_causes_and_labels_do_not_parse() {
        for json in [
            r#"{"cause":"kernel","confidence_percent":1,"rationale":"r","target_repository":"t","title":"t","body":"b","labels":[]}"#,
            r#"{"cause":"unknown","confidence_percent":1,"rationale":"r","target_repository":"t","title":"t","body":"b","labels":["p0"]}"#,
            r#"{"cause":"unknown","confidence_percent":1,"rationale":"r","target_repository":"t","title":"t","body":"b","labels":[],"token":"x"}"#,
            r#"{"cause":"unknown","confidence_percent":300,"rationale":"r","target_repository":"t","title":"t","body":"b","labels":[]}"#,
        ] {
            assert!(serde_json::from_str::<IssueProposal>(json).is_err(), "{json} must not parse");
        }
    }

    #[test]
    fn the_target_follows_the_cause() {
        assert!(target_fits(Cause::PlatformService, "tacplus-dev/kernel"));
        assert!(target_fits(Cause::Generator, GENERATOR_REPOSITORY));
        assert!(target_fits(Cause::TenantCode, PRIVATE_TRIAGE));
        assert!(target_fits(Cause::Unknown, PRIVATE_TRIAGE));
        for (cause, target) in [
            (Cause::TenantCode, "tacplus-dev/gateway"),
            (Cause::PlatformService, PRIVATE_TRIAGE),
            (Cause::PlatformService, "tacplus-dev/"),
            (Cause::PlatformService, "tacplus-dev/../x"),
            (Cause::PlatformService, "tacplus-dev/a/b"),
            (Cause::PlatformService, "tacplus-devx/kernel"),
            (Cause::PlatformService, "someone/kernel"),
            (Cause::Generator, "tacplus-dev/gateway"),
            (Cause::Unknown, "seabeam/other"),
        ] {
            assert!(!target_fits(cause, target), "{cause:?} must not go to {target}");
        }
        let mut p = proposal();
        p.cause = Cause::TenantCode;
        assert!(matches!(p.validate(&withheld()), Err(Rejection::Target { .. })));
    }

    #[test]
    fn validate_refuses_what_the_schema_bounds() {
        let with = |f: &dyn Fn(&mut IssueProposal)| {
            let mut p = proposal();
            f(&mut p);
            p.validate(&withheld())
        };
        assert_eq!(with(&|p| p.confidence_percent = 101), Err(Rejection::ConfidenceOutOfRange(101)));
        assert_eq!(with(&|p| p.title = " ".into()), Err(Rejection::Empty("title")));
        assert_eq!(with(&|p| p.title = "one\ntwo".into()), Err(Rejection::TitleNotOneLine));
        assert_eq!(with(&|p| p.rationale = String::new()), Err(Rejection::Empty("rationale")));
        assert_eq!(
            with(&|p| p.body = "x".repeat(MAX_BODY_BYTES + 1)),
            Err(Rejection::TooLong { field: "body", bytes: MAX_BODY_BYTES + 1, max: MAX_BODY_BYTES })
        );
        assert_eq!(
            with(&|p| p.labels = vec![Label::Crash; MAX_LABELS + 1]),
            Err(Rejection::TooManyLabels(MAX_LABELS + 1))
        );
        assert_eq!(
            with(&|p| p.labels = vec![Label::Crash, Label::Crash]),
            Err(Rejection::DuplicateLabel(Label::Crash))
        );
        for core in [Label::Bug, Label::NotYetReproduced, Label::SymptomGroup] {
            assert_eq!(with(&|p| p.labels = vec![core]), Err(Rejection::CoreLabel(core)));
        }
        assert_eq!(with(&|p| p.confidence_percent = 100), Ok(()));
    }

    #[test]
    fn a_leak_in_any_text_is_refused() {
        let leaks = |text: &str| leak(text, &withheld());
        // Each fixture is put together at run time, so the source holds no secret's shape for a scanner to find.
        let jwt = ["eyJhbGciOiJSUzI1NiJ9", "eyJzdWIiOiJ1c2VyIn0", "c2lnbmF0dXJl"].join(".");
        for secret in [
            format!("-----BEGIN RSA {} KEY-----\nMIIE...", "PRIVATE"),
            format!("token {jwt}"),
            format!("Authorization: Bearer {}", "abcdefghijklmnopqrstuvwxyz"),
            format!(r#"{{"{}": "hunter2"}}"#, "password"),
            format!("{}=12345", "api_key"),
            format!("{}_{}", "ghp", "0123456789abcdefghij"),
            format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP"),
        ] {
            let secret = secret.as_str();
            assert_eq!(leaks(secret), Some(Leak::Secret), "{secret}");
        }
        assert_eq!(leaks("the caller /tenants/globex/services/web"), Some(Leak::TenantIdentifier));
        assert_eq!(leaks("ACME's service fails"), Some(Leak::TenantIdentifier));
        assert_eq!(leaks("the user /users/alice@example.com"), Some(Leak::UserIdentity));
        for clean in [
            "the caller /tenants/[REDACTED]/services/web",
            "acmesoft is not the tenant's name",
            "password: [REDACTED]",
            "the secret's version reference stays visible",
            "a gateway call answers UNAUTHENTICATED (16)",
        ] {
            assert_eq!(leaks(clean), None, "{clean}");
        }
        let mut p = proposal();
        p.body = "the acme tenant's call fails".into();
        assert_eq!(p.validate(&withheld()), Err(Rejection::Leaks { field: "body", leak: Leak::TenantIdentifier }));
    }

    #[test]
    fn the_schema_lists_what_serde_takes() {
        let schema = IssueProposal::input_schema();
        let causes = schema["properties"]["cause"]["enum"].as_array().expect("an enum");
        assert_eq!(causes.len(), Cause::ALL.len());
        let labels = schema["properties"]["labels"]["items"]["enum"].as_array().expect("an enum");
        for label in Label::PROPOSABLE {
            assert!(labels.contains(&serde_json::to_value(label).expect("serializes")));
            assert_eq!(serde_json::to_value(label).expect("serializes"), label.name());
        }
        assert!(!labels.contains(&serde_json::to_value(Label::Bug).expect("serializes")));
        let mut required: Vec<&str> =
            schema["required"].as_array().expect("required").iter().map(|v| v.as_str().expect("a name")).collect();
        let serialized = serde_json::to_value(proposal()).expect("serializes");
        let mut fields: Vec<&str> = serialized.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        required.sort_unstable();
        fields.sort_unstable();
        assert_eq!(required, fields);
    }
}
