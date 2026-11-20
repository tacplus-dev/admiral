// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use admiral_model::types::services::admiral::{Classification, Report, Reproduction, ReproductionStatus, Symptom};

use crate::advisor::model::{Block, Model, ModelError, ModelReply, ModelRequest, ToolUse};
use crate::advisor::{DEFAULT_MODEL_ALIAS, HARD_TRIAGE_MODEL_ALIAS, PROPOSE_ISSUE};
use crate::core::proposal::{Cause, IssueProposal, Label, Withheld, PRIVATE_TRIAGE};
use crate::core::symptoms::{ESCALATE_AT_REPORTS, ESCALATE_AT_TENANTS};
use crate::core::{ProposedBy, Reporter, Triage};

// llmproxy as a script per alias: each alias answers with its own replies in order (then fails), and every call is
// recorded with its alias, trace and first user turn.
#[derive(Default)]
struct Aliases {
    replies: Mutex<HashMap<&'static str, Vec<Result<ModelReply, ModelError>>>>,
    calls: Mutex<Vec<(String, String, String)>>,
}

impl Aliases {
    fn with(replies: Vec<(&'static str, Vec<Result<ModelReply, ModelError>>)>) -> Arc<Self> {
        let model = Aliases::default();
        for (alias, mut r) in replies {
            r.reverse();
            model.replies.lock().unwrap().insert(alias, r);
        }
        Arc::new(model)
    }

    fn aliases(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect()
    }
}

#[async_trait::async_trait]
impl Model for Aliases {
    async fn complete(&self, request: ModelRequest) -> Result<ModelReply, ModelError> {
        let turn = match request.messages.first().and_then(|m| m.content.first()) {
            Some(Block::Text(t)) => t.clone(),
            _ => String::new(),
        };
        self.calls.lock().unwrap().push((request.model_alias.clone(), request.trace_id.clone(), turn));
        let mut replies = self.replies.lock().unwrap();
        let next = replies.get_mut(request.model_alias.as_str()).and_then(|r| r.pop());
        next.unwrap_or_else(|| Err(ModelError(format!("{} is not routed", request.model_alias))))
    }
}

fn propose(cause: &str, target: &str, confidence: u8) -> Result<ModelReply, ModelError> {
    let input = serde_json::json!({
        "cause": cause, "confidence_percent": confidence, "rationale": "the trace fails in the gateway",
        "target_repository": target, "title": "gateway: UNAUTHENTICATED after rotation",
        "body": "gateway.Gateway/Route answers UNAUTHENTICATED (16) for a token minted after rotation.",
        "labels": ["wrong-answer"]
    });
    Ok(ModelReply::ToolUse(ToolUse {
        id: "c".to_string(),
        name: PROPOSE_ISSUE.to_string(),
        input_json: input.to_string(),
    }))
}

fn report(id: &str, status: ReproductionStatus, tenant: &str) -> Report {
    Report {
        report_id: id.to_string(),
        revision: 1,
        summary: format!("sign-in of {tenant}'s users fails after rotation"),
        symptom: Some(Symptom {
            service: "gateway".to_string(),
            method: "gateway.Gateway/Route".to_string(),
            grpc_code: 16,
            error: format!("token for /tenants/{tenant}/services/web expired at 17310000{}", id.len()),
            ..Default::default()
        }),
        reproduction: Some(Reproduction { status: status as i32, runs: 3, ..Default::default() }),
        classification: Some(Classification { cause: 2, confidence: 40, ..Default::default() }),
        ..Default::default()
    }
}

struct Tenant {
    tenant: String,
    names: Vec<String>,
}

impl Tenant {
    fn new(name: &str) -> Self {
        Tenant { tenant: format!("/tenants/{name}"), names: vec![name.to_string()] }
    }

    fn reporter(&self) -> Reporter<'_> {
        Reporter { tenant: &self.tenant, tenant_names: &self.names }
    }
}

#[tokio::test]
async fn with_no_model_every_report_still_gets_a_proposal_and_a_group() {
    let triage = Triage::without_model();
    assert!(!triage.has_model());
    let acme = Tenant::new("acme");
    let got =
        triage.triage("trace-1", acme.reporter(), &report("r-1", ReproductionStatus::NotYetReproduced, "acme")).await;
    assert_eq!(got.proposed.by, ProposedBy::Core("no model is configured".to_string()));
    let issue = &got.proposed.issue;
    assert_eq!((issue.cause, issue.target_repository.as_str()), (Cause::Unknown, PRIVATE_TRIAGE));
    assert_eq!(issue.title, "gateway gateway.gateway/route: gRPC code 16");
    assert!(issue.body.contains("Reproduction: not yet reproduced, 3 runs, 0 failed as reported"), "{}", issue.body);
    assert!(!issue.body.contains("acme"), "{}", issue.body);
    assert_eq!(issue.labels, vec![Label::NotYetReproduced]);
    // The core's own proposal passes the checks the advisor's does; only the label it added is the core's to add.
    let unlabelled = IssueProposal { labels: vec![], ..issue.clone() };
    assert_eq!(unlabelled.validate(&Withheld { tenant_names: acme.names.clone() }), Ok(()));
    assert_eq!(got.grouped.as_ref().map(|g| g.reports), Some(1));
    assert!(got.escalation.is_none());
}

#[tokio::test]
async fn a_confident_opus_proposal_is_taken_and_fable_is_not_asked() {
    let model =
        Aliases::with(vec![(DEFAULT_MODEL_ALIAS, vec![propose("platform_service", "tacplus-dev/gateway", 85)])]);
    let triage = Triage::with_model(model.clone());
    let acme = Tenant::new("acme");
    let got = triage.triage("trace-1", acme.reporter(), &report("r-1", ReproductionStatus::Reproduced, "acme")).await;
    assert_eq!(got.proposed.by, ProposedBy::Model(DEFAULT_MODEL_ALIAS.to_string()));
    assert_eq!(got.proposed.issue.target_repository, "tacplus-dev/gateway");
    assert_eq!(got.proposed.issue.labels, vec![Label::WrongAnswer, Label::Bug]);
    // A reproduced report is a bug of its own: it joins no symptom group.
    assert!(got.grouped.is_none());
    assert_eq!(model.aliases(), vec![DEFAULT_MODEL_ALIAS]);

    let finding = got.finding();
    let classification = finding.classification.expect("classified");
    assert_eq!(classification.cause, 2);
    assert_eq!(classification.component, "tacplus-dev/gateway");
    assert_eq!(classification.confidence, 85);
    assert_eq!(finding.summary, "gateway: UNAUTHENTICATED after rotation");
}

#[tokio::test]
async fn a_hard_triage_asks_fable() {
    let model = Aliases::with(vec![
        (DEFAULT_MODEL_ALIAS, vec![propose("unknown", PRIVATE_TRIAGE, 20)]),
        (HARD_TRIAGE_MODEL_ALIAS, vec![propose("generator", "tacplus-dev/tacplus", 70)]),
    ]);
    let triage = Triage::with_model(model.clone());
    let acme = Tenant::new("acme");
    let got = triage.triage("trace-1", acme.reporter(), &report("r-1", ReproductionStatus::Reproduced, "acme")).await;
    assert_eq!(got.proposed.by, ProposedBy::Model(HARD_TRIAGE_MODEL_ALIAS.to_string()));
    assert_eq!(got.proposed.issue.cause, Cause::Generator);
    assert_eq!(model.aliases(), vec![DEFAULT_MODEL_ALIAS, HARD_TRIAGE_MODEL_ALIAS]);
}

#[tokio::test]
async fn opus_is_kept_when_fable_fails_and_the_core_answers_when_both_do() {
    let model = Aliases::with(vec![(DEFAULT_MODEL_ALIAS, vec![propose("unknown", PRIVATE_TRIAGE, 20)])]);
    let triage = Triage::with_model(model.clone());
    let acme = Tenant::new("acme");
    let got = triage.triage("trace-1", acme.reporter(), &report("r-1", ReproductionStatus::Reproduced, "acme")).await;
    assert_eq!(got.proposed.by, ProposedBy::Model(DEFAULT_MODEL_ALIAS.to_string()));

    let triage = Triage::with_model(Aliases::with(vec![]));
    let got = triage.triage("trace-1", acme.reporter(), &report("r-1", ReproductionStatus::Reproduced, "acme")).await;
    let ProposedBy::Core(why) = &got.proposed.by else { panic!("the core's proposal") };
    assert!(why.contains(DEFAULT_MODEL_ALIAS) && why.contains(HARD_TRIAGE_MODEL_ALIAS), "{why}");
    assert_eq!(got.proposed.issue.target_repository, PRIVATE_TRIAGE);
    assert_eq!(got.proposed.issue.labels, vec![Label::Bug]);
}

// Several unreproduced reports of one symptom from different tenants escalate once, as a bug of their own, triaged
// as hard from the symptom only: no tenant's name reaches the model, and the escalation carries the trace of the
// report that made the group escalate (tacplus-dev/looper#51).
#[tokio::test]
async fn several_unreproduced_reports_of_one_symptom_escalate_as_a_bug() {
    assert_eq!((ESCALATE_AT_REPORTS, ESCALATE_AT_TENANTS), (3, 2));
    let model = Aliases::with(vec![
        (DEFAULT_MODEL_ALIAS, (0..3).map(|_| propose("unknown", PRIVATE_TRIAGE, 60)).collect()),
        (HARD_TRIAGE_MODEL_ALIAS, vec![propose("platform_service", "tacplus-dev/gateway", 75)]),
    ]);
    let triage = Triage::with_model(model.clone());
    let (acme, globex) = (Tenant::new("acme"), Tenant::new("globex"));
    let nyr = ReproductionStatus::NotYetReproduced;
    let first = triage.triage("trace-a1", acme.reporter(), &report("r-1", nyr, "acme")).await;
    let second = triage.triage("trace-a2", acme.reporter(), &report("r-22", nyr, "acme")).await;
    assert!(first.escalation.is_none() && second.escalation.is_none());
    let third = triage.triage("trace-g1", globex.reporter(), &report("r-333", nyr, "globex")).await;
    let grouped = third.grouped.as_ref().expect("grouped");
    assert_eq!((grouped.reports, grouped.tenants), (3, 2));
    let escalation = third.escalation.expect("escalated");
    assert_eq!(escalation.proposed.by, ProposedBy::Model(HARD_TRIAGE_MODEL_ALIAS.to_string()));
    assert_eq!(escalation.proposed.issue.labels, vec![Label::WrongAnswer, Label::Bug, Label::SymptomGroup]);
    assert_eq!(third.proposed.issue.labels, vec![Label::WrongAnswer, Label::NotYetReproduced]);

    let calls = model.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    let (alias, trace, turn) = &calls[3];
    assert_eq!((alias.as_str(), trace.as_str()), (HARD_TRIAGE_MODEL_ALIAS, "trace-g1"));
    assert!(turn.starts_with("<<symptom-group nonce="), "{turn}");
    assert!(turn.contains("3 reports from 2 tenants"), "{turn}");
    assert!(!turn.contains("acme") && !turn.contains("globex"), "{turn}");
    let traces: Vec<&str> = calls.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(traces, vec!["trace-a1", "trace-a2", "trace-g1", "trace-g1"]);

    // Escalated once: a fourth report joins the group and escalates nothing more.
    let initech = Tenant::new("initech");
    let fourth = triage.triage("trace-i1", initech.reporter(), &report("r-4444", nyr, "initech")).await;
    assert_eq!(fourth.grouped.map(|g| g.reports), Some(4));
    assert!(fourth.escalation.is_none());
}

// Each triage draws its own nonce, so one report's text cannot close another's block.
#[tokio::test]
async fn each_triage_draws_its_own_nonce() {
    let model = Aliases::with(vec![(
        DEFAULT_MODEL_ALIAS,
        vec![propose("unknown", PRIVATE_TRIAGE, 60), propose("unknown", PRIVATE_TRIAGE, 60)],
    )]);
    let triage = Triage::with_model(model.clone());
    let acme = Tenant::new("acme");
    let reproduced = ReproductionStatus::Reproduced;
    triage.triage("t", acme.reporter(), &report("r-1", reproduced, "acme")).await;
    triage.triage("t", acme.reporter(), &report("r-2", reproduced, "acme")).await;
    let calls = model.calls.lock().unwrap().clone();
    let nonce = |turn: &str| turn["<<report nonce=".len()..].split(' ').next().unwrap_or("").to_string();
    let (a, b) = (nonce(&calls[0].2), nonce(&calls[1].2));
    assert_eq!(a.len(), 32);
    assert_ne!(a, b);
}
