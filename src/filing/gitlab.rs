// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The private triage project on GitLab's REST API (`seabeam/triage`): each record is a confidential issue of it.
//! Its credential is the core's alone.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::filing::forge::{Credential, ForgeError, NewRecord, Record, TriageProject};
use crate::filing::github::TRACE_HEADER;
use crate::filing::render::parse_time;

pub const API: &str = "https://gitlab.com/api/v4";

/// The most records one listing reads: 100 a page.
const MAX_PAGES: usize = 50;
const PER_PAGE: usize = 100;

pub struct GitLab {
    api: String,
    /// The project's path, URL-encoded (`seabeam%2Ftriage`).
    project: String,
    http: reqwest::Client,
    credential: Arc<dyn Credential>,
}

impl GitLab {
    pub fn new(api: impl Into<String>, project: &str, credential: Arc<dyn Credential>) -> Self {
        GitLab {
            api: api.into().trim_end_matches('/').to_string(),
            project: project.replace('/', "%2F"),
            http: reqwest::Client::new(),
            credential,
        }
    }

    fn issues(&self) -> String {
        format!("{}/projects/{}/issues", self.api, self.project)
    }

    async fn call(&self, trace_id: &str, request: reqwest::RequestBuilder) -> Result<Value, ForgeError> {
        let token = self.credential.bearer().await?;
        let response = request
            .bearer_auth(token)
            .header("User-Agent", "tacplus-admiral")
            .header(TRACE_HEADER, trace_id) // propagate
            .send()
            .await
            .map_err(|e| ForgeError(format!("gitlab: {}", e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ForgeError(format!("gitlab: {status}")));
        }
        response.json().await.map_err(|e| ForgeError(format!("gitlab: the answer is not JSON: {}", e.without_url())))
    }
}

fn record_of(value: &Value) -> Result<Record, ForgeError> {
    let id = value["iid"].as_u64().ok_or_else(|| ForgeError("gitlab: an issue with no iid".to_string()))?;
    Ok(Record {
        id,
        title: value["title"].as_str().unwrap_or_default().to_string(),
        body: value["description"].as_str().unwrap_or_default().to_string(),
        labels: value["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect(),
        open: value["state"].as_str() == Some("opened"),
        closed_at_ms: value["closed_at"].as_str().and_then(parse_time),
    })
}

#[async_trait::async_trait]
impl TriageProject for GitLab {
    async fn find(&self, trace_id: &str, marker: &str) -> Result<Option<Record>, ForgeError> {
        // The key is one hex word: the search finds it, and the body is checked for the whole marker.
        let key = marker.split_whitespace().nth(2).unwrap_or(marker);
        let query = [("search", key), ("in", "description"), ("scope", "all"), ("per_page", "20")];
        let found = self.call(trace_id, self.http.get(self.issues()).query(&query)).await?;
        for item in found.as_array().into_iter().flatten() {
            if item["description"].as_str().is_some_and(|d| d.contains(marker)) {
                return record_of(item).map(Some);
            }
        }
        Ok(None)
    }

    async fn create(&self, trace_id: &str, record: &NewRecord) -> Result<Record, ForgeError> {
        let body = json!({
            "title": record.title,
            "description": record.body,
            "labels": record.labels.join(","),
            "confidential": true,
        });
        let created = self.call(trace_id, self.http.post(self.issues()).json(&body)).await?;
        record_of(&created)
    }

    async fn update(&self, trace_id: &str, id: u64, body: &str, labels: &[String]) -> Result<(), ForgeError> {
        let edit = json!({"description": body, "labels": labels.join(",")});
        self.call(trace_id, self.http.put(format!("{}/{id}", self.issues())).json(&edit)).await.map(|_| ())
    }

    async fn close(&self, trace_id: &str, id: u64) -> Result<(), ForgeError> {
        let edit = json!({"state_event": "close"});
        self.call(trace_id, self.http.put(format!("{}/{id}", self.issues())).json(&edit)).await.map(|_| ())
    }

    async fn labelled(&self, trace_id: &str, labels: &[&str], open: bool) -> Result<Vec<Record>, ForgeError> {
        let state = if open { "opened" } else { "closed" };
        let labels = labels.join(",");
        let mut records = Vec::new();
        for page in 1..=MAX_PAGES {
            let (page, per_page) = (page.to_string(), PER_PAGE.to_string());
            let query =
                [("labels", labels.as_str()), ("state", state), ("per_page", per_page.as_str()), ("page", page.as_str())];
            let listed = self.call(trace_id, self.http.get(self.issues()).query(&query)).await?;
            let items = listed.as_array().cloned().unwrap_or_default();
            for item in &items {
                records.push(record_of(item)?);
            }
            if items.len() < PER_PAGE {
                break;
            }
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests;
