// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use serde_json::{json, Value};

use super::{GitLab, PER_PAGE};
use crate::filing::fake_api::{Api, Seen, Token, TOKEN};
use crate::filing::forge::{NewRecord, TriageProject};
use crate::filing::render::record_marker;

const PROJECT: &str = "/projects/seabeam%2Ftriage/issues";

fn issue(iid: u64, description: &str) -> Value {
    json!({"iid": iid, "title": "a record", "description": description, "labels": ["admiral"], "state": "opened"})
}

/// A request's query parameter.
fn param<'a>(seen: &'a Seen, name: &str) -> Option<&'a str> {
    let (_, query) = seen.path.split_once('?')?;
    query.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// The triage project's API: a search finds record 3, and the listing by label holds one full page then one more.
fn answer(marker: String) -> impl Fn(&Seen) -> Value + Send + Sync + 'static {
    move |seen| match (param(seen, "search"), param(seen, "page")) {
        (Some(_), _) => json!([issue(3, &marker)]),
        (None, Some("1")) => Value::Array((1..=PER_PAGE as u64).map(|iid| issue(100 + iid, "")).collect()),
        (None, Some(_)) => json!([issue(500, "")]),
        (None, None) => issue(3, &marker),
    }
}

// Every call to GitLab carries the trace of the work that caused it, as `X-Tacplus-Trace-Id` (looper#51; README):
// each page of a listing too.
#[tokio::test]
async fn every_call_to_gitlab_carries_its_trace() {
    let marker = record_marker("0123abcd");
    let api = Api::start(answer(marker.clone())).await;
    let gitlab = GitLab::new(format!("{}/", api.base), "seabeam/triage", Arc::new(Token));

    assert_eq!(gitlab.find("trace-find", &marker).await.unwrap().expect("the record").id, 3);
    let record = NewRecord { title: "a record".to_string(), body: marker.clone(), labels: vec!["admiral".to_string()] };
    assert_eq!(gitlab.create("trace-create", &record).await.unwrap().id, 3);
    gitlab.update("trace-update", 3, &marker, &["admiral".to_string()]).await.unwrap();
    gitlab.close("trace-close", 3).await.unwrap();
    let listed = gitlab.labelled("trace-labelled", &["admiral"], true).await.unwrap();
    assert_eq!(listed.len(), PER_PAGE + 1, "both pages");

    let seen = api.seen();
    let path = |s: &Seen| s.path.split('?').next().unwrap().to_string();
    let calls: Vec<(&str, String, &str)> =
        seen.iter().map(|s| (s.method.as_str(), path(s), s.trace_id.as_str())).collect();
    let one = format!("{PROJECT}/3");
    assert_eq!(
        calls,
        [
            ("GET", PROJECT.to_string(), "trace-find"),
            ("POST", PROJECT.to_string(), "trace-create"),
            ("PUT", one.clone(), "trace-update"),
            ("PUT", one, "trace-close"),
            ("GET", PROJECT.to_string(), "trace-labelled"),
            ("GET", PROJECT.to_string(), "trace-labelled"),
        ]
    );
    assert_eq!((param(&seen[4], "page"), param(&seen[5], "page")), (Some("1"), Some("2")), "{seen:?}");
    assert!(seen[3].body.contains("\"state_event\":\"close\""), "{}", seen[3].body);
    assert!(seen.iter().all(|s| s.authorization == format!("Bearer {TOKEN}")), "{seen:?}");
}
