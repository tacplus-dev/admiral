// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! Both forges in memory: the tests' fakes, which record each call with its trace, and can be told to fail.

use std::sync::Mutex;

use crate::filing::forge::{ForgeError, NewIssue, NewRecord, PublicIssue, PublicTracker, Record, TriageProject};

/// One call a fake forge took: what it was, and the trace it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub what: &'static str,
    pub trace_id: String,
}

/// The private triage project, in memory.
#[derive(Default)]
pub struct MemoryProject {
    pub records: Mutex<Vec<Record>>,
    pub calls: Mutex<Vec<Call>>,
    /// While set, every write fails.
    pub failing: Mutex<bool>,
}

impl MemoryProject {
    fn call(&self, what: &'static str, trace_id: &str) -> Result<(), ForgeError> {
        self.calls.lock().unwrap().push(Call { what, trace_id: trace_id.to_string() });
        if *self.failing.lock().unwrap() && what != "find" && what != "labelled" {
            return Err(ForgeError(format!("the triage project refused {what}")));
        }
        Ok(())
    }

    pub fn record(&self, id: u64) -> Option<Record> {
        self.records.lock().unwrap().iter().find(|r| r.id == id).cloned()
    }

    /// Closes a record as a person would, at `at_ms`.
    pub fn close_at(&self, id: u64, at_ms: i64) {
        if let Some(r) = self.records.lock().unwrap().iter_mut().find(|r| r.id == id) {
            r.open = false;
            r.closed_at_ms = Some(at_ms);
        }
    }

    pub fn traces(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.trace_id.clone()).collect()
    }
}

#[async_trait::async_trait]
impl TriageProject for MemoryProject {
    async fn find(&self, trace_id: &str, marker: &str) -> Result<Option<Record>, ForgeError> {
        self.call("find", trace_id)?;
        Ok(self.records.lock().unwrap().iter().find(|r| r.body.contains(marker)).cloned())
    }

    async fn create(&self, trace_id: &str, record: &NewRecord) -> Result<Record, ForgeError> {
        self.call("create", trace_id)?;
        let mut records = self.records.lock().unwrap();
        let created = Record {
            id: records.len() as u64 + 1,
            title: record.title.clone(),
            body: record.body.clone(),
            labels: record.labels.clone(),
            open: true,
            closed_at_ms: None,
        };
        records.push(created.clone());
        Ok(created)
    }

    async fn update(&self, trace_id: &str, id: u64, body: &str, labels: &[String]) -> Result<(), ForgeError> {
        self.call("update", trace_id)?;
        let mut records = self.records.lock().unwrap();
        let r = records.iter_mut().find(|r| r.id == id).ok_or_else(|| ForgeError(format!("no record {id}")))?;
        r.body = body.to_string();
        r.labels = labels.to_vec();
        Ok(())
    }

    async fn close(&self, trace_id: &str, id: u64) -> Result<(), ForgeError> {
        self.call("close", trace_id)?;
        let mut records = self.records.lock().unwrap();
        let r = records.iter_mut().find(|r| r.id == id).ok_or_else(|| ForgeError(format!("no record {id}")))?;
        r.open = false;
        r.closed_at_ms = Some(crate::filing::now_ms());
        Ok(())
    }

    async fn labelled(&self, trace_id: &str, labels: &[&str], open: bool) -> Result<Vec<Record>, ForgeError> {
        self.call("labelled", trace_id)?;
        let records = self.records.lock().unwrap();
        Ok(records
            .iter()
            .filter(|r| r.open == open && labels.iter().all(|l| r.labels.iter().any(|have| have == l)))
            .cloned()
            .collect())
    }
}

/// An issue the public tracker holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldIssue {
    pub issue: NewIssue,
    pub number: u64,
    pub open: bool,
}

/// The public tracker, in memory.
#[derive(Default)]
pub struct MemoryTracker {
    pub issues: Mutex<Vec<HeldIssue>>,
    pub calls: Mutex<Vec<Call>>,
    pub failing: Mutex<bool>,
}

impl MemoryTracker {
    fn call(&self, what: &'static str, trace_id: &str) -> Result<(), ForgeError> {
        self.calls.lock().unwrap().push(Call { what, trace_id: trace_id.to_string() });
        if *self.failing.lock().unwrap() {
            return Err(ForgeError(format!("the tracker refused {what}")));
        }
        Ok(())
    }

    fn public(held: &HeldIssue) -> PublicIssue {
        PublicIssue {
            repository: held.issue.repository.clone(),
            number: held.number,
            url: format!("https://github.com/{}/issues/{}", held.issue.repository, held.number),
        }
    }

    /// Closes an issue, as its fix landing would.
    pub fn close(&self, number: u64) {
        if let Some(i) = self.issues.lock().unwrap().iter_mut().find(|i| i.number == number) {
            i.open = false;
        }
    }

    pub fn traces(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.trace_id.clone()).collect()
    }
}

#[async_trait::async_trait]
impl PublicTracker for MemoryTracker {
    async fn find_open(
        &self,
        trace_id: &str,
        repository: &str,
        marker: &str,
    ) -> Result<Option<PublicIssue>, ForgeError> {
        self.call("find_open", trace_id)?;
        let issues = self.issues.lock().unwrap();
        Ok(issues
            .iter()
            .find(|i| i.open && i.issue.repository == repository && i.issue.body.contains(marker))
            .map(Self::public))
    }

    async fn open(&self, trace_id: &str, issue: &NewIssue) -> Result<PublicIssue, ForgeError> {
        self.call("open", trace_id)?;
        let mut issues = self.issues.lock().unwrap();
        let held = HeldIssue { issue: issue.clone(), number: issues.len() as u64 + 1, open: true };
        issues.push(held.clone());
        Ok(Self::public(&held))
    }

    async fn is_open(&self, trace_id: &str, repository: &str, number: u64) -> Result<bool, ForgeError> {
        self.call("is_open", trace_id)?;
        let issues = self.issues.lock().unwrap();
        Ok(issues.iter().any(|i| i.open && i.number == number && i.issue.repository == repository))
    }
}
