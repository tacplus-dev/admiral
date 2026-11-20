// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Admiral's own screening of a report before it is held: the Skipper redacted it (secrets, other tenants'
//! data, the reporter's identity), and the Admiral checks that again, since every message from a Skipper is
//! untrusted (tacplus-dev/looper#40). What it finds is redacted in place, never refused: every report surfaces
//! (tacplus-dev/roadmap decisions/skipper-and-admiral-2026-11-01.md), and a refused one would be sent again
//! forever. What was redacted is counted for the audit; what it was is never kept or logged.

use std::sync::LazyLock;

use admiral_model::types::services::admiral::Report;
use regex::{Captures, Regex};

const REDACTED: &str = "[REDACTED]";

/// Secrets the chassis logger's redaction (header and `*_token`/`*_secret` shapes) does not cover.
static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // A PEM private key, to its end line or the end of the text.
        Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)").unwrap(),
        // A JWT (a bearer token without its `Bearer` prefix).
        Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*").unwrap(),
        // `"password": "..."`, as JSON.
        Regex::new(r#"(?i)("(?:password|passwd|api[_-]?key|private[_-]?key|credentials?)"\s*:\s*")[^"]*"#).unwrap(),
        // `password=...`, as text.
        Regex::new(r#"(?i)\b((?:password|passwd|api[_-]?key)\s*[:=]\s*)[^"\s,}]+"#).unwrap(),
    ]
});

/// A tenant's place in an identity: `/tenants/<tenant>` in a namespace or a SPIFFE id.
static TENANT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(\btenants/)([A-Za-z0-9._-]+)").unwrap());

/// A user's identity: `/users/<user>` in a namespace or a SPIFFE id. A report carries no reporter identity.
static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(/users/)([A-Za-z0-9._@+-]+)").unwrap());

/// The tenant a report comes from: the identifiers its Skipper's token names it by (its namespace's, and its
/// SPIFFE id's when that names it otherwise). Any other tenant named in a report is another tenant's data.
pub struct OwnTenant {
    names: Vec<String>,
}

impl OwnTenant {
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self { names: names.into_iter().filter(|n| !n.is_empty()).collect() }
    }

    fn owns(&self, name: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }
}

/// Screened counts the text fields of a report the screening changed, by what it found in them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Screened {
    pub secrets: usize,
    pub other_tenants: usize,
    pub identities: usize,
}

impl Screened {
    pub fn clean(&self) -> bool {
        *self == Screened::default()
    }
}

/// screen redacts, in every text field of the report, secrets, other tenants' identifiers and users'
/// identities, and counts the fields it changed.
pub fn screen(report: &mut Report, own: &OwnTenant) -> Screened {
    let mut found = Screened::default();
    for field in text_fields(report) {
        screen_text(field, own, &mut found);
    }
    found
}

fn screen_text(text: &mut String, own: &OwnTenant, found: &mut Screened) {
    let mut secret = false;
    let redacted = logger::redact::redact_credentials(text);
    if redacted != *text {
        *text = redacted;
        secret = true;
    }
    for re in SECRETS.iter() {
        if re.is_match(text) {
            *text = re.replace_all(text, |c: &Captures| format!("{}{REDACTED}", c.get(1).map_or("", |m| m.as_str()))).into_owned();
            secret = true;
        }
    }
    found.secrets += secret as usize;

    let mut other = false;
    let replaced = TENANT.replace_all(text, |c: &Captures| {
        if own.owns(&c[2]) {
            c[0].to_string()
        } else {
            other = true;
            format!("{}{REDACTED}", &c[1])
        }
    });
    if other {
        *text = replaced.into_owned();
        found.other_tenants += 1;
    }

    if USER.is_match(text) {
        *text = USER.replace_all(text, |c: &Captures| format!("{}{REDACTED}", &c[1])).into_owned();
        found.identities += 1;
    }
}

/// Every text field of a report. A field added to the report's messages must be added here.
fn text_fields(r: &mut Report) -> Vec<&mut String> {
    let mut fields = vec![&mut r.report_id, &mut r.summary];
    if let Some(s) = r.symptom.as_mut() {
        fields.extend([&mut s.service, &mut s.method, &mut s.error]);
    }
    if let Some(p) = r.reproduction.as_mut() {
        fields.push(&mut p.attempts);
        for e in p.steps.iter_mut() {
            fields.extend([&mut e.method, &mut e.request_json, &mut e.response_json, &mut e.call_trace_id]);
        }
    }
    if let Some(ev) = r.evidence.as_mut() {
        for e in ev.exchanges.iter_mut() {
            fields.extend([&mut e.method, &mut e.request_json, &mut e.response_json, &mut e.call_trace_id]);
        }
        for t in ev.trace_events.iter_mut() {
            fields.extend([&mut t.trace_id, &mut t.error]);
        }
        for l in ev.logs.iter_mut() {
            fields.extend([&mut l.service, &mut l.level, &mut l.text]);
        }
        for v in ev.versions.iter_mut() {
            fields.extend([&mut v.component, &mut v.version]);
        }
    }
    if let Some(c) = r.classification.as_mut() {
        fields.extend([&mut c.component, &mut c.rationale]);
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::{screen, OwnTenant, Screened};
    use admiral_model::types::services::admiral::{Classification, Evidence, Exchange, LogLine, Report, Reproduction, Symptom};

    fn acme() -> OwnTenant {
        OwnTenant::new(["acme".to_string(), "aaaaaaaa".to_string()])
    }

    fn report(text: &str) -> Report {
        Report {
            report_id: "r-1".to_string(),
            revision: 1,
            summary: text.to_string(),
            symptom: Some(Symptom { service: "orders".to_string(), ..Default::default() }),
            reproduction: Some(Reproduction::default()),
            classification: Some(Classification::default()),
            ..Default::default()
        }
    }

    #[test]
    fn a_clean_report_is_left_as_it_is() {
        let mut r = report("GetOrder answers NOT_FOUND in /tenants/acme for an order just created");
        let before = r.clone();
        assert!(screen(&mut r, &acme()).clean());
        assert_eq!(r, before);
    }

    #[test]
    fn secrets_are_redacted_wherever_they_are() {
        let jwt = "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJhbGljZSJ9.c2lnbmF0dXJl";
        let secrets = [
            "authorization: Bearer abcdefghijklmnopqrstuvwxyz",
            r#"{"x-api-token":"s3cr3t-value"}"#,
            "-----BEGIN PRIVATE KEY-----\nMIIBVQIBADANBg\n-----END PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBVQIBADANBg (cut)",
            jwt,
            r#"{"password":"hunter2"}"#,
            "login with password=hunter2 failed",
        ];
        for secret in secrets {
            let mut r = report("ok");
            r.evidence = Some(Evidence {
                logs: vec![LogLine { text: format!("before {secret} after"), ..Default::default() }],
                ..Default::default()
            });
            let found = screen(&mut r, &acme());
            assert_eq!(found, Screened { secrets: 1, ..Default::default() }, "{secret}");
            let text = &r.evidence.as_ref().unwrap().logs[0].text;
            for part in ["abcdefghijklmnopqrstuvwxyz", "s3cr3t-value", "MIIBVQIBADANBg", jwt, "hunter2"] {
                assert!(!text.contains(part), "{secret} must be redacted: {text}");
            }
            assert!(text.contains("[REDACTED]"), "{text}");
        }
    }

    #[test]
    fn another_tenants_identifiers_are_redacted_and_its_own_kept() {
        let mut r = report("orders in /tenants/beta fail like /tenants/acme");
        r.reproduction.as_mut().unwrap().steps = vec![Exchange {
            request_json: r#"{"peer":"spiffe://example.com/tenants/bbbbbbbb/services/orders","self":"spiffe://example.com/tenants/aaaaaaaa/services/orders"}"#.to_string(),
            ..Default::default()
        }];
        let found = screen(&mut r, &acme());
        assert_eq!(found, Screened { other_tenants: 2, ..Default::default() });
        assert_eq!(r.summary, "orders in /tenants/[REDACTED] fail like /tenants/acme");
        let step = &r.reproduction.as_ref().unwrap().steps[0].request_json;
        assert!(!step.contains("bbbbbbbb") && step.contains("tenants/aaaaaaaa"), "{step}");
    }

    #[test]
    fn a_users_identity_is_redacted() {
        let mut r = report("alice (spiffe://example.com/tenants/acme/users/alice) saw it");
        let found = screen(&mut r, &acme());
        assert_eq!(found, Screened { identities: 1, ..Default::default() });
        assert_eq!(r.summary, "alice (spiffe://example.com/tenants/acme/users/[REDACTED]) saw it");
    }

    #[test]
    fn every_text_field_is_screened() {
        let secret = "password=hunter2";
        let mut r = report(secret);
        r.symptom = Some(Symptom { error: secret.to_string(), ..Default::default() });
        r.reproduction.as_mut().unwrap().attempts = secret.to_string();
        r.evidence = Some(Evidence {
            exchanges: vec![Exchange { response_json: secret.to_string(), ..Default::default() }],
            ..Default::default()
        });
        r.classification = Some(Classification { rationale: secret.to_string(), ..Default::default() });
        assert_eq!(screen(&mut r, &acme()).secrets, 5);
        assert!(!format!("{r:?}").contains("hunter2"));
    }
}
