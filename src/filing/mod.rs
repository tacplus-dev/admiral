// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The deterministic filing core (#5): the only part of the Admiral that holds the forges' credentials, and it files
//! only what triage proposed, after its own checks (tacplus-dev/roadmap decisions/skipper-and-admiral-2026-11-01.md,
//! the 2026-11-19 addenda: an LLM may propose; it never holds a token or a write path).
//!
//! - **Its checks**, on every proposal, whoever made it: the proposal's own (its bounds, a target that fits its
//!   cause, no secret's shape, tenant identifier or user's identity in its text), the target against the allow-list
//!   (the private triage project, or a tacplus-dev repository the deployment's Config names), the public issue's
//!   whole text against the same leaks once its evidence is added, and duplicates: one record per report (a revision
//!   updates it) and one public issue per cause (its fingerprint, the target and the symptom's shape).
//! - **The private record first:** every report's full record goes to `seabeam/triage` before anything is public.
//!   If it cannot be written, nothing is filed.
//! - **A platform cause also gets a public issue** in its tacplus-dev repository, holding only sanitized evidence
//!   and a reproduction from generated fixtures (render.rs), and naming its private record by id only. Tenant data
//!   never reaches it.
//! - **Retention:** a record closes when its public issue does (or when a person closes it); 90 days after it
//!   closed, its evidence is purged and its summary kept (`sweep`).
//! - **Its credentials** (credentials.rs) are minted from the Admiral's own identity through GCP: no long-lived
//!   secret in its config.

pub mod credentials;
#[cfg(test)]
mod fake_api;
pub mod forge;
pub mod github;
pub mod gitlab;
pub mod memory;
pub mod render;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use admiral_model::types::services::admiral::Report;

use crate::core::proposal::{leak, target_fits, Cause, IssueProposal, Rejection, Withheld, PRIVATE_TRIAGE};
use crate::core::symptoms::{Grouped, SymptomKey};
use crate::core::{Escalation, Proposed, ProposedBy, Reporter};
use crate::filing::forge::{ForgeError, NewIssue, NewRecord, PublicIssue, PublicTracker, Record, TriageProject};

/// How long a closed record keeps its evidence (the 2026-11-19 defaults: until its issue closes plus 90 days).
pub const PURGE_AFTER_MS: i64 = 90 * 86_400_000;

/// Every record the core writes carries this label.
pub const LABEL_ADMIRAL: &str = "admiral";
/// A record that still holds its evidence.
pub const LABEL_EVIDENCE: &str = "evidence";
/// A record whose evidence was purged.
pub const LABEL_PURGED: &str = "evidence-purged";
/// A record with a public issue, which closes it.
pub const LABEL_PUBLIC: &str = "public-issue";
/// How often the records are swept: closed with their public issue, and purged.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3600);

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// What filing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    /// The private record's id.
    pub record: u64,
    /// The public issue, for a platform cause: the one opened, or the open one it duplicates.
    pub public: Option<PublicIssue>,
    /// Whether the public issue was there already (another report of the same cause, or an earlier revision).
    pub duplicate: bool,
    /// Why a platform cause got no public issue, when it got none.
    pub withheld: Option<String>,
}

/// Why nothing was filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilingError {
    /// The proposal failed the core's checks.
    Refused(Rejection),
    /// The private record could not be written (so nothing was), or the public issue could not be opened after it.
    Forge(ForgeError),
}

impl std::fmt::Display for FilingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilingError::Refused(r) => write!(f, "the filing core refused the proposal: {r}"),
            FilingError::Forge(e) => write!(f, "filing: {e}"),
        }
    }
}

impl std::error::Error for FilingError {}

impl From<ForgeError> for FilingError {
    fn from(e: ForgeError) -> Self {
        FilingError::Forge(e)
    }
}

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Swept {
    /// Records closed because their public issue closed.
    pub closed: usize,
    /// Records whose evidence was purged.
    pub purged: usize,
}

/// One thing to file: a report, or a symptom group's bug.
struct Subject<'a> {
    key: String,
    proposed: &'a Proposed,
    symptom: SymptomKey,
    report: Option<(&'a str, &'a Report)>,
    group: Option<&'a Grouped>,
    withheld: Withheld,
    evidence: String,
}

/// The filing core: the two forges, and the public repositories the deployment allows.
pub struct Filing {
    project: Arc<dyn TriageProject>,
    tracker: Arc<dyn PublicTracker>,
    public_repositories: RwLock<BTreeSet<String>>,
    /// The public issues this process opened, by fingerprint: the tracker's search may not show one yet.
    opened: Mutex<HashMap<String, PublicIssue>>,
}

impl Filing {
    /// A filing core with no public repository allowed: every report gets its private record only, until Config
    /// names some.
    pub fn new(project: Arc<dyn TriageProject>, tracker: Arc<dyn PublicTracker>) -> Self {
        Filing { project, tracker, public_repositories: RwLock::default(), opened: Mutex::default() }
    }

    /// Sets the public repositories a platform cause may be filed in: each a tacplus-dev repository (a platform
    /// service's, or the generator's). Any other name is dropped, and returned.
    pub fn allow_public(&self, repositories: impl IntoIterator<Item = String>) -> Vec<String> {
        let (allowed, dropped): (Vec<String>, Vec<String>) = repositories
            .into_iter()
            .partition(|r| target_fits(Cause::PlatformService, r) || target_fits(Cause::Generator, r));
        *self.public_repositories.write().unwrap() = allowed.into_iter().collect();
        dropped
    }

    fn allows_public(&self, repository: &str) -> bool {
        self.public_repositories.read().unwrap().contains(repository)
    }

    /// Files a tenant's report as triage proposed it (its held revision): its private record first, and for a
    /// platform cause its public issue. `trace_id` is the trace of the message that brought the report: every forge
    /// call this makes carries it.
    pub async fn file_report(
        &self,
        trace_id: &str,
        reporter: Reporter<'_>,
        report: &Report,
        proposed: &Proposed,
    ) -> Result<Filed, FilingError> {
        let symptom = SymptomKey::of(&report.symptom.clone().unwrap_or_default(), reporter.tenant_names);
        let subject = Subject {
            key: render::report_key(reporter.tenant, &report.report_id),
            proposed,
            symptom,
            report: Some((reporter.tenant, report)),
            group: None,
            withheld: Withheld { tenant_names: reporter.tenant_names.to_vec() },
            evidence: render::report_evidence(report),
        };
        self.file(trace_id, subject).await
    }

    /// Files a symptom group's own bug: its record holds the symptom's shape and counts only. `trace_id` is the
    /// trace of the report that made the group escalate.
    pub async fn file_group(&self, trace_id: &str, escalation: &Escalation) -> Result<Filed, FilingError> {
        let subject = Subject {
            key: render::group_key(&escalation.group.key),
            proposed: &escalation.proposed,
            symptom: escalation.group.key.clone(),
            report: None,
            group: Some(&escalation.group),
            withheld: Withheld::default(),
            evidence: render::group_evidence(&escalation.group),
        };
        self.file(trace_id, subject).await
    }

    async fn file(&self, trace_id: &str, subject: Subject<'_>) -> Result<Filed, FilingError> {
        let issue = &subject.proposed.issue;
        checked(issue, &subject.withheld).map_err(FilingError::Refused)?;
        let by = match &subject.proposed.by {
            ProposedBy::Model(alias) => alias.clone(),
            ProposedBy::Core(_) => "the Admiral's core, with no model".to_string(),
        };
        let public_wanted = issue.target_repository != PRIVATE_TRIAGE;
        let mut withheld = None;
        if public_wanted && !self.allows_public(&issue.target_repository) {
            withheld = Some(format!("{} is not on this deployment's allow-list", issue.target_repository));
        }

        // The record first: found by its key (a revision, or a report sent again after a restart), else written.
        let marker = render::record_marker(&subject.key);
        let existing = self.project.find(trace_id, &marker).await?;
        let pending = match (&withheld, public_wanted) {
            (Some(why), _) => Err(why.as_str()),
            (None, true) => Err("not opened yet"),
            (None, false) => Err("the cause is not the platform's"),
        };
        let body = self.record_body(&subject, &by, pending, None);
        let mut labels = record_labels(issue, false);
        let record = match existing {
            Some(r) => {
                // A record that had a public issue keeps it, until the issue is open no longer.
                if r.labels.iter().any(|l| l == LABEL_PUBLIC) {
                    labels.push(LABEL_PUBLIC.to_string());
                }
                let keep = render::public_of(&r.body);
                let body = match &keep {
                    Some((repo, n)) => self.record_body(
                        &subject,
                        &by,
                        Ok(&format!("https://github.com/{repo}/issues/{n}")),
                        Some(render::public_marker(repo, *n)),
                    ),
                    None => body,
                };
                self.project.update(trace_id, r.id, &body, &labels).await?;
                Record { body, labels: labels.clone(), ..r }
            }
            None => {
                let new = NewRecord { title: record_title(issue), body, labels: labels.clone() };
                self.project.create(trace_id, &new).await?
            }
        };
        let mut filed = Filed { record: record.id, public: None, duplicate: false, withheld: withheld.clone() };
        if !public_wanted || withheld.is_some() {
            return Ok(filed);
        }

        // The public issue: the open one of the same cause, else a new one, if its text passes the checks.
        let fingerprint = render::fingerprint(&issue.target_repository, &subject.symptom);
        let found = match self.known(trace_id, &record, &fingerprint, &issue.target_repository).await? {
            Some(issue) => Some((issue, true)),
            None => match self.public_text(&subject, &by, record.id, &fingerprint) {
                Ok(new) => {
                    let opened = self.tracker.open(trace_id, &new).await?;
                    self.opened.lock().unwrap().insert(fingerprint.clone(), opened.clone());
                    Some((opened, false))
                }
                Err(why) => {
                    filed.withheld = Some(why);
                    None
                }
            },
        };
        let Some((public, duplicate)) = found else {
            let why = filed.withheld.clone().unwrap_or_default();
            let body = self.record_body(&subject, &by, Err(&why), None);
            self.project.update(trace_id, record.id, &body, &labels).await?;
            return Ok(filed);
        };
        let body = self.record_body(
            &subject,
            &by,
            Ok(&public.url),
            Some(render::public_marker(&public.repository, public.number)),
        );
        let labels = record_labels(issue, true);
        self.project.update(trace_id, record.id, &body, &labels).await?;
        filed.public = Some(public);
        filed.duplicate = duplicate;
        Ok(filed)
    }

    /// The open public issue of this cause, if there is one: the record's own (an earlier revision's), one this
    /// process opened, or one the tracker finds by the fingerprint.
    async fn known(
        &self,
        trace_id: &str,
        record: &Record,
        fingerprint: &str,
        repository: &str,
    ) -> Result<Option<PublicIssue>, ForgeError> {
        if let Some((repo, number)) = render::public_of(&record.body) {
            if repo == repository && self.tracker.is_open(trace_id, &repo, number).await? {
                let url = format!("https://github.com/{repo}/issues/{number}");
                return Ok(Some(PublicIssue { repository: repo, number, url }));
            }
        }
        let cached = self.opened.lock().unwrap().get(fingerprint).cloned();
        if let Some(issue) = cached {
            if self.tracker.is_open(trace_id, &issue.repository, issue.number).await? {
                return Ok(Some(issue));
            }
            self.opened.lock().unwrap().remove(fingerprint);
        }
        self.tracker.find_open(trace_id, repository, &render::fingerprint_marker(fingerprint)).await
    }

    fn record_body(
        &self,
        subject: &Subject<'_>,
        by: &str,
        public: Result<&str, &str>,
        public_marker: Option<String>,
    ) -> String {
        let summary = render::Summary {
            issue: &subject.proposed.issue,
            by,
            report: subject.report.map(|(tenant, r)| (tenant, r.report_id.as_str(), r.revision)),
            public,
            public_marker,
        };
        render::record_body(&subject.key, &summary, &subject.evidence)
    }

    /// The public issue to open, checked as a whole: its title and its body, evidence and all, hold no secret's
    /// shape, tenant identifier, user's identity or the report's id. Evidence that does not pass is left out; if
    /// even the model's body does not, there is no public issue (why).
    fn public_text(
        &self,
        subject: &Subject<'_>,
        by: &str,
        record_id: u64,
        fingerprint: &str,
    ) -> Result<NewIssue, String> {
        let issue = &subject.proposed.issue;
        let report = subject.report.map(|(_, r)| r);
        let full = render::public_body(
            issue,
            by,
            &subject.symptom,
            report,
            subject.group,
            record_id,
            fingerprint,
            &subject.withheld.tenant_names,
        );
        let minimal = render::minimal_public_body(issue, by, record_id, fingerprint);
        let title = render::defanged(&issue.title);
        if public_leak(&title, subject).is_some() {
            return Err("its title did not pass the checks".to_string());
        }
        let body = [full, minimal].into_iter().find(|b| public_leak(b, subject).is_none());
        let Some(body) = body else {
            return Err("its text did not pass the checks".to_string());
        };
        Ok(NewIssue {
            repository: issue.target_repository.clone(),
            title,
            body,
            labels: issue.labels.iter().map(|l| l.name().to_string()).collect(),
        })
    }

    /// Closes the records whose public issue closed, and purges the evidence of every record closed PURGE_AFTER_MS
    /// ago or more, keeping its summary.
    pub async fn sweep(&self, now_ms: i64) -> Result<Swept, ForgeError> {
        // trace: unrelated: the sweep runs on a timer, caused by no request.
        let trace_id = "";
        let mut swept = Swept::default();
        for record in self.project.labelled(trace_id, &[LABEL_ADMIRAL, LABEL_PUBLIC], true).await? {
            let Some((repository, number)) = render::public_of(&record.body) else { continue };
            if !self.tracker.is_open(trace_id, &repository, number).await? {
                self.project.close(trace_id, record.id).await?;
                swept.closed += 1;
            }
        }
        for record in self.project.labelled(trace_id, &[LABEL_ADMIRAL, LABEL_EVIDENCE], false).await? {
            let Some(closed) = record.closed_at_ms else { continue };
            if now_ms - closed < PURGE_AFTER_MS {
                continue;
            }
            let body = render::purged(&record.body, &render::date(now_ms));
            let mut labels: Vec<String> = record.labels.into_iter().filter(|l| l != LABEL_EVIDENCE).collect();
            labels.push(LABEL_PURGED.to_string());
            self.project.update(trace_id, record.id, body.as_deref().unwrap_or(&record.body), &labels).await?;
            swept.purged += 1;
        }
        Ok(swept)
    }

    /// Sweeps every `period` for as long as it runs: how the Admiral keeps the retention rule.
    pub async fn sweep_every(self: Arc<Self>, period: Duration) {
        let mut tick = tokio::time::interval(period);
        loop {
            tick.tick().await;
            match self.sweep(now_ms()).await {
                Ok(swept) => tracing::info!(closed = swept.closed, purged = swept.purged, "filing: swept the records"),
                Err(e) => tracing::warn!(error = %e, "filing: the sweep failed; the next one tries again"),
            }
        }
    }
}

/// The proposal's own checks, again: triage validated it, and the core does not take triage's word for it. The
/// labels triage's core added (`bug`, `not yet reproduced`, `symptom-group`) are the core's to add.
fn checked(issue: &IssueProposal, withheld: &Withheld) -> Result<(), Rejection> {
    let labels = issue.labels.iter().copied().filter(|l| l.is_proposable()).collect();
    IssueProposal { labels, ..issue.clone() }.validate(withheld)
}

/// A public text's first leak: what the proposal's checks refuse, or the report's own id.
fn public_leak(text: &str, subject: &Subject<'_>) -> Option<String> {
    if let Some(l) = leak(text, &subject.withheld) {
        return Some(l.to_string());
    }
    let id = subject.report.map(|(_, r)| r.report_id.as_str()).unwrap_or("");
    (id.len() >= 6 && text.contains(id)).then(|| "the report's id".to_string())
}

fn record_title(issue: &IssueProposal) -> String {
    issue.title.clone()
}

fn record_labels(issue: &IssueProposal, public: bool) -> Vec<String> {
    let mut labels = vec![LABEL_ADMIRAL.to_string(), LABEL_EVIDENCE.to_string()];
    labels.extend(issue.labels.iter().map(|l| l.name().to_string()));
    if public {
        labels.push(LABEL_PUBLIC.to_string());
    }
    labels
}

#[cfg(test)]
mod tests;
