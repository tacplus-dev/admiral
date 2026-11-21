// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use serde_json::json;

use super::GitHub;
use crate::filing::fake_api::{Api, Token, TOKEN};
use crate::filing::forge::{NewIssue, PublicIssue, PublicTracker};
use crate::filing::render::fingerprint_marker;

const GATEWAY: &str = "tacplus-dev/gateway";

// Every call to GitHub carries the trace of the work that caused it, as `X-Tacplus-Trace-Id` (looper#51; README).
#[tokio::test]
async fn every_call_to_github_carries_its_trace() {
    let marker = fingerprint_marker("0123abcd");
    let found = marker.clone();
    let api = Api::start(move |seen| {
        if seen.path.starts_with("/search/issues?") {
            let url = "https://github.com/tacplus-dev/gateway/issues/7";
            json!({"items": [{"number": 7, "html_url": url, "body": found}]})
        } else if seen.method == "POST" {
            json!({"number": 8, "html_url": "https://github.com/tacplus-dev/gateway/issues/8"})
        } else {
            json!({"number": 7, "state": "open"})
        }
    })
    .await;
    let github = GitHub::new(format!("{}/", api.base), Arc::new(Token));

    let issue = github.find_open("trace-find", GATEWAY, &marker).await.unwrap().expect("the open issue");
    assert_eq!(issue.number, 7);
    let new = NewIssue {
        repository: GATEWAY.to_string(),
        title: "gateway: UNAUTHENTICATED after rotation".to_string(),
        body: marker.clone(),
        labels: vec!["bug".to_string()],
    };
    let opened = github.open("trace-open", &new).await.unwrap();
    let url = "https://github.com/tacplus-dev/gateway/issues/8".to_string();
    assert_eq!(opened, PublicIssue { repository: GATEWAY.to_string(), number: 8, url });
    assert!(github.is_open("trace-is-open", GATEWAY, 7).await.unwrap());

    let seen = api.seen();
    let calls: Vec<(&str, &str, &str)> =
        seen.iter().map(|s| (s.method.as_str(), s.path.split('?').next().unwrap(), s.trace_id.as_str())).collect();
    assert_eq!(
        calls,
        [
            ("GET", "/search/issues", "trace-find"),
            ("POST", "/repos/tacplus-dev/gateway/issues", "trace-open"),
            ("GET", "/repos/tacplus-dev/gateway/issues/7", "trace-is-open"),
        ]
    );
    assert!(seen.iter().all(|s| s.authorization == format!("Bearer {TOKEN}")), "{seen:?}");
}
