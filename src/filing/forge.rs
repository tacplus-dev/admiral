// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The two forges the filing core writes, behind traits: the private triage project (GitLab `seabeam/triage`), where
//! every report's full record goes first, and the public tracker (GitHub, tacplus-dev), where a platform cause's
//! sanitized issue goes. The core holds them; nothing on the advisor's side names this module. Each call takes the
//! trace of the work that caused it (looper#51), which the HTTP clients send as `X-Tacplus-Trace-Id`; `memory`
//! implements both in memory, for the tests and for an Admiral with no forge credentials.

/// Why a forge call failed. It never carries a credential, nor a record's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeError(pub String);

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ForgeError {}

/// A record to write in the private triage project: one confidential issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRecord {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// A record as the triage project holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The project's id for it (a GitLab issue's iid): what a public issue names it by, and nothing more.
    pub id: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub open: bool,
    /// When it closed (Unix ms), once it has.
    pub closed_at_ms: Option<i64>,
}

/// The private triage project.
#[async_trait::async_trait]
pub trait TriageProject: Send + Sync {
    /// The record whose body holds `marker`, if there is one.
    async fn find(&self, trace_id: &str, marker: &str) -> Result<Option<Record>, ForgeError>;
    /// Writes a new record, confidential.
    async fn create(&self, trace_id: &str, record: &NewRecord) -> Result<Record, ForgeError>;
    /// Replaces a record's body and labels.
    async fn update(&self, trace_id: &str, id: u64, body: &str, labels: &[String]) -> Result<(), ForgeError>;
    async fn close(&self, trace_id: &str, id: u64) -> Result<(), ForgeError>;
    /// The records carrying every label of `labels`, open or closed.
    async fn labelled(&self, trace_id: &str, labels: &[&str], open: bool) -> Result<Vec<Record>, ForgeError>;
}

/// A public issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicIssue {
    /// `tacplus-dev/<repository>`.
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// A public issue to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIssue {
    pub repository: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// The public tracker.
#[async_trait::async_trait]
pub trait PublicTracker: Send + Sync {
    /// An open issue of `repository` whose body holds `marker`, if there is one.
    async fn find_open(&self, trace_id: &str, repository: &str, marker: &str)
        -> Result<Option<PublicIssue>, ForgeError>;
    async fn open(&self, trace_id: &str, issue: &NewIssue) -> Result<PublicIssue, ForgeError>;
    /// Whether the issue is still open.
    async fn is_open(&self, trace_id: &str, repository: &str, number: u64) -> Result<bool, ForgeError>;
}

/// A bearer credential for one forge: how it is minted (the GitHub App's installation token, the triage project's
/// token through workload identity federation) is the credential's business, not the client's.
#[async_trait::async_trait]
pub trait Credential: Send + Sync {
    async fn bearer(&self) -> Result<String, ForgeError>;
}
