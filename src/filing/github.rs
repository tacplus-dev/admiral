// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The public tracker on GitHub's REST API, as the `tacplus-admiral` GitHub App (issues: write on tacplus-dev only):
//! its credential is the App's installation token, which the core alone holds.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::filing::forge::{Credential, ForgeError, NewIssue, PublicIssue, PublicTracker};

pub const API: &str = "https://api.github.com";

/// The header every forge call carries its trace in (looper#51).
pub const TRACE_HEADER: &str = "X-Tacplus-Trace-Id";

pub struct GitHub {
    api: String,
    http: reqwest::Client,
    credential: Arc<dyn Credential>,
}

impl GitHub {
    pub fn new(api: impl Into<String>, credential: Arc<dyn Credential>) -> Self {
        GitHub { api: api.into().trim_end_matches('/').to_string(), http: reqwest::Client::new(), credential }
    }

    async fn call(&self, trace_id: &str, request: reqwest::RequestBuilder) -> Result<Value, ForgeError> {
        let token = self.credential.bearer().await?;
        let response = request
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tacplus-admiral")
            .header(TRACE_HEADER, trace_id) // propagate
            .send()
            .await
            .map_err(|e| ForgeError(format!("github: {}", e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ForgeError(format!("github: {status}")));
        }
        response.json().await.map_err(|e| ForgeError(format!("github: the answer is not JSON: {}", e.without_url())))
    }
}

fn issue_of(repository: &str, value: &Value) -> Result<PublicIssue, ForgeError> {
    let number = value["number"].as_u64().ok_or_else(|| ForgeError("github: an issue with no number".to_string()))?;
    let url = value["html_url"].as_str().unwrap_or_default().to_string();
    Ok(PublicIssue { repository: repository.to_string(), number, url })
}

#[async_trait::async_trait]
impl PublicTracker for GitHub {
    async fn find_open(
        &self,
        trace_id: &str,
        repository: &str,
        marker: &str,
    ) -> Result<Option<PublicIssue>, ForgeError> {
        // The fingerprint is hex: the search finds it as a word, and the body is checked for the whole marker.
        let fingerprint = marker.split_whitespace().nth(2).unwrap_or(marker);
        let q = format!("repo:{repository} is:issue is:open in:body \"{fingerprint}\"");
        let found = self.call(trace_id, self.http.get(format!("{}/search/issues", self.api)).query(&[("q", q)])).await?;
        for item in found["items"].as_array().into_iter().flatten() {
            if item["body"].as_str().is_some_and(|b| b.contains(marker)) {
                return issue_of(repository, item).map(Some);
            }
        }
        Ok(None)
    }

    async fn open(&self, trace_id: &str, issue: &NewIssue) -> Result<PublicIssue, ForgeError> {
        let body = json!({"title": issue.title, "body": issue.body, "labels": issue.labels});
        let url = format!("{}/repos/{}/issues", self.api, issue.repository);
        let created = self.call(trace_id, self.http.post(url).json(&body)).await?;
        issue_of(&issue.repository, &created)
    }

    async fn is_open(&self, trace_id: &str, repository: &str, number: u64) -> Result<bool, ForgeError> {
        let url = format!("{}/repos/{repository}/issues/{number}", self.api);
        let issue = self.call(trace_id, self.http.get(url)).await?;
        Ok(issue["state"].as_str() == Some("open"))
    }
}

#[cfg(test)]
mod tests;
