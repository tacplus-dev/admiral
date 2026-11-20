// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The deterministic core of triage (#4): the only part of the Admiral's triage that may hold a client, and it
//! works with no model at all. It renders a report for the advisor, picks the model (Opus by default, Fable for hard
//! triage), takes back only a typed proposal, validates it again, adds the label validation sets, and groups
//! unreproduced reports by symptom across tenants, escalating several of one symptom as a bug of their own. It files
//! nothing: the filing core (#5) files what triage proposes, after its own checks (tacplus-dev/roadmap
//! decisions/skipper-and-admiral-2026-11-01.md, 2026-11-19 addenda).

pub mod llmproxy;
pub mod proposal;
pub mod report;
pub mod symptoms;

use std::sync::{Arc, Mutex};

use admiral_model::types::services::admiral as api;
use admiral_model::types::services::admiral::{Report, ReproductionStatus};
use rand::RngCore;

use crate::advisor::model::Model;
use crate::advisor::{Advisor, AdvisorError, Material, Subject, DEFAULT_MODEL_ALIAS, HARD_TRIAGE_MODEL_ALIAS};
use crate::core::proposal::{Cause, IssueProposal, Label, Withheld, PRIVATE_TRIAGE};
use crate::core::symptoms::{Grouped, SymptomGroups, SymptomKey};

/// A proposal below this confidence makes the triage hard: Fable is asked too, and its proposal taken if it gives
/// one.
pub const HARD_BELOW_CONFIDENCE: u8 = 50;

/// Who proposed an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedBy {
    /// The advisor, asking this llmproxy alias.
    Model(String),
    /// The core alone: no model is configured, or none gave a valid proposal (why).
    Core(String),
}

/// The tenant a report came from, as intake names it from its Skipper's verified token.
#[derive(Debug, Clone, Copy)]
pub struct Reporter<'a> {
    /// The key intake holds the tenant's reports under (its namespace).
    pub tenant: &'a str,
    /// The tenant's identifiers, which no proposal may carry.
    pub tenant_names: &'a [String],
}

/// A proposed issue, with who proposed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub issue: IssueProposal,
    pub by: ProposedBy,
}

/// Several unreproduced reports of one symptom, escalated as a bug of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    pub group: Grouped,
    pub proposed: Proposed,
}

/// One report's triage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triaged {
    /// The report's issue: every report gets one (every report surfaces), labelled `bug` when it was reproduced
    /// and `not yet reproduced` when not.
    pub proposed: Proposed,
    /// The report's symptom group, when it was not reproduced.
    pub grouped: Option<Grouped>,
    /// The group's own bug, when this report made it escalate.
    pub escalation: Option<Escalation>,
}

impl Triaged {
    /// The finding the Skipper is told (ReportUpdate.finding, at STAGE_TRIAGED).
    pub fn finding(&self) -> api::Finding {
        let issue = &self.proposed.issue;
        api::Finding {
            classification: Some(api::Classification {
                cause: wire_cause(issue.cause) as i32,
                component: issue.target_repository.clone(),
                confidence: u32::from(issue.confidence_percent),
                rationale: issue.rationale.clone(),
                ..Default::default()
            }),
            summary: issue.title.clone(),
            ..Default::default()
        }
    }
}

fn wire_cause(cause: Cause) -> api::Cause {
    match cause {
        Cause::TenantCode => api::Cause::TenantCode,
        Cause::PlatformService => api::Cause::PlatformService,
        Cause::Generator => api::Cause::Generator,
        Cause::Unknown => api::Cause::Unknown,
    }
}

/// A nonce for one triage's provenance blocks: 128 random bits, never shown to anyone but the model.
fn nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Triage: the advisor, if a model is configured, and the symptom groups.
pub struct Triage {
    advisor: Option<Advisor>,
    groups: Mutex<SymptomGroups>,
}

impl Triage {
    /// Triage with no model: every report still gets a proposal, the core's own, and is grouped.
    pub fn without_model() -> Self {
        Triage { advisor: None, groups: Mutex::default() }
    }

    /// Triage whose advisor asks `model`: Opus, and Fable for hard triage.
    pub fn with_model(model: Arc<dyn Model>) -> Self {
        Triage { advisor: Some(Advisor::new(model)), groups: Mutex::default() }
    }

    pub fn has_model(&self) -> bool {
        self.advisor.is_some()
    }

    /// Triages one report (its highest revision, as intake holds it, screened). `trace_id` is the trace of the
    /// message that brought it: every model call this triage makes carries it.
    pub async fn triage(&self, trace_id: &str, reporter: Reporter<'_>, report: &Report) -> Triaged {
        let withheld = Withheld { tenant_names: reporter.tenant_names.to_vec() };
        let reproduced = report.reproduction.as_ref().is_some_and(|r| r.status() == ReproductionStatus::Reproduced);
        let key = SymptomKey::of(&report.symptom.clone().unwrap_or_default(), reporter.tenant_names);

        let text = report::render(report);
        let mut proposed = self.propose(trace_id, Subject::Report, &text, &withheld, false).await;
        if matches!(proposed.by, ProposedBy::Core(_)) {
            proposed.issue = fallback(&key, Some(report), &withheld, &proposed.by);
        }
        proposed.issue.labels.push(if reproduced { Label::Bug } else { Label::NotYetReproduced });

        let grouped = {
            let mut groups = self.groups.lock().unwrap();
            if reproduced {
                groups.remove(reporter.tenant, &report.report_id);
                None
            } else {
                Some(groups.record(reporter.tenant, &report.report_id, key.clone()))
            }
        };
        let escalation = match &grouped {
            Some(group) if group.escalate => Some(self.escalate(trace_id, group, &withheld).await),
            _ => None,
        };
        Triaged { proposed, grouped, escalation }
    }

    /// Takes a report out of its symptom group (it closed).
    pub fn forget(&self, tenant: &str, report_id: &str) {
        self.groups.lock().unwrap().remove(tenant, report_id);
    }

    /// A group's own bug: its symptom only, triaged as hard. The trace is that of the report that made the group
    /// escalate: the escalation is that report's doing.
    async fn escalate(&self, trace_id: &str, group: &Grouped, withheld: &Withheld) -> Escalation {
        let text = report::render_group(group);
        let mut proposed = self.propose(trace_id, Subject::SymptomGroup, &text, withheld, true).await;
        if matches!(proposed.by, ProposedBy::Core(_)) {
            proposed.issue = fallback(&group.key, None, withheld, &proposed.by);
        }
        proposed.issue.labels.extend([Label::Bug, Label::SymptomGroup]);
        Escalation { group: group.clone(), proposed }
    }

    /// The advisor's proposal: Opus first unless `hard`; Fable when the triage is hard, Opus gave none or gave one
    /// below HARD_BELOW_CONFIDENCE. A proposal the core's own validation refuses counts as none. With no valid
    /// proposal from any model, the result is `ProposedBy::Core` and the caller writes the core's own.
    async fn propose(&self, trace_id: &str, subject: Subject, text: &str, withheld: &Withheld, hard: bool) -> Proposed {
        let Some(advisor) = &self.advisor else {
            return none("no model is configured".to_string());
        };
        let nonce = nonce();
        let material = Material { subject, nonce: &nonce, text };
        let ask = |alias: &'static str| async move {
            let got = advisor.propose(trace_id, alias, material, withheld).await.and_then(|issue| {
                // The advisor checked it already; the core does not take the advisor's word for it.
                issue.validate(withheld).map(|()| issue).map_err(|rejection| {
                    tracing::warn!(trace_id, %rejection, "the core refused the advisor's proposal");
                    AdvisorError::NoProposal
                })
            });
            if let Err(e) = &got {
                tracing::warn!(trace_id, model_alias = alias, error = %e, "triage: no proposal");
            }
            got.map(|issue| Proposed { issue, by: ProposedBy::Model(alias.to_string()) })
        };
        let first = if hard { None } else { Some(ask(DEFAULT_MODEL_ALIAS).await) };
        let easy = match &first {
            Some(Ok(p)) => p.issue.confidence_percent >= HARD_BELOW_CONFIDENCE,
            _ => false,
        };
        if easy {
            return first.and_then(Result::ok).expect("an easy triage has its proposal");
        }
        match ask(HARD_TRIAGE_MODEL_ALIAS).await {
            Ok(p) => p,
            Err(hard_error) => match first {
                Some(Ok(p)) => p,
                Some(Err(e)) => none(format!("{DEFAULT_MODEL_ALIAS}: {e}; {HARD_TRIAGE_MODEL_ALIAS}: {hard_error}")),
                None => none(format!("{HARD_TRIAGE_MODEL_ALIAS}: {hard_error}")),
            },
        }
    }
}

fn none(why: String) -> Proposed {
    Proposed { issue: empty(), by: ProposedBy::Core(why) }
}

fn empty() -> IssueProposal {
    IssueProposal {
        cause: Cause::Unknown,
        confidence_percent: 0,
        rationale: String::new(),
        target_repository: PRIVATE_TRIAGE.to_string(),
        title: String::new(),
        body: String::new(),
        labels: Vec::new(),
    }
}

/// The core's own proposal, with no model: cause unknown, to the private triage project, its text from the
/// symptom's shape (which holds no identifier) and the reproduction's counts. If even that fails the proposal's
/// checks (a service name shaped like a secret, say), the text says only that the report needs triage.
fn fallback(key: &SymptomKey, report: Option<&Report>, withheld: &Withheld, by: &ProposedBy) -> IssueProposal {
    let why = match by {
        ProposedBy::Core(why) => why.as_str(),
        ProposedBy::Model(_) => "",
    };
    let service = if key.service.is_empty() { "an unnamed service" } else { key.service.as_str() };
    let code = match (key.grpc_code, key.http_status) {
        (0, 0) => "a wrong answer".to_string(),
        (0, http) => format!("HTTP {http}"),
        (grpc, 0) => format!("gRPC code {grpc}"),
        (grpc, http) => format!("gRPC code {grpc}, HTTP {http}"),
    };
    let method = if key.method.is_empty() { String::new() } else { format!(" {}", key.method) };
    let mut body = vec![
        format!("Service: {service}{method}"),
        format!("Status: {code}"),
        format!("Error, its shape: {}", if key.error.is_empty() { "none given" } else { key.error.as_str() }),
    ];
    match report.and_then(|r| r.reproduction.as_ref()) {
        Some(r) => body.push(format!(
            "Reproduction: {}, {} runs, {} failed as reported",
            report::status_name(r.status()),
            r.runs,
            r.failed
        )),
        None if report.is_none() => body.push("Several reports of this symptom, none reproduced yet.".to_string()),
        None => {}
    }
    body.push(format!("Triaged by the Admiral's core without a model ({why}): it needs a person's triage."));
    let issue = IssueProposal {
        rationale: format!("no model triaged this report: {why}"),
        title: format!("{service}{method}: {code}"),
        body: body.join("\n"),
        ..empty()
    };
    if issue.validate(withheld).is_ok() {
        return issue;
    }
    IssueProposal {
        rationale: "no model triaged this report".to_string(),
        title: "A report the Admiral could not triage".to_string(),
        body: "A report reached the Admiral that no model triaged, and whose symptom cannot be quoted here: it needs \
               a person's triage from its private record."
            .to_string(),
        ..empty()
    }
}

#[cfg(test)]
mod tests;
