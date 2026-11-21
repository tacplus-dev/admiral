// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use admiral_model::types::services::admiral::{
    ComponentVersion, Evidence, Exchange, LogLine, Report, Reproduction, ReproductionStatus, Symptom,
};

use crate::core::proposal::{Cause, IssueProposal, Label, Rejection, PRIVATE_TRIAGE};
use crate::core::symptoms::{Grouped, SymptomKey};
use crate::core::{Escalation, Proposed, ProposedBy, Reporter};
use crate::filing::memory::{MemoryProject, MemoryTracker};
use crate::filing::render::{record_marker, report_key, EVIDENCE_PURGED};
use crate::filing::{Filed, Filing, FilingError, LABEL_EVIDENCE, LABEL_PUBLIC, LABEL_PURGED, PURGE_AFTER_MS};

const GATEWAY: &str = "tacplus-dev/gateway";

struct Fixture {
    project: Arc<MemoryProject>,
    tracker: Arc<MemoryTracker>,
    filing: Filing,
}

fn fixture() -> Fixture {
    let project = Arc::new(MemoryProject::default());
    let tracker = Arc::new(MemoryTracker::default());
    let filing = Filing::new(project.clone(), tracker.clone());
    assert!(filing.allow_public([GATEWAY.to_string()]).is_empty());
    Fixture { project, tracker, filing }
}

fn proposed(cause: Cause, target: &str) -> Proposed {
    Proposed {
        issue: IssueProposal {
            cause,
            confidence_percent: 80,
            rationale: "the trace fails in the gateway's auth step".to_string(),
            target_repository: target.to_string(),
            title: "gateway: UNAUTHENTICATED after rotation".to_string(),
            body: "gateway.Gateway/Route answers UNAUTHENTICATED for a token minted after rotation.".to_string(),
            labels: vec![Label::WrongAnswer, Label::Bug],
        },
        by: ProposedBy::Model("claude-opus-latest".to_string()),
    }
}

fn report(id: &str, revision: u32, tenant: &str) -> Report {
    Report {
        report_id: id.to_string(),
        revision,
        summary: format!("{tenant}'s users cannot sign in after the token rotates"),
        symptom: Some(Symptom {
            service: "gateway".to_string(),
            method: "gateway.Gateway/Route".to_string(),
            grpc_code: 16,
            http_status: 401,
            error: format!("token for /tenants/{tenant}/services/web expired at 1731000000"),
            ..Default::default()
        }),
        reproduction: Some(Reproduction {
            status: ReproductionStatus::Reproduced as i32,
            runs: 3,
            failed: 3,
            steps: vec![Exchange {
                method: "gateway.Gateway/Route".to_string(),
                request_json: format!(r#"{{"order_id":"{tenant}-order-7","items":[{{"sku":"A1"}}]}}"#),
                grpc_code: 16,
                ..Default::default()
            }],
            ..Default::default()
        }),
        evidence: Some(Evidence {
            versions: vec![ComponentVersion {
                component: "gateway".to_string(),
                version: "2.6.13".to_string(),
                ..Default::default()
            }],
            logs: vec![LogLine { text: format!("{tenant} order 7 refused"), ..Default::default() }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

impl Fixture {
    /// Files `tenant`'s report as `proposed`.
    async fn file(&self, trace: &str, tenant: &str, report: &Report, p: &Proposed) -> Result<Filed, FilingError> {
        let (key, names) = (format!("/tenants/{tenant}"), vec![tenant.to_string()]);
        self.filing.file_report(trace, Reporter { tenant: &key, tenant_names: &names }, report, p).await
    }
}

// A platform cause: the private record first, then the public issue, which holds only sanitized evidence and names
// the record by id; then the record names the issue. Every forge call carries the report's trace (looper#51).
#[tokio::test]
async fn a_platform_cause_gets_its_private_record_first_then_a_sanitized_public_issue() {
    let f = fixture();
    let p = proposed(Cause::PlatformService, GATEWAY);
    let filed = f.file("trace-r1", "acme", &report("r-1", 1, "acme"), &p).await.expect("filed");
    let public = filed.public.clone().expect("a public issue");
    assert_eq!((public.repository.as_str(), filed.duplicate, filed.withheld.as_deref()), (GATEWAY, false, None));

    let project_calls: Vec<&str> = f.project.calls.lock().unwrap().iter().map(|c| c.what).collect();
    assert_eq!(project_calls, ["find", "create", "update"]);
    let tracker_calls: Vec<&str> = f.tracker.calls.lock().unwrap().iter().map(|c| c.what).collect();
    assert_eq!(tracker_calls, ["find_open", "open"]);
    assert!(f.project.traces().iter().chain(f.tracker.traces().iter()).all(|t| t == "trace-r1"));

    let record = f.project.record(filed.record).expect("the record");
    assert!(record.body.contains(&record_marker(&report_key("/tenants/acme", "r-1"))));
    assert!(record.body.contains("acme's users cannot sign in"), "the record holds the full evidence");
    assert!(record.body.contains(&public.url), "the record links its public issue");
    assert!(record.labels.iter().any(|l| l == LABEL_PUBLIC) && record.labels.iter().any(|l| l == LABEL_EVIDENCE));

    let issues = f.tracker.issues.lock().unwrap();
    let issue = &issues[0].issue;
    assert_eq!(issue.labels, ["wrong-answer", "bug"]);
    assert!(!issue.body.to_ascii_lowercase().contains("acme"), "{}", issue.body);
    assert!(!issue.body.contains("order 7") && !issue.body.contains("r-1"), "{}", issue.body);
    assert!(issue.body.contains(&format!("{PRIVATE_TRIAGE} record {}", filed.record)), "{}", issue.body);
    assert!(!issue.body.contains("gitlab.com"), "the record is named by id only");
    assert!(issue.body.contains("### Reproduction, from generated fixtures"), "{}", issue.body);
    assert!(issue.body.contains(r#""order_id":"string""#), "{}", issue.body);
    assert!(issue.body.contains("token for /tenants/[tenant]/services/web expired at #"), "{}", issue.body);
    assert!(issue.body.contains("`gateway` `2.6.13`"), "{}", issue.body);
}

#[tokio::test]
async fn nothing_is_public_unless_the_private_record_is_written() {
    let f = fixture();
    *f.project.failing.lock().unwrap() = true;
    let got = f.file("t", "acme", &report("r-1", 1, "acme"), &proposed(Cause::PlatformService, GATEWAY)).await;
    assert!(matches!(got, Err(FilingError::Forge(_))), "{got:?}");
    assert!(f.tracker.calls.lock().unwrap().is_empty(), "no public call");
}

#[tokio::test]
async fn a_cause_outside_tacplus_gets_its_private_record_only() {
    let f = fixture();
    let mut p = proposed(Cause::TenantCode, PRIVATE_TRIAGE);
    p.issue.labels = vec![Label::NotYetReproduced];
    let filed = f.file("t", "acme", &report("r-1", 1, "acme"), &p).await.expect("filed");
    assert!(filed.public.is_none() && filed.withheld.is_none());
    assert!(f.tracker.calls.lock().unwrap().is_empty());
    assert!(f.project.record(filed.record).unwrap().body.contains("none (the cause is not the platform's)"));
}

#[tokio::test]
async fn a_target_off_the_allow_list_is_not_filed_in_public() {
    let f = fixture();
    let p = proposed(Cause::PlatformService, "tacplus-dev/kernel");
    let filed = f.file("t", "acme", &report("r-1", 1, "acme"), &p).await.expect("filed");
    assert!(filed.public.is_none());
    assert_eq!(filed.withheld.as_deref(), Some("tacplus-dev/kernel is not on this deployment's allow-list"));
    assert!(f.tracker.calls.lock().unwrap().is_empty());
    let dropped = f.filing.allow_public(["someone/kernel".to_string(), "tacplus-dev/kernel".to_string()]);
    assert_eq!(dropped, ["someone/kernel"]);
}

#[tokio::test]
async fn a_proposal_that_leaks_is_refused_and_nothing_filed() {
    let f = fixture();
    let mut p = proposed(Cause::PlatformService, GATEWAY);
    p.issue.body = "the acme tenant's sign-in fails".to_string();
    let got = f.file("t", "acme", &report("r-1", 1, "acme"), &p).await;
    assert!(matches!(got, Err(FilingError::Refused(Rejection::Leaks { field: "body", .. }))), "{got:?}");
    assert!(f.project.calls.lock().unwrap().is_empty() && f.tracker.calls.lock().unwrap().is_empty());
}

// Duplicates: a revision updates its report's record; another tenant's report of the same platform failure gets its
// own record and shares the one public issue.
#[tokio::test]
async fn one_record_per_report_and_one_public_issue_per_cause() {
    let f = fixture();
    let p = proposed(Cause::PlatformService, GATEWAY);
    let first = f.file("t1", "acme", &report("r-1", 1, "acme"), &p).await.unwrap();
    let revised = f.file("t2", "acme", &report("r-1", 2, "acme"), &p).await.unwrap();
    assert_eq!(revised.record, first.record);
    assert!(revised.duplicate);
    assert_eq!(revised.public, first.public);
    assert!(f.project.record(first.record).unwrap().body.contains("revision 2"));

    let other = f.file("t3", "beta", &report("r-1", 1, "beta"), &p).await.unwrap();
    assert_ne!(other.record, first.record);
    assert!(other.duplicate);
    assert_eq!(other.public, first.public);
    assert_eq!(f.project.records.lock().unwrap().len(), 2);
    assert_eq!(f.tracker.issues.lock().unwrap().len(), 1);

    // Once the issue closed, the same failure again is a new issue.
    f.tracker.close(first.public.as_ref().unwrap().number);
    let again = f.file("t4", "beta", &report("r-2", 1, "beta"), &p).await.unwrap();
    assert!(!again.duplicate);
    assert_ne!(again.public, first.public);
}

// Evidence that would leak in public (a version naming the tenant) is left out of the public issue: it keeps the
// model's checked body and the record's id.
#[tokio::test]
async fn evidence_that_would_leak_is_left_out_of_the_public_issue() {
    let f = fixture();
    let mut r = report("r-1", 1, "acme");
    r.evidence.as_mut().unwrap().versions[0].version = "acme-build-7".to_string();
    let filed = f.file("t", "acme", &r, &proposed(Cause::PlatformService, GATEWAY)).await.unwrap();
    assert!(filed.public.is_some());
    let issues = f.tracker.issues.lock().unwrap();
    let body = &issues[0].issue.body;
    assert!(!body.to_ascii_lowercase().contains("acme"), "{body}");
    assert!(body.contains("Its evidence is in its private record only"), "{body}");
}

#[tokio::test]
async fn a_symptom_groups_bug_is_filed_with_no_tenant_data() {
    let f = fixture();
    let key = SymptomKey {
        service: "gateway".to_string(),
        method: "gateway.gateway/route".to_string(),
        grpc_code: 16,
        http_status: 401,
        error: "token for /tenants/[tenant]/services/web expired at #".to_string(),
    };
    let escalation = Escalation {
        group: Grouped { key, reports: 3, tenants: 2, escalate: true },
        proposed: Proposed {
            issue: IssueProposal {
                labels: vec![Label::WrongAnswer, Label::Bug, Label::SymptomGroup],
                ..proposed(Cause::PlatformService, GATEWAY).issue
            },
            by: ProposedBy::Model("claude-fable-latest".to_string()),
        },
    };
    let filed = f.filing.file_group("trace-g", &escalation).await.expect("filed");
    let record = f.project.record(filed.record).unwrap();
    assert!(record.body.contains("3 reports from 2 tenants"), "{}", record.body);
    assert!(!record.body.contains("- Report:"), "a group's record names no report");
    let issues = f.tracker.issues.lock().unwrap();
    assert!(issues[0].issue.body.contains("Reported 3 times from 2 tenants"));
    assert!(f.project.traces().iter().all(|t| t == "trace-g"));
    // A platform cause's group opens a public issue too: those calls carry the group's trace as well.
    let tracker_traces = f.tracker.traces();
    assert!(!tracker_traces.is_empty() && tracker_traces.iter().all(|t| t == "trace-g"), "{tracker_traces:?}");
}

// Retention: a record closes when its public issue does; 90 days after it closed, its evidence is purged and its
// summary kept.
#[tokio::test]
async fn a_record_closes_with_its_issue_and_its_evidence_is_purged_90_days_later() {
    let f = fixture();
    let p = proposed(Cause::PlatformService, GATEWAY);
    let filed = f.file("t", "acme", &report("r-1", 1, "acme"), &p).await.unwrap();
    let mut q = proposed(Cause::TenantCode, PRIVATE_TRIAGE);
    q.issue.labels = vec![Label::Bug];
    let private = f.file("t", "acme", &report("r-2", 1, "acme"), &q).await.unwrap();

    let now = 1_800_000_000_000;
    assert_eq!(f.filing.sweep(now).await.unwrap(), Default::default());
    f.tracker.close(filed.public.unwrap().number);
    let swept = f.filing.sweep(now).await.unwrap();
    assert_eq!((swept.closed, swept.purged), (1, 0));
    assert!(!f.project.record(filed.record).unwrap().open);

    // A person closed the private-only record 91 days ago; the public one closed just now.
    f.project.close_at(private.record, now - PURGE_AFTER_MS - 86_400_000);
    let swept = f.filing.sweep(now).await.unwrap();
    assert_eq!((swept.closed, swept.purged), (0, 1));
    let purged = f.project.record(private.record).unwrap();
    assert!(purged.body.contains(EVIDENCE_PURGED), "{}", purged.body);
    assert!(!purged.body.contains("users cannot sign in"), "the evidence is gone");
    assert!(purged.body.contains("gateway: UNAUTHENTICATED after rotation"), "the summary is kept");
    assert!(purged.labels.iter().any(|l| l == LABEL_PURGED) && !purged.labels.iter().any(|l| l == LABEL_EVIDENCE));
    assert!(f.project.record(filed.record).unwrap().body.contains("users cannot sign in"), "not 90 days yet");
    assert_eq!(f.filing.sweep(now).await.unwrap().purged, 0, "purged once");
}
