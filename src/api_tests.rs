// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Skipper-Admiral API (#3) as both sides generate it from proto/services/admiral.proto: the envelopes
//! round-trip on the wire, the generated validation (which the generated wrapper runs on every message of the
//! stream, both ways) enforces the declared bounds, and only a Skipper may open the stream.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use admiral_model::s2sm::internal::grpc::servers::services::admiral::admiral::register_method_auth;
use admiral_model::types::services::admiral::{
    admiral_message, admiral_request, request_answer, skipper_message, AdmiralHello, AdmiralMessage, AdmiralRequest,
    Cause, Classification, ComponentVersion, Declined, Evidence, Exchange, Finding, LogLine, RedactedConfig, Report,
    ReportAccepted, ReportCursor, ReportUpdate, ReproduceRequest, Reproduction, ReproductionStatus, RequestAnswer,
    SkipperHello, SkipperMessage, Stage, Symptom, TenantConfigRequest,
};
use base64::Engine as _;
use key::keys;
use prost::Message;

fn report() -> Report {
    Report {
        report_id: "r-1".to_string(),
        revision: 1,
        reported_at_ms: 1_763_600_000_000,
        summary: "GetOrder answers NOT_FOUND for an order just created".to_string(),
        symptom: Some(Symptom {
            service: "orders".to_string(),
            method: "orders.Orders/GetOrder".to_string(),
            grpc_code: 5,
            http_status: 404,
            error: "order <redacted> not found".to_string(),
            ..Default::default()
        }),
        reproduction: Some(Reproduction {
            status: ReproductionStatus::Reproduced as i32,
            steps: vec![Exchange {
                method: "orders.Orders/GetOrder".to_string(),
                request_json: r#"{"id":"<redacted>"}"#.to_string(),
                response_json: "{}".to_string(),
                grpc_code: 5,
                call_trace_id: "t-1".to_string(),
                ..Default::default()
            }],
            runs: 3,
            failed: 3,
            attempts: "created an order, read it back at once".to_string(),
            ..Default::default()
        }),
        evidence: Some(Evidence {
            logs: vec![LogLine {
                service: "orders".to_string(),
                timestamp_ms: 1_763_600_000_001,
                level: "ERROR".to_string(),
                text: "read before the write was visible".to_string(),
                ..Default::default()
            }],
            versions: vec![ComponentVersion {
                component: "tacplus".to_string(),
                version: "2.6.13".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        classification: Some(Classification {
            cause: Cause::TenantCode as i32,
            component: "orders".to_string(),
            confidence: 70,
            rationale: "the read path skips the service's own cache".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn sent(report: Report) -> SkipperMessage {
    SkipperMessage { body: Some(skipper_message::Body::Report(report)), ..Default::default() }
}

fn update(seq: u64) -> ReportUpdate {
    ReportUpdate {
        report_id: "r-1".to_string(),
        seq,
        stage: Stage::Filed as i32,
        at_ms: 1_763_600_100_000,
        finding: Some(Finding { classification: report().classification, summary: "a cache bug".to_string(), ..Default::default() }),
        issue_url: "https://github.com/tacplus-dev/tacplus/issues/1".to_string(),
        ..Default::default()
    }
}

fn round_trips<M: Message + Default + PartialEq + std::fmt::Debug>(msg: &M) {
    let back = M::decode(msg.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(&back, msg);
}

#[test]
fn what_a_skipper_sends_round_trips_and_validates() {
    let hello = SkipperMessage {
        body: Some(skipper_message::Body::Hello(SkipperHello {
            skipper_version: "0.3.1".to_string(),
            open_reports: vec![ReportCursor { report_id: "r-1".to_string(), last_seq: 2, ..Default::default() }],
            ..Default::default()
        })),
        ..Default::default()
    };
    let answers = [
        request_answer::Outcome::Declined(Declined { reason: "policy".to_string(), ..Default::default() }),
        request_answer::Outcome::TenantConfig(RedactedConfig {
            message_type: "tenant.Config".to_string(),
            version: 7,
            value: vec![1, 2, 3],
            ..Default::default()
        }),
        request_answer::Outcome::ReproducedRevision(2),
    ];
    let mut msgs = vec![hello, sent(report()), SkipperMessage { last: true, ..Default::default() }];
    for outcome in answers {
        msgs.push(SkipperMessage {
            body: Some(skipper_message::Body::Answer(RequestAnswer {
                request_id: "q-1".to_string(),
                outcome: Some(outcome),
                ..Default::default()
            })),
            ..Default::default()
        });
    }
    for msg in &msgs {
        round_trips(msg);
        msg.validate().unwrap_or_else(|e| panic!("{msg:?} must validate: {e}"));
    }
}

#[test]
fn what_the_admiral_sends_round_trips_and_validates() {
    let request = |kind| AdmiralMessage {
        body: Some(admiral_message::Body::Request(AdmiralRequest {
            request_id: "q-1".to_string(),
            kind: Some(kind),
            ..Default::default()
        })),
        ..Default::default()
    };
    let msgs = [
        AdmiralMessage {
            body: Some(admiral_message::Body::Hello(AdmiralHello {
                admiral_version: "0.1.0".to_string(),
                unknown_reports: vec!["r-9".to_string()],
                ..Default::default()
            })),
            ..Default::default()
        },
        AdmiralMessage {
            body: Some(admiral_message::Body::Accepted(ReportAccepted {
                report_id: "r-1".to_string(),
                revision: 1,
                ..Default::default()
            })),
            ..Default::default()
        },
        AdmiralMessage { body: Some(admiral_message::Body::Update(update(1))), ..Default::default() },
        request(admiral_request::Kind::TenantConfig(TenantConfigRequest { report_id: "r-1".to_string(), ..Default::default() })),
        request(admiral_request::Kind::Reproduce(ReproduceRequest { report_id: "r-1".to_string(), ..Default::default() })),
        AdmiralMessage { last: true, ..Default::default() },
    ];
    for msg in &msgs {
        round_trips(msg);
        msg.validate().unwrap_or_else(|e| panic!("{msg:?} must validate: {e}"));
    }
}

// The bounds, checked on the envelope, as the generated wrapper checks each message of the stream: a Skipper's
// message is untrusted, and one over a bound never reaches the Admiral's handler.
#[test]
fn a_report_out_of_its_bounds_is_refused() {
    let long = |n: usize| "x".repeat(n);
    let cases: Vec<(&str, Box<dyn Fn(&mut Report)>)> = vec![
        ("no report_id", Box::new(|r| r.report_id.clear())),
        ("report_id over 64 bytes", Box::new(move |r| r.report_id = long(65))),
        ("revision 0", Box::new(|r| r.revision = 0)),
        ("no summary", Box::new(|r| r.summary.clear())),
        ("summary over 4096 bytes", Box::new(move |r| r.summary = long(4097))),
        ("no symptom", Box::new(|r| r.symptom = None)),
        ("no reproduction", Box::new(|r| r.reproduction = None)),
        ("no classification", Box::new(|r| r.classification = None)),
        ("grpc_code past 16", Box::new(|r| r.symptom.as_mut().unwrap().grpc_code = 17)),
        ("http_status past 599", Box::new(|r| r.symptom.as_mut().unwrap().http_status = 600)),
        ("error over 1024 bytes", Box::new(move |r| r.symptom.as_mut().unwrap().error = long(1025))),
        ("reproduction status unset", Box::new(|r| r.reproduction.as_mut().unwrap().status = 0)),
        ("more than 64 steps", Box::new(|r| {
            let step = r.reproduction.as_ref().unwrap().steps[0].clone();
            r.reproduction.as_mut().unwrap().steps = vec![step; 65];
        })),
        ("a step's request over 64 KiB", Box::new(move |r| r.reproduction.as_mut().unwrap().steps[0].request_json = long(65537))),
        ("attempts over 8192 bytes", Box::new(move |r| r.reproduction.as_mut().unwrap().attempts = long(8193))),
        ("a log line over 4096 bytes", Box::new(move |r| r.evidence.as_mut().unwrap().logs[0].text = long(4097))),
        ("a version naming no component", Box::new(|r| r.evidence.as_mut().unwrap().versions[0].component.clear())),
        ("cause unset", Box::new(|r| r.classification.as_mut().unwrap().cause = 0)),
        ("confidence past 100", Box::new(|r| r.classification.as_mut().unwrap().confidence = 101)),
    ];
    for (what, break_it) in cases {
        let mut r = report();
        break_it(&mut r);
        assert!(sent(r).validate().is_err(), "a report with {what} must be refused");
    }
}

#[test]
fn a_hello_or_an_answer_out_of_its_bounds_is_refused() {
    let hello = |open_reports| SkipperMessage {
        body: Some(skipper_message::Body::Hello(SkipperHello { open_reports, ..Default::default() })),
        ..Default::default()
    };
    let cursor = ReportCursor { report_id: "r-1".to_string(), ..Default::default() };
    assert!(hello(vec![cursor.clone(); 4096]).validate().is_ok());
    assert!(hello(vec![cursor; 4097]).validate().is_err(), "more than 4096 open reports");
    assert!(hello(vec![ReportCursor::default()]).validate().is_err(), "a cursor naming no report");

    let answer = |outcome| SkipperMessage {
        body: Some(skipper_message::Body::Answer(RequestAnswer {
            request_id: "q-1".to_string(),
            outcome: Some(outcome),
            ..Default::default()
        })),
        ..Default::default()
    };
    let config = |value: Vec<u8>| RedactedConfig { message_type: "tenant.Config".to_string(), value, ..Default::default() };
    assert!(answer(request_answer::Outcome::TenantConfig(config(vec![0; 1 << 20]))).validate().is_ok());
    assert!(
        answer(request_answer::Outcome::TenantConfig(config(vec![0; (1 << 20) + 1]))).validate().is_err(),
        "a config over 1 MiB"
    );
    let unanswered = SkipperMessage {
        body: Some(skipper_message::Body::Answer(RequestAnswer::default())),
        ..Default::default()
    };
    assert!(unanswered.validate().is_err(), "an answer naming no request");
}

#[test]
fn what_the_admiral_sends_out_of_its_bounds_is_refused() {
    let sent = |u| AdmiralMessage { body: Some(admiral_message::Body::Update(u)), ..Default::default() };
    assert!(sent(update(0)).validate().is_err(), "seq 0");
    assert!(sent(ReportUpdate { stage: 0, ..update(1) }).validate().is_err(), "stage unset");
    assert!(sent(ReportUpdate { issue_url: "x".repeat(513), ..update(1) }).validate().is_err(), "issue_url over 512 bytes");
    let unknown = AdmiralMessage {
        body: Some(admiral_message::Body::Hello(AdmiralHello { unknown_reports: vec!["x".repeat(65)], ..Default::default() })),
        ..Default::default()
    };
    assert!(unknown.validate().is_err(), "an unknown report id over 64 bytes");
    let unaccepted = AdmiralMessage {
        body: Some(admiral_message::Body::Accepted(ReportAccepted { report_id: "r-1".to_string(), ..Default::default() })),
        ..Default::default()
    };
    assert!(unaccepted.validate().is_err(), "an acceptance of revision 0");
}

// ---------------------------------------------------------------------------
// Who may open the stream: the generated Authorizer with the auth_services the generated wrapper registers, as
// the ServiceManager wires them. A Skipper, of any tenant, is admitted; any other service or a user is refused.
// ---------------------------------------------------------------------------

const CHANNEL: &str = "/admiral.Admiral/BrokerSkipperChannel";

fn verifying_jwk(key: &keys::ImmutableKey) -> jwk::ImmutableJwk {
    let x = match key.public_key() {
        keys::PublicKey::Ed25519(ed) => base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ed.as_bytes()),
        _ => panic!("expected an ed25519 public key"),
    };
    jwk::ImmutableJwk::new(jwk::jwk::Jwk {
        kty: "OKP".to_string(),
        alg: Some("EdDSA".to_string()),
        kid: key.id().to_string(),
        key_use: Some("sig".to_string()),
        n: None,
        e: None,
        crv: Some("Ed25519".to_string()),
        x: Some(x),
        y: None,
        exp: None,
        created: None,
    })
    .expect("build jwk")
}

/// A signed token for `subject` in `namespace`, audience admiral.
fn bearer(key: &keys::ImmutableKey, subject: &str, namespace: &str, sub_type: &str) -> String {
    let exp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 + 3600;
    let mut tok = token::token::Token::new();
    tok.set_issuer("test-issuer").unwrap();
    tok.set_audience("admiral").unwrap();
    tok.set_expiration(exp).unwrap();
    tok.set_namespace(namespace).unwrap();
    tok.set_sub_type(sub_type).unwrap();
    tok.set_subject(subject).unwrap();
    tok.sign(key).expect("sign");
    format!("Bearer {}", tok.base64().expect("signed token has base64"))
}

#[tokio::test]
async fn only_a_skipper_may_open_the_channel() {
    let key = keys::generate_ed25519_key().expect("generate ed25519 key");
    let auth = grpcserver::authorizer::Authorizer::with_seeded_jwks(vec![verifying_jwk(&key)]);
    let own = bearer(&key, "spiffe://example.com/system/services/admiral", "/system", "service");
    auth.set_host_namespace_from_token(own.trim_start_matches("Bearer ")).expect("own identity token");
    register_method_auth(&auth);
    let open = |subject: &str, namespace: &str, sub_type: &str| {
        let authorization = bearer(&key, subject, namespace, sub_type);
        let auth = &auth;
        async move {
            auth.authorize_grpc(CHANNEL, "BrokerSkipperChannel", Some(&authorization), &HashMap::new()).await.is_ok()
        }
    };

    for (subject, namespace) in [
        ("spiffe://example.com/tenants/aaaaaaaa/services/skipper", "/tenants/acme"),
        ("spiffe://example.com/tenants/bbbbbbbb/services/skipper", "/tenants/beta"),
    ] {
        assert!(open(subject, namespace, "service").await, "{subject} must be admitted");
    }
    for (subject, namespace, sub_type) in [
        ("spiffe://example.com/tenants/aaaaaaaa/services/orders", "/tenants/acme", "service"),
        ("spiffe://example.com/system/services/kernel", "/system", "service"),
        ("spiffe://example.com/tenants/acme/users/alice", "/tenants/acme", "user"),
    ] {
        assert!(!open(subject, namespace, sub_type).await, "{subject} must be refused");
    }
}
