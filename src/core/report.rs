// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! A report, and a symptom group, as text for the triage advisor's model. The report is the one intake screened
//! (src/screen.rs) and holds; the text is still untrusted data, which the advisor sends only inside its nonce-keyed
//! block. Every field is bounded here as well as by the API, so one report cannot crowd out the rest of its own
//! evidence, nor make a triage cost what a megabyte of prompt costs.

use std::fmt::Write as _;

use admiral_model::types::services::admiral::{Cause, Exchange, Report, ReproductionStatus};

use crate::core::symptoms::{Grouped, SymptomKey};

/// The most bytes of one text field the model is shown.
pub const MAX_FIELD_BYTES: usize = 8192;
/// The most bytes of one report the model is shown.
pub const MAX_MATERIAL_BYTES: usize = 131072;

/// `text`, cut to `max` bytes on a character boundary, with how much was cut.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[... {} more bytes]", &text[..end], text.len() - end)
}

fn field(text: &str) -> String {
    cut(text, MAX_FIELD_BYTES)
}

pub fn status_name(status: ReproductionStatus) -> &'static str {
    match status {
        ReproductionStatus::Reproduced => "reproduced",
        ReproductionStatus::NotYetReproduced => "not yet reproduced",
        ReproductionStatus::Unspecified => "unspecified",
    }
}

fn cause_name(cause: Cause) -> &'static str {
    match cause {
        Cause::TenantCode => "tenant_code",
        Cause::PlatformService => "platform_service",
        Cause::Generator => "generator",
        Cause::Unknown => "unknown",
        Cause::Unspecified => "unspecified",
    }
}

fn exchange(out: &mut Vec<String>, what: &str, i: usize, e: &Exchange) {
    out.push(format!(
        "{what} {i}: method {:?}, grpc_code {}, trace {:?}",
        field(&e.method),
        e.grpc_code,
        field(&e.call_trace_id)
    ));
    out.push(format!("{what} {i} request: {}", field(&e.request_json)));
    out.push(format!("{what} {i} response: {}", field(&e.response_json)));
}

/// The report as labelled lines, its summary and symptom first, its evidence last, cut at MAX_MATERIAL_BYTES.
pub fn render(report: &Report) -> String {
    let mut lines = vec![format!("summary: {}", field(&report.summary))];
    if let Some(s) = &report.symptom {
        lines.push(format!(
            "symptom: service {:?}, method {:?}, grpc_code {}, http_status {}",
            field(&s.service),
            field(&s.method),
            s.grpc_code,
            s.http_status
        ));
        lines.push(format!("symptom error: {}", field(&s.error)));
    }
    if let Some(c) = &report.classification {
        lines.push(format!(
            "the Skipper's classification: cause {}, component {:?}, confidence {}%",
            cause_name(c.cause()),
            field(&c.component),
            c.confidence
        ));
        lines.push(format!("the Skipper's rationale: {}", field(&c.rationale)));
    }
    if let Some(r) = &report.reproduction {
        lines.push(format!(
            "reproduction: {}, {} runs, {} failed as reported",
            status_name(r.status()),
            r.runs,
            r.failed
        ));
        lines.push(format!("reproduction attempts: {}", field(&r.attempts)));
        for (i, step) in r.steps.iter().enumerate() {
            exchange(&mut lines, "reproduction step", i + 1, step);
        }
    }
    if let Some(e) = &report.evidence {
        if !e.versions.is_empty() {
            let versions: Vec<String> =
                e.versions.iter().map(|v| format!("{} {}", field(&v.component), field(&v.version))).collect();
            lines.push(format!("versions: {}", versions.join(", ")));
        }
        for (i, x) in e.exchanges.iter().enumerate() {
            exchange(&mut lines, "reporter's call", i + 1, x);
        }
        for (i, t) in e.trace_events.iter().enumerate() {
            lines.push(format!("trace event {}: {}", i + 1, field(&format!("{t:?}"))));
        }
        for (i, l) in e.logs.iter().enumerate() {
            lines.push(format!(
                "log {}: {} {} at {} ms: {}",
                i + 1,
                field(&l.service),
                field(&l.level),
                l.timestamp_ms,
                field(&l.text)
            ));
        }
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if out.len() + line.len() + 1 > MAX_MATERIAL_BYTES {
            let _ = write!(out, "[... {} more lines of evidence not shown]", lines.len() - i);
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// A symptom group as labelled lines: the symptom's shape and the counts, nothing of any one report.
pub fn render_group(group: &Grouped) -> String {
    let SymptomKey { service, method, grpc_code, http_status, error } = &group.key;
    [
        format!("symptom: service {service:?}, method {method:?}, grpc_code {grpc_code}, http_status {http_status}"),
        format!("symptom error, its shape (identifiers and numbers taken out): {error}"),
        format!(
            "not yet reproduced: {} reports from {} tenants, each validated by its tenant's own Skipper, none \
             reproduced yet",
            group.reports, group.tenants
        ),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use admiral_model::types::services::admiral::{
        Classification, ComponentVersion, Evidence, Exchange, LogLine, Report, Reproduction, ReproductionStatus,
        Symptom,
    };

    use crate::core::report::{cut, render, render_group, MAX_FIELD_BYTES, MAX_MATERIAL_BYTES};
    use crate::core::symptoms::{Grouped, SymptomKey};

    fn report() -> Report {
        Report {
            report_id: "r-1".to_string(),
            revision: 1,
            summary: "sign-in fails after the token rotates".to_string(),
            symptom: Some(Symptom {
                service: "gateway".to_string(),
                method: "gateway.Gateway/Route".to_string(),
                grpc_code: 16,
                http_status: 401,
                error: "token expired".to_string(),
                ..Default::default()
            }),
            reproduction: Some(Reproduction {
                status: ReproductionStatus::NotYetReproduced as i32,
                runs: 3,
                failed: 0,
                attempts: "signed in three times after a rotation".to_string(),
                steps: vec![Exchange { method: "gateway.Gateway/Route".to_string(), ..Default::default() }],
                ..Default::default()
            }),
            evidence: Some(Evidence {
                versions: vec![ComponentVersion {
                    component: "gateway".to_string(),
                    version: "2.6.13".to_string(),
                    ..Default::default()
                }],
                logs: vec![LogLine {
                    service: "gateway".to_string(),
                    level: "WARN".to_string(),
                    text: "expired".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            classification: Some(Classification {
                cause: 2,
                component: "gateway".to_string(),
                confidence: 60,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_report_renders_as_labelled_lines() {
        let text = render(&report());
        for line in [
            "summary: sign-in fails after the token rotates",
            "symptom: service \"gateway\", method \"gateway.Gateway/Route\", grpc_code 16, http_status 401",
            "the Skipper's classification: cause platform_service, component \"gateway\", confidence 60%",
            "reproduction: not yet reproduced, 3 runs, 0 failed as reported",
            "reproduction step 1: method \"gateway.Gateway/Route\", grpc_code 0, trace \"\"",
            "versions: gateway 2.6.13",
            "log 1: gateway WARN at 0 ms: expired",
        ] {
            assert!(text.lines().any(|l| l == line), "{line:?} in\n{text}");
        }
    }

    #[test]
    fn a_report_is_bounded() {
        let mut big = report();
        big.summary = "é".repeat(MAX_FIELD_BYTES);
        let evidence = big.evidence.as_mut().unwrap();
        evidence.logs = (0..1024).map(|_| LogLine { text: "x".repeat(4096), ..Default::default() }).collect();
        let text = render(&big);
        assert!(text.len() <= MAX_MATERIAL_BYTES + 64, "{}", text.len());
        assert!(text.ends_with("more lines of evidence not shown]"));
        assert!(text.lines().next().unwrap().ends_with(&format!("[... {} more bytes]", MAX_FIELD_BYTES)));
        assert_eq!(cut("abc", 2), "ab[... 1 more bytes]");
    }

    #[test]
    fn a_group_renders_its_shape_and_counts_only() {
        let group = Grouped {
            key: SymptomKey {
                service: "gateway".to_string(),
                method: "gateway.Gateway/Route".to_string(),
                grpc_code: 16,
                http_status: 401,
                error: "token for /tenants/[tenant] expired at #".to_string(),
            },
            reports: 3,
            tenants: 2,
            escalate: true,
        };
        let text = render_group(&group);
        assert!(text.contains("3 reports from 2 tenants"));
        assert!(text.contains("token for /tenants/[tenant] expired at #"));
    }
}
