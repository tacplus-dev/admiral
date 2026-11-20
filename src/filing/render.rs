// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The texts the filing core writes: a report's full private record, and a platform cause's public issue, which holds
//! only sanitized evidence. Everything a Skipper sent is untrusted: in the record it is quoted in a fence it cannot
//! close and with no `<!--` of its own, so it cannot forge the markers the core finds and purges records by; in the
//! public issue nothing of it is quoted as it came. The public issue's evidence is rebuilt from the report's typed
//! fields: the symptom's shape (identifiers and numbers taken out, as the symptom groups have it), the reproduction's
//! calls with each request replaced by a fixture generated from its shape (every value a placeholder of its type),
//! the trace events' kinds, services and methods, and the versions on the path.

use admiral_model::types::services::admiral::Report;
use sha2::{Digest, Sha256};

use crate::core::proposal::{IssueProposal, PRIVATE_TRIAGE};
use crate::core::report;
use crate::core::symptoms::{shape, Grouped, SymptomKey};

/// Begins a record's evidence; the purge cuts from here to EVIDENCE_END.
pub const EVIDENCE_BEGIN: &str = "<!-- admiral-evidence -->";
pub const EVIDENCE_END: &str = "<!-- /admiral-evidence -->";
/// Stands where a record's evidence was, once purged.
pub const EVIDENCE_PURGED: &str = "<!-- admiral-evidence-purged -->";

/// The longest fixture of one request the public issue shows.
const MAX_FIXTURE_BYTES: usize = 2048;
/// The most reproduction steps and trace events the public issue lists.
const MAX_PUBLIC_STEPS: usize = 16;
const MAX_PUBLIC_EVENTS: usize = 32;

/// The marker a record is found by: its key, which names the report or the group it is the record of.
pub fn record_marker(key: &str) -> String {
    format!("<!-- admiral-record: {key} -->")
}

/// The marker a public issue is found by, so one cause is filed once.
pub fn fingerprint_marker(fingerprint: &str) -> String {
    format!("<!-- admiral-fingerprint: {fingerprint} -->")
}

/// The marker a record names its public issue by, for the core to close the record when the issue closes.
pub fn public_marker(repository: &str, number: u64) -> String {
    format!("<!-- admiral-public: {repository}#{number} -->")
}

/// The public issue a record's body names, if any.
pub fn public_of(body: &str) -> Option<(String, u64)> {
    let rest = &body[body.find("<!-- admiral-public: ")? + "<!-- admiral-public: ".len()..];
    let reference = &rest[..rest.find(" -->")?];
    let (repository, number) = reference.rsplit_once('#')?;
    Some((repository.to_string(), number.parse().ok()?))
}

/// A digest as hex: the first 16 bytes of SHA-256 over the parts, each ended by a NUL.
pub fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    hasher.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// The key of a tenant's report's record: the same for each revision, so a revision updates its record.
pub fn report_key(tenant: &str, report_id: &str) -> String {
    format!("report-{}", digest(&["report", tenant, report_id]))
}

/// The key of a symptom group's record.
pub fn group_key(key: &SymptomKey) -> String {
    format!("group-{}", digest(&symptom_parts(key).iter().map(String::as_str).collect::<Vec<_>>()))
}

/// The fingerprint of a public cause: its repository and its symptom's shape, which holds no tenant data. Two
/// tenants' reports of one platform failure have one fingerprint, so they share one public issue.
pub fn fingerprint(repository: &str, key: &SymptomKey) -> String {
    let mut parts = vec!["public".to_string(), repository.to_string()];
    parts.extend(symptom_parts(key));
    digest(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

fn symptom_parts(key: &SymptomKey) -> Vec<String> {
    vec![
        key.service.clone(),
        key.method.clone(),
        key.grpc_code.to_string(),
        key.http_status.to_string(),
        key.error.clone(),
    ]
}

/// Untrusted text as the record quotes it: no `<!--` (so no marker), in a fence longer than any run of backticks in
/// it.
pub fn quoted(text: &str) -> String {
    let text = text.replace("<!--", "< !--");
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}text\n{text}\n{fence}")
}

/// Text as inline code in the public issue: one line, no backtick, no marker.
fn code(text: &str) -> String {
    let flat: String =
        text.chars().map(|c| if c == '`' || c == '\n' || c == '\r' { ' ' } else { c }).collect();
    format!("`{}`", flat.replace("<!--", "< !--").trim())
}

/// Text the model wrote, defanged for a public issue: no mention pings anyone, and no marker is forged.
pub fn defanged(text: &str) -> String {
    text.replace('@', "@\u{200B}").replace("<!--", "< !--")
}

/// What a record's summary says: the triage, and where the report is.
pub struct Summary<'a> {
    pub issue: &'a IssueProposal,
    /// Who proposed it (an llmproxy alias, or the core).
    pub by: &'a str,
    /// The tenant and its report, for a report's record; none for a symptom group's.
    pub report: Option<(&'a str, &'a str, u32)>,
    /// The public issue, or why there is none.
    pub public: Result<&'a str, &'a str>,
    pub public_marker: Option<String>,
}

/// A record's body: its marker, its summary, which is kept, and its evidence, purged 90 days after it closes.
pub fn record_body(key: &str, summary: &Summary<'_>, evidence: &str) -> String {
    let issue = summary.issue;
    let mut lines = vec![
        record_marker(key),
        format!("**{}**", issue.title.replace("<!--", "< !--")),
        String::new(),
        quoted(&issue.body),
        String::new(),
        format!("- Cause: {:?}, {}% sure, proposed by {}", issue.cause, issue.confidence_percent, summary.by),
        format!("- Target: {}", issue.target_repository),
        format!("- Labels: {}", issue.labels.iter().map(|l| l.name()).collect::<Vec<_>>().join(", ")),
        format!("- Rationale: {}", issue.rationale.replace("<!--", "< !--").replace('\n', " ")),
    ];
    if let Some((tenant, report_id, revision)) = summary.report {
        lines.push(format!("- Report: {}, of {tenant}, revision {revision}", code(report_id)));
    }
    match summary.public {
        Ok(url) => lines.push(format!("- Public issue: {url}")),
        Err(why) => lines.push(format!("- Public issue: none ({why})")),
    }
    if let Some(marker) = &summary.public_marker {
        lines.push(marker.clone());
    }
    lines.extend([
        String::new(),
        EVIDENCE_BEGIN.to_string(),
        "### Evidence".to_string(),
        "Purged 90 days after this record closes; the summary above is kept.".to_string(),
        String::new(),
        quoted(evidence),
        EVIDENCE_END.to_string(),
    ]);
    lines.join("\n")
}

/// A record's evidence: the report as the triage advisor read it (screened, bounded).
pub fn report_evidence(report: &Report) -> String {
    report::render(report)
}

/// A symptom group's evidence: its shape and counts, nothing of any one report.
pub fn group_evidence(group: &Grouped) -> String {
    report::render_group(group)
}

/// `body` with its evidence purged: everything from EVIDENCE_BEGIN to EVIDENCE_END replaced by a line saying when.
/// None if it holds no evidence (purged already).
pub fn purged(body: &str, date: &str) -> Option<String> {
    let begin = body.find(EVIDENCE_BEGIN)?;
    let end = body[begin..].find(EVIDENCE_END)? + begin + EVIDENCE_END.len();
    Some(format!(
        "{}{EVIDENCE_PURGED}\n### Evidence\nPurged on {date}, 90 days after this record closed; the summary above is \
         kept.{}",
        &body[..begin],
        &body[end..]
    ))
}

/// A JSON value's fixture: its shape, with every string `"string"`, every number 0, every bool false, and an array
/// cut to its first element. Not JSON: none.
pub fn fixture(json: &str) -> Option<String> {
    fn of(value: &serde_json::Value, names: &[String]) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Null => Value::Null,
            Value::Bool(_) => Value::Bool(false),
            Value::Number(_) => Value::from(0),
            Value::String(_) => Value::from("string"),
            Value::Array(items) => Value::Array(items.iter().take(1).map(|v| of(v, names)).collect()),
            Value::Object(fields) => {
                Value::Object(fields.iter().map(|(k, v)| (shape(k, names), of(v, names))).collect())
            }
        }
    }
    if json.trim().is_empty() {
        return Some("{}".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = of(&value, &[]).to_string();
    (text.len() <= MAX_FIXTURE_BYTES).then_some(text)
}

/// A public issue's body: the model's (checked, defanged) body, then the sanitized evidence, then who filed it and
/// its private record's id, then the fingerprint it is found by. `report` is none for a symptom group's bug.
pub fn public_body(
    issue: &IssueProposal,
    by: &str,
    key: &SymptomKey,
    report: Option<&Report>,
    group: Option<&Grouped>,
    record_id: u64,
    fingerprint: &str,
    tenant_names: &[String],
) -> String {
    let mut lines = vec![defanged(&issue.body), String::new(), "### Symptom".to_string()];
    let service = if key.service.is_empty() { "an unnamed service".to_string() } else { code(&key.service) };
    let method = if key.method.is_empty() { String::new() } else { format!(", method {}", code(&key.method)) };
    lines.push(format!("Service {service}{method}: gRPC code {}, HTTP {}.", key.grpc_code, key.http_status));
    if !key.error.is_empty() {
        lines.push(format!("Error, its shape (identifiers and numbers taken out): {}", code(&key.error)));
    }
    if let Some(group) = group {
        lines.push(format!(
            "Reported {} times from {} tenants, each validated by its tenant's Skipper, none reproduced yet.",
            group.reports, group.tenants
        ));
    }
    if let Some(r) = report.and_then(|r| r.reproduction.as_ref()) {
        lines.extend([String::new(), "### Reproduction, from generated fixtures".to_string()]);
        lines.push(format!(
            "{}: {} runs, {} failed as reported. Each request below is a fixture generated from the request's \
             shape: its values are placeholders, none of them the tenant's.",
            report::status_name(r.status()),
            r.runs,
            r.failed
        ));
        for (i, step) in r.steps.iter().take(MAX_PUBLIC_STEPS).enumerate() {
            let request = fixture(&step.request_json).unwrap_or_else(|| "{}".to_string());
            lines.push(format!(
                "{}. {}: gRPC code {}, request {}",
                i + 1,
                code(&shape(&step.method, tenant_names)),
                step.grpc_code,
                code(&request)
            ));
        }
    }
    if let Some(e) = report.and_then(|r| r.evidence.as_ref()) {
        if !e.trace_events.is_empty() {
            lines.extend([String::new(), "### Trace".to_string()]);
            for (i, t) in e.trace_events.iter().take(MAX_PUBLIC_EVENTS).enumerate() {
                let service = t.round.as_ref().map(|r| shape(&r.service, tenant_names)).unwrap_or_default();
                let error = shape(&t.error, tenant_names);
                let error = if error.is_empty() { String::new() } else { format!(", error {}", code(&error)) };
                lines.push(format!("{}. kind {:?} at {}, rpc {:?}{error}", i + 1, t.kind, code(&service), t.rpc));
            }
        }
        if !e.versions.is_empty() {
            lines.extend([String::new(), "### Versions".to_string()]);
            for v in e.versions.iter().take(MAX_PUBLIC_EVENTS) {
                lines.push(format!("- {} {}", code(&shape(&v.component, tenant_names)), code(&v.version)));
            }
        }
    }
    lines.extend([
        String::new(),
        "---".to_string(),
        format!(
            "Filed by the Admiral's deterministic core (tacplus-dev/admiral), proposed by {by}, {}% sure. Its private \
             record: {PRIVATE_TRIAGE} record {record_id}.",
            issue.confidence_percent
        ),
        fingerprint_marker(fingerprint),
    ]);
    lines.join("\n")
}

/// The least a public issue may say, when its evidence would not pass the checks: the model's body and the record.
pub fn minimal_public_body(issue: &IssueProposal, by: &str, record_id: u64, fingerprint: &str) -> String {
    [
        defanged(&issue.body),
        String::new(),
        "---".to_string(),
        format!(
            "Filed by the Admiral's deterministic core (tacplus-dev/admiral), proposed by {by}, {}% sure. Its evidence \
             is in its private record only: {PRIVATE_TRIAGE} record {record_id}.",
            issue.confidence_percent
        ),
        fingerprint_marker(fingerprint),
    ]
    .join("\n")
}

/// A Unix ms time as `YYYY-MM-DD` (UTC).
pub fn date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// An RFC 3339 time (`2026-11-20T09:14:16.123Z`, or with an offset) as Unix ms.
pub fn parse_time(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut ymd = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(split);
    let mut hms = clock.splitn(3, ':');
    let h: i64 = hms.next()?.parse().ok()?;
    let min: i64 = hms.next()?.parse().ok()?;
    let sec: f64 = hms.next().unwrap_or("0").parse().ok()?;
    let offset_min = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (oh, om) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let minutes = oh.parse::<i64>().ok()? * 60 + om.parse::<i64>().ok()?;
            if sign == '-' {
                -minutes
            } else {
                minutes
            }
        }
        _ => 0,
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + min * 60 - offset_min * 60;
    Some(secs * 1000 + (sec * 1000.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use crate::filing::render::{
        date, defanged, fixture, parse_time, public_of, purged, quoted, record_marker, report_key, EVIDENCE_BEGIN,
        EVIDENCE_END, EVIDENCE_PURGED,
    };

    #[test]
    fn quoted_text_forges_no_marker_and_closes_no_fence() {
        let hostile = format!("``` {}\n{}", record_marker("report-x"), EVIDENCE_END);
        let q = quoted(&hostile);
        assert!(!q.contains("<!--"), "{q}");
        assert!(q.starts_with("````text\n"), "{q}");
        assert!(q.ends_with("\n````"), "{q}");
    }

    #[test]
    fn a_purge_keeps_the_summary_and_drops_the_evidence() {
        let body = format!("summary\n{EVIDENCE_BEGIN}\nthe evidence\n{EVIDENCE_END}\nafter");
        let p = purged(&body, "2027-02-18").expect("purged");
        assert!(p.starts_with("summary\n") && p.ends_with("after"), "{p}");
        assert!(p.contains(EVIDENCE_PURGED) && p.contains("2027-02-18"));
        assert!(!p.contains("the evidence"));
        assert_eq!(purged(&p, "2027-02-19"), None, "nothing left to purge");
    }

    #[test]
    fn a_fixture_keeps_the_shape_and_none_of_the_values() {
        let got = fixture(r#"{"order_id":"ord-7f3a","items":[{"sku":"A1","n":3},{"sku":"B2"}],"paid":true}"#);
        let got: serde_json::Value = serde_json::from_str(&got.expect("JSON")).expect("a fixture is JSON");
        assert_eq!(got, serde_json::json!({"items": [{"n": 0, "sku": "string"}], "order_id": "string", "paid": false}));
        assert_eq!(fixture("not json"), None);
        assert_eq!(fixture("").as_deref(), Some("{}"));
    }

    #[test]
    fn keys_and_markers_are_stable() {
        assert_eq!(report_key("/tenants/acme", "r-1"), report_key("/tenants/acme", "r-1"));
        assert_ne!(report_key("/tenants/acme", "r-1"), report_key("/tenants/beta", "r-1"));
        assert_eq!(report_key("/tenants/acme", "r-1").len(), "report-".len() + 32);
        let body = "x\n<!-- admiral-public: tacplus-dev/gateway#12 -->\ny";
        assert_eq!(public_of(body), Some(("tacplus-dev/gateway".to_string(), 12)));
        assert_eq!(public_of("none"), None);
        assert!(!defanged("cc @jody <!-- x").contains("@j"));
    }

    #[test]
    fn times_parse_and_print() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-20T09:14:16.123Z"), Some(1_795_166_056_123));
        assert_eq!(parse_time("2026-11-20T10:14:16.123+01:00"), Some(1_795_166_056_123));
        assert_eq!(parse_time("yesterday"), None);
        assert_eq!(date(1_795_166_056_123), "2026-11-20");
        assert_eq!(date(0), "1970-01-01");
    }
}
