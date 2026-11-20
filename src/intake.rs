// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! Report intake (#2): the Admiral's side of BrokerSkipperChannel, the one stream each Skipper opens and holds.
//!
//! Every message from a Skipper is untrusted (tacplus-dev/looper#40). Before one reaches `receive`, the generated
//! wrapper has verified the Skipper's token (only `skipper` may open the stream) and checked the message against
//! the bounds proto/services/admiral.proto declares. Here the tenant is the token's namespace, never a field; a
//! report is screened again (secrets, other tenants' data, users' identities: src/screen.rs) before it is held;
//! no text of it is interpreted or logged; and every message is audited (target `admiral::audit`: the tenant, the
//! message's kind, the report and revision, the outcome).
//!
//! The Admiral holds a tenant's reports in memory, the highest revision of each, with the updates it sent: a
//! Skipper that reconnects names its open reports in its hello and is sent the updates it missed, and one the
//! Admiral holds no record of (after a restart, say) it is told to send again.
//!
//! Each new report, and each newer revision, is triaged in the background (#4, src/core): its updates TRIAGED, and
//! GROUPED when it was not reproduced, are held with it and sent on the tenant's open stream; when its symptom
//! group escalates, every report of the group is sent ESCALATED. The triage is kept with the report for the filing
//! core (#5). The triage's model calls are caused by the message that brought the report, so they carry its
//! trace_id (looper#51); the intake makes no other outgoing call.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use admiral::core::{Proposed, Reporter, Triage, Triaged};
use admiral_model::types::services::admiral::{
    admiral_message, skipper_message, AdmiralHello, AdmiralMessage, Report, ReportAccepted, ReportUpdate, SkipperHello,
    SkipperMessage, Stage,
};
use futures::StreamExt;
use servicemanager::auth::Caller;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tonic::Status;

use crate::screen::{screen, OwnTenant};

pub type SkipperStream = Pin<Box<dyn futures::Stream<Item = Result<SkipperMessage, Status>> + Send>>;
pub type AdmiralStream = Pin<Box<dyn futures::Stream<Item = Result<AdmiralMessage, Status>> + Send>>;

/// The most reports the Admiral holds for one tenant: as many as a hello may name.
pub const MAX_REPORTS_PER_TENANT: usize = 4096;

/// How many messages to a Skipper may wait to be sent before the stream's reader waits for them.
const OUTBOUND: usize = 64;

/// How many reports are triaged at once: each is up to a few model calls, so this bounds the Admiral's model spend
/// however fast the Skippers send.
pub const TRIAGES_AT_ONCE: usize = 4;

const AUDIT: &str = "admiral::audit";

/// The Skipper on one stream, as its verified token names it.
#[derive(Clone)]
pub struct Skipper {
    /// tenant is the token's normalized namespace (`/tenants/<tenant>`): the key every report is held under.
    tenant: String,
    /// names are the identifiers of the tenant (its namespace's, and its SPIFFE id's), for the screening.
    names: Vec<String>,
}

impl Skipper {
    /// from_caller names the Skipper by its verified token. The generated Authorizer admits only `skipper`, a
    /// tenant-owned service, so a caller outside a tenant is refused here too.
    pub fn from_caller(caller: Option<&Caller>) -> Result<Self, Status> {
        let caller = caller.ok_or_else(|| Status::unauthenticated("the Skipper channel needs the Skipper's token"))?;
        let tenant = caller
            .token()
            .namespace_path()
            .map_err(|e| Status::unauthenticated(format!("the Skipper's token: {e}")))?;
        let name = tenant
            .strip_prefix("/tenants/")
            .and_then(|rest| rest.split('/').next())
            .filter(|seg| !seg.is_empty())
            .ok_or_else(|| Status::permission_denied("only a tenant's Skipper may open the Skipper channel"))?
            .to_string();
        let mut names = vec![name];
        if let Some(id) = caller.token().subject().and_then(tenant_segment) {
            names.push(id.to_string());
        }
        Ok(Self { tenant, names })
    }
}

/// The segment after `/tenants/` in a SPIFFE id.
fn tenant_segment(path: &str) -> Option<&str> {
    let rest = &path[path.find("/tenants/")? + "/tenants/".len()..];
    let seg = rest.split('/').next().unwrap_or("");
    (!seg.is_empty()).then_some(seg)
}

struct Held {
    report: Report,
    updates: Vec<ReportUpdate>,
    /// The triage of the held revision, once it is done.
    triaged: Option<Triaged>,
}

impl Held {
    /// Adds the report's next update and returns it.
    fn update(&mut self, stage: Stage, proposed: &Proposed) -> ReportUpdate {
        let update = ReportUpdate {
            report_id: self.report.report_id.clone(),
            seq: self.updates.last().map_or(1, |u| u.seq + 1),
            stage: stage as i32,
            at_ms: now_ms(),
            finding: Some(proposed.finding()),
            ..Default::default()
        };
        self.updates.push(update.clone());
        update
    }
}

type Outbound = mpsc::Sender<Result<AdmiralMessage, Status>>;

#[derive(Default)]
struct Tenant {
    /// stream counts the tenant's streams; the latest is the open one.
    stream: u64,
    /// close ends the open stream when a newer one replaces it.
    close: Option<oneshot::Sender<()>>,
    /// outbound sends on the open stream, for the updates triage makes after the message that caused them.
    outbound: Option<Outbound>,
    reports: HashMap<String, Held>,
}

impl Tenant {
    /// Sends `update` on the open stream, if there is one with room: a Skipper that misses it is sent it again
    /// after its next hello, which names the last seq it holds.
    fn send(&self, update: ReportUpdate) {
        if let Some(outbound) = &self.outbound {
            let _ = outbound.try_send(Ok(message(admiral_message::Body::Update(update))));
        }
    }
}

/// Intake holds what the Skippers sent, per tenant, and serves their streams.
pub struct Intake {
    tenants: Mutex<HashMap<String, Tenant>>,
    max_reports: usize,
    /// triage, when the Admiral triages what it receives.
    triage: Option<Arc<Triage>>,
    triages: Semaphore,
}

impl Default for Intake {
    fn default() -> Self {
        Self::with_limit(MAX_REPORTS_PER_TENANT)
    }
}

impl Intake {
    /// An intake that holds reports and triages none: a report's one update is RECEIVED.
    pub fn with_limit(max_reports: usize) -> Self {
        Self {
            tenants: Mutex::new(HashMap::new()),
            max_reports,
            triage: None,
            triages: Semaphore::new(TRIAGES_AT_ONCE),
        }
    }

    /// An intake that triages each report it receives with `triage`.
    pub fn with_triage(triage: Arc<Triage>) -> Self {
        Self { triage: Some(triage), ..Self::default() }
    }

    /// serve takes one Skipper's stream. A newer stream of the same tenant replaces it (latest wins): the
    /// replaced one is sent `last` and ends.
    pub fn serve(self: &Arc<Self>, skipper: Skipper, inbound: SkipperStream) -> AdmiralStream {
        let (tx, mut rx) = mpsc::channel(OUTBOUND);
        let (close_tx, close_rx) = oneshot::channel();
        let stream = {
            let mut tenants = self.tenants.lock().unwrap();
            let tenant = tenants.entry(skipper.tenant.clone()).or_default();
            tenant.stream += 1;
            if let Some(old) = tenant.close.replace(close_tx) {
                let _ = old.send(());
            }
            tenant.outbound = Some(tx.clone());
            tenant.stream
        };
        tracing::info!(target: AUDIT, tenant = %skipper.tenant, stream, "opened");
        tokio::spawn(self.clone().run(skipper, stream, inbound, tx, close_rx));
        Box::pin(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)))
    }

    async fn run(
        self: Arc<Self>,
        skipper: Skipper,
        stream: u64,
        mut inbound: SkipperStream,
        tx: mpsc::Sender<Result<AdmiralMessage, Status>>,
        mut close: oneshot::Receiver<()>,
    ) {
        let mut hello = false;
        let end = loop {
            let item = tokio::select! {
                _ = &mut close => {
                    let _ = tx.send(Ok(AdmiralMessage { last: true, ..Default::default() })).await;
                    break "replaced";
                }
                item = inbound.next() => item,
            };
            let msg = match item {
                None => break "ended without last",
                Some(Err(status)) => {
                    // The generated wrapper refused the message (over a bound, say): the stream ends with it.
                    tracing::warn!(target: AUDIT, tenant = %skipper.tenant, stream, code = ?status.code(), "refused");
                    let _ = tx.send(Err(status)).await;
                    break "refused";
                }
                Some(Ok(msg)) => msg,
            };
            let last = msg.last;
            match self.receive(&skipper, &mut hello, msg) {
                Ok(replies) => {
                    for reply in replies {
                        if tx.send(Ok(reply)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(status) => {
                    let _ = tx.send(Err(status)).await;
                    break "refused";
                }
            }
            if last {
                break "last";
            }
            if tx.is_closed() {
                break "gone";
            }
        };
        tracing::info!(target: AUDIT, tenant = %skipper.tenant, stream, end, "closed");
        let mut tenants = self.tenants.lock().unwrap();
        if let Some(tenant) = tenants.get_mut(&skipper.tenant) {
            if tenant.stream == stream {
                tenant.close = None;
                tenant.outbound = None;
            }
        }
    }

    /// receive takes one message of a Skipper's stream, already validated, and answers it. `hello` is whether
    /// the stream's hello came. An error ends the stream.
    pub fn receive(
        self: &Arc<Self>,
        skipper: &Skipper,
        hello: &mut bool,
        msg: SkipperMessage,
    ) -> Result<Vec<AdmiralMessage>, Status> {
        let tenant = skipper.tenant.as_str();
        let Some(body) = msg.body else {
            if msg.last {
                tracing::info!(target: AUDIT, tenant, message = "last", "goodbye");
                return Ok(vec![]);
            }
            tracing::warn!(target: AUDIT, tenant, message = "empty", "refused");
            return Err(Status::invalid_argument("a SkipperMessage carries a body or `last`"));
        };
        match body {
            skipper_message::Body::Hello(h) => {
                if std::mem::replace(hello, true) {
                    tracing::warn!(target: AUDIT, tenant, message = "hello", "refused: a second hello");
                    return Err(Status::failed_precondition("the stream has had its hello"));
                }
                Ok(self.hello(skipper, h))
            }
            _ if !*hello => {
                tracing::warn!(target: AUDIT, tenant, message = kind(&body), "refused: before the hello");
                Err(Status::failed_precondition("a Skipper's first message is its SkipperHello"))
            }
            skipper_message::Body::Report(r) => self.report(skipper, r, &msg.trace_id),
            skipper_message::Body::Answer(a) => {
                // The Admiral sends no request yet (triage, #4, will): an answer answers nothing it asked.
                tracing::warn!(target: AUDIT, tenant, message = "answer", request_id = ?a.request_id, "ignored: no such request");
                Ok(vec![])
            }
        }
    }

    fn hello(&self, skipper: &Skipper, hello: SkipperHello) -> Vec<AdmiralMessage> {
        let tenants = self.tenants.lock().unwrap();
        let held = tenants.get(&skipper.tenant).map(|t| &t.reports);
        let mut unknown = Vec::new();
        let mut missed = Vec::new();
        for cursor in &hello.open_reports {
            match held.and_then(|h| h.get(&cursor.report_id)) {
                Some(h) => missed.extend(h.updates.iter().filter(|u| u.seq > cursor.last_seq).cloned()),
                None => unknown.push(cursor.report_id.clone()),
            }
        }
        tracing::info!(
            target: AUDIT,
            tenant = %skipper.tenant,
            message = "hello",
            skipper_version = ?hello.skipper_version,
            open = hello.open_reports.len(),
            unknown = unknown.len(),
            replayed = missed.len(),
            "accepted"
        );
        let mut replies = vec![message(admiral_message::Body::Hello(AdmiralHello {
            admiral_version: env!("CARGO_PKG_VERSION").to_string(),
            unknown_reports: unknown,
            ..Default::default()
        }))];
        replies.extend(missed.into_iter().map(|u| message(admiral_message::Body::Update(u))));
        replies
    }

    fn report(
        self: &Arc<Self>,
        skipper: &Skipper,
        mut report: Report,
        trace_id: &str,
    ) -> Result<Vec<AdmiralMessage>, Status> {
        let found = screen(&mut report, &OwnTenant::new(skipper.names.iter().cloned()));
        let (id, revision) = (report.report_id.clone(), report.revision);
        let audit = |outcome: &str| {
            tracing::info!(
                target: AUDIT,
                tenant = %skipper.tenant,
                message = "report",
                report_id = ?id,
                revision,
                secrets_redacted = found.secrets,
                other_tenants_redacted = found.other_tenants,
                identities_redacted = found.identities,
                outcome
            )
        };
        let accepted = message(admiral_message::Body::Accepted(ReportAccepted {
            report_id: id.clone(),
            revision,
            ..Default::default()
        }));

        let mut tenants = self.tenants.lock().unwrap();
        let tenant = tenants.entry(skipper.tenant.clone()).or_default();
        if let Some(held) = tenant.reports.get_mut(&id) {
            if held.report.revision >= revision {
                audit("held already");
                return Ok(vec![accepted]);
            }
            held.report = report.clone();
            held.triaged = None;
            audit("revised");
            self.triage(skipper, report, trace_id);
            return Ok(vec![accepted]);
        }
        if tenant.reports.len() >= self.max_reports {
            audit("refused: the tenant's reports are at their limit");
            return Err(Status::resource_exhausted(format!(
                "the Admiral holds {} reports of this tenant already",
                self.max_reports
            )));
        }
        let received = ReportUpdate {
            report_id: id.clone(),
            seq: 1,
            stage: Stage::Received as i32,
            at_ms: now_ms(),
            ..Default::default()
        };
        let held = Held { report: report.clone(), updates: vec![received.clone()], triaged: None };
        tenant.reports.insert(id.clone(), held);
        audit("received");
        self.triage(skipper, report, trace_id);
        Ok(vec![accepted, message(admiral_message::Body::Update(received))])
    }

    /// Triages a report's revision in the background, if this intake triages; at most TRIAGES_AT_ONCE run at once.
    fn triage(self: &Arc<Self>, skipper: &Skipper, report: Report, trace_id: &str) {
        let Some(triage) = self.triage.clone() else { return };
        let (intake, skipper, trace_id) = (self.clone(), skipper.clone(), trace_id.to_string());
        tokio::spawn(async move {
            let Ok(_slot) = intake.triages.acquire().await else { return };
            let reporter = Reporter { tenant: &skipper.tenant, tenant_names: &skipper.names };
            let triaged = triage.triage(&trace_id, reporter, &report).await; // propagate
            intake.triaged(&skipper.tenant, &report, triaged, &triage);
        });
    }

    /// Holds a report's triage and tells its Skipper: TRIAGED, then GROUPED for one not reproduced; and when its
    /// group escalated, ESCALATED to every report of the group, whichever tenant's. A triage of a revision no
    /// longer held is dropped: the newer one's is on its way.
    fn triaged(&self, tenant_key: &str, report: &Report, triaged: Triaged, triage: &Triage) {
        let mut tenants = self.tenants.lock().unwrap();
        let Some(tenant) = tenants.get_mut(tenant_key) else { return };
        let Some(held) = tenant.reports.get_mut(&report.report_id).filter(|h| h.report.revision == report.revision)
        else {
            tracing::info!(
                target: AUDIT,
                tenant = tenant_key,
                report_id = ?report.report_id,
                revision = report.revision,
                "triage dropped: a newer revision is held"
            );
            return;
        };
        tracing::info!(
            target: AUDIT,
            tenant = tenant_key,
            message = "triage",
            report_id = ?report.report_id,
            revision = report.revision,
            by = ?triaged.proposed.by,
            cause = ?triaged.proposed.issue.cause,
            target_repository = %triaged.proposed.issue.target_repository,
            grouped = ?triaged.grouped.as_ref().map(|g| g.reports),
            escalated = triaged.escalation.is_some(),
            "triaged"
        );
        let mut updates = vec![held.update(Stage::Triaged, &triaged.proposed)];
        if triaged.grouped.is_some() {
            updates.push(held.update(Stage::Grouped, &triaged.proposed));
        }
        let escalation = triaged.escalation.clone();
        held.triaged = Some(triaged);
        for update in updates {
            tenant.send(update);
        }
        let Some(escalation) = escalation else { return };
        for (key, tenant) in tenants.iter_mut() {
            let mut escalated = Vec::new();
            for (id, held) in tenant.reports.iter_mut() {
                if triage.in_group(&escalation.group.key, key, id) {
                    escalated.push(held.update(Stage::Escalated, &escalation.proposed));
                }
            }
            for update in escalated {
                tenant.send(update);
            }
        }
    }

    /// triaged_of is the triage the Admiral holds for a tenant's report, once its held revision is triaged.
    pub fn triaged_of(&self, tenant: &str, report_id: &str) -> Option<Triaged> {
        self.tenants.lock().unwrap().get(tenant)?.reports.get(report_id)?.triaged.clone()
    }

    /// report_of is the report the Admiral holds for a tenant, as it holds it (screened, its highest revision).
    pub fn report_of(&self, tenant: &str, report_id: &str) -> Option<Report> {
        self.tenants.lock().unwrap().get(tenant)?.reports.get(report_id).map(|h| h.report.clone())
    }
}

fn message(body: admiral_message::Body) -> AdmiralMessage {
    AdmiralMessage { body: Some(body), ..Default::default() }
}

fn kind(body: &skipper_message::Body) -> &'static str {
    match body {
        skipper_message::Body::Hello(_) => "hello",
        skipper_message::Body::Report(_) => "report",
        skipper_message::Body::Answer(_) => "answer",
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{AdmiralStream, Intake, Skipper, SkipperStream};
    use admiral_model::types::services::admiral::{
        admiral_message, skipper_message, Classification, ReportCursor, Report, Reproduction, RequestAnswer, SkipperHello,
        SkipperMessage, Stage, Symptom,
    };
    use admiral::advisor::model::{Model, ModelError, ModelReply, ModelRequest};
    use admiral::core::proposal::{Label, PRIVATE_TRIAGE};
    use admiral::core::Triage;
    use admiral_model::types::services::admiral::{AdmiralMessage, ReportUpdate};
    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use futures::StreamExt;
    use servicemanager::auth::Caller;
    use std::sync::{Arc, Mutex};
    use tonic::{Code, Status};

    fn caller(subject: &str, namespace: &str) -> Caller {
        let mut tok = token::token::Token::new();
        tok.set_subject(subject).unwrap();
        tok.set_namespace(namespace).unwrap();
        tok.set_sub_type("service").unwrap();
        Caller::new(tok, "req-1".to_string())
    }

    fn skipper(tenant: &str) -> Skipper {
        let c = caller(&format!("spiffe://example.com/tenants/{tenant}-id/services/skipper"), &format!("/tenants/{tenant}"));
        Skipper::from_caller(Some(&c)).expect("a tenant's Skipper")
    }

    fn hello(open: &[(&str, u64)]) -> SkipperMessage {
        let open_reports = open
            .iter()
            .map(|(id, seq)| ReportCursor { report_id: id.to_string(), last_seq: *seq, ..Default::default() })
            .collect();
        SkipperMessage {
            body: Some(skipper_message::Body::Hello(SkipperHello { skipper_version: "0.1.0".to_string(), open_reports, ..Default::default() })),
            ..Default::default()
        }
    }

    fn report(id: &str, revision: u32, summary: &str) -> SkipperMessage {
        SkipperMessage {
            body: Some(skipper_message::Body::Report(Report {
                report_id: id.to_string(),
                revision,
                summary: summary.to_string(),
                symptom: Some(Symptom::default()),
                reproduction: Some(Reproduction::default()),
                classification: Some(Classification::default()),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    struct Stream {
        to: UnboundedSender<Result<SkipperMessage, Status>>,
        from: AdmiralStream,
    }

    impl Stream {
        fn open(intake: &Arc<Intake>, skipper: Skipper) -> Self {
            let (to, rx) = unbounded();
            let from = intake.serve(skipper, Box::pin(rx) as SkipperStream);
            Self { to, from }
        }

        fn send(&self, msg: SkipperMessage) {
            self.to.unbounded_send(Ok(msg)).expect("stream open");
        }

        async fn next(&mut self) -> Option<Result<AdmiralMessage, Status>> {
            tokio::time::timeout(std::time::Duration::from_secs(5), self.from.next()).await.expect("an answer in time")
        }

        async fn body(&mut self) -> admiral_message::Body {
            self.next().await.expect("a message").expect("not an error").body.expect("a body")
        }

        async fn status(&mut self) -> Status {
            self.next().await.expect("a message").err().expect("an error")
        }
    }

    #[test]
    fn the_skipper_is_its_tokens_tenant() {
        let s = skipper("acme");
        assert_eq!(s.tenant, "/tenants/acme");
        assert_eq!(s.names, ["acme", "acme-id"]);
        assert_eq!(Skipper::from_caller(None).err().unwrap().code(), Code::Unauthenticated);
        let system = caller("spiffe://example.com/system/services/kernel", "/system");
        assert_eq!(Skipper::from_caller(Some(&system)).err().unwrap().code(), Code::PermissionDenied);
    }

    #[tokio::test]
    async fn the_hello_comes_first() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(report("r-1", 1, "x"));
        assert_eq!(s.status().await.code(), Code::FailedPrecondition);
        assert!(s.next().await.is_none(), "the stream ends");
        assert!(intake.report_of("/tenants/acme", "r-1").is_none());
    }

    #[tokio::test]
    async fn a_second_hello_is_refused() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        assert!(matches!(s.body().await, admiral_message::Body::Hello(_)));
        s.send(hello(&[]));
        assert_eq!(s.status().await.code(), Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn a_report_is_accepted_and_received() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[("r-9", 0)]));
        let admiral_message::Body::Hello(h) = s.body().await else { panic!("a hello") };
        assert_eq!(h.admiral_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(h.unknown_reports, ["r-9"], "a report the Admiral holds no record of is sent again");

        s.send(report("r-1", 1, "GetOrder fails"));
        let admiral_message::Body::Accepted(a) = s.body().await else { panic!("accepted") };
        assert_eq!((a.report_id.as_str(), a.revision), ("r-1", 1));
        let admiral_message::Body::Update(u) = s.body().await else { panic!("an update") };
        assert_eq!((u.report_id.as_str(), u.seq, u.stage), ("r-1", 1, Stage::Received as i32));
        assert_eq!(intake.report_of("/tenants/acme", "r-1").unwrap().summary, "GetOrder fails");
    }

    #[tokio::test]
    async fn a_revision_held_changes_nothing_and_a_newer_one_replaces_it() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        s.send(report("r-1", 2, "second"));
        s.body().await;
        s.body().await;

        s.send(report("r-1", 1, "first"));
        let admiral_message::Body::Accepted(a) = s.body().await else { panic!("accepted again") };
        assert_eq!(a.revision, 1);
        assert_eq!(intake.report_of("/tenants/acme", "r-1").unwrap().summary, "second");

        s.send(report("r-1", 3, "third"));
        let admiral_message::Body::Accepted(a) = s.body().await else { panic!("accepted") };
        assert_eq!(a.revision, 3);
        let held = intake.report_of("/tenants/acme", "r-1").unwrap();
        assert_eq!((held.revision, held.summary.as_str()), (3, "third"));

        // No second RECEIVED: the next message answers the next report.
        s.send(report("r-2", 1, "other"));
        let admiral_message::Body::Accepted(a) = s.body().await else { panic!("accepted") };
        assert_eq!(a.report_id, "r-2");
    }

    #[tokio::test]
    async fn a_reconnecting_skipper_is_sent_what_it_missed() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        s.send(report("r-1", 1, "x"));
        s.body().await;
        s.body().await;
        s.send(SkipperMessage { last: true, ..Default::default() });
        assert!(s.next().await.is_none(), "`last` ends the stream");

        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[("r-1", 0), ("r-2", 0)]));
        let admiral_message::Body::Hello(h) = s.body().await else { panic!("a hello") };
        assert_eq!(h.unknown_reports, ["r-2"]);
        let admiral_message::Body::Update(u) = s.body().await else { panic!("the missed update") };
        assert_eq!((u.report_id.as_str(), u.seq), ("r-1", 1));

        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[("r-1", 1)]));
        let admiral_message::Body::Hello(h) = s.body().await else { panic!("a hello") };
        assert!(h.unknown_reports.is_empty());
        s.send(SkipperMessage { last: true, ..Default::default() });
        assert!(s.next().await.is_none(), "nothing missed, nothing replayed");
    }

    #[tokio::test]
    async fn a_tenants_reports_are_its_own() {
        let intake = Arc::new(Intake::default());
        let mut acme = Stream::open(&intake, skipper("acme"));
        acme.send(hello(&[]));
        acme.body().await;
        acme.send(report("r-1", 1, "x"));
        acme.body().await;

        let mut beta = Stream::open(&intake, skipper("beta"));
        beta.send(hello(&[("r-1", 0)]));
        let admiral_message::Body::Hello(h) = beta.body().await else { panic!("a hello") };
        assert_eq!(h.unknown_reports, ["r-1"], "another tenant's report is no record of this one's");
        assert!(intake.report_of("/tenants/beta", "r-1").is_none());
    }

    #[tokio::test]
    async fn a_newer_stream_replaces_the_open_one() {
        let intake = Arc::new(Intake::default());
        let mut old = Stream::open(&intake, skipper("acme"));
        old.send(hello(&[]));
        old.body().await;
        let mut new = Stream::open(&intake, skipper("acme"));
        let last = old.next().await.expect("a goodbye").expect("not an error");
        assert!(last.last && last.body.is_none());
        assert!(old.next().await.is_none());

        new.send(hello(&[]));
        assert!(matches!(new.body().await, admiral_message::Body::Hello(_)));
        let mut other = Stream::open(&intake, skipper("beta"));
        other.send(hello(&[]));
        assert!(matches!(other.body().await, admiral_message::Body::Hello(_)), "another tenant's stream is its own");
        new.send(report("r-1", 1, "x"));
        assert!(matches!(new.body().await, admiral_message::Body::Accepted(_)), "a tenant's other stream left it open");
    }

    #[tokio::test]
    async fn a_message_the_wrapper_refused_ends_the_stream() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.to.unbounded_send(Err(Status::invalid_argument("validation failed"))).unwrap();
        assert_eq!(s.status().await.code(), Code::InvalidArgument);
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn an_empty_message_is_refused_and_an_answer_to_nothing_ignored() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        s.send(SkipperMessage {
            body: Some(skipper_message::Body::Answer(RequestAnswer { request_id: "q-1".to_string(), ..Default::default() })),
            ..Default::default()
        });
        s.send(SkipperMessage::default());
        assert_eq!(s.status().await.code(), Code::InvalidArgument, "the answer had no reply; the empty message ends it");
    }

    #[tokio::test]
    async fn a_report_is_held_screened() {
        let intake = Arc::new(Intake::default());
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        s.send(report("r-1", 1, "password=hunter2 in /tenants/beta and /tenants/acme, for /users/alice"));
        s.body().await;
        let held = intake.report_of("/tenants/acme", "r-1").unwrap();
        assert_eq!(held.summary, "password=[REDACTED] in /tenants/[REDACTED] and /tenants/acme, for /users/[REDACTED]");
    }

    #[tokio::test]
    async fn a_tenants_reports_are_bounded() {
        let intake = Arc::new(Intake::with_limit(1));
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        s.send(report("r-1", 1, "x"));
        s.body().await;
        s.body().await;
        s.send(report("r-1", 2, "a revision is no new report"));
        assert!(matches!(s.body().await, admiral_message::Body::Accepted(_)));
        s.send(report("r-2", 1, "x"));
        assert_eq!(s.status().await.code(), Code::ResourceExhausted);
    }

    // llmproxy as a recorder: it records each call's trace, and answers with no proposal, so the core proposes.
    #[derive(Default)]
    struct Traces(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl Model for Traces {
        async fn complete(&self, request: ModelRequest) -> Result<ModelReply, ModelError> {
            self.0.lock().unwrap().push(request.trace_id);
            Err(ModelError("not routed in this test".to_string()))
        }
    }

    async fn update(s: &mut Stream) -> ReportUpdate {
        match s.body().await {
            admiral_message::Body::Update(u) => u,
            other => panic!("not an update: {other:?}"),
        }
    }

    /// Sends a report no Skipper reproduced and reads what it is answered: accepted, then RECEIVED, TRIAGED and
    /// GROUPED.
    async fn reported(s: &mut Stream, msg: SkipperMessage) -> Vec<ReportUpdate> {
        s.send(msg);
        assert!(matches!(s.body().await, admiral_message::Body::Accepted(_)));
        vec![update(s).await, update(s).await, update(s).await]
    }

    #[tokio::test]
    async fn a_report_is_triaged_and_its_skipper_told() {
        let intake = Arc::new(Intake::with_triage(Arc::new(Triage::without_model())));
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        let updates = reported(&mut s, report("r-1", 1, "GetOrder fails")).await;
        let stages: Vec<(u64, i32)> = updates.iter().map(|u| (u.seq, u.stage)).collect();
        assert_eq!(
            stages,
            [(1, Stage::Received as i32), (2, Stage::Triaged as i32), (3, Stage::Grouped as i32)],
            "{updates:?}"
        );
        let finding = updates[1].finding.clone().expect("a finding");
        assert_eq!(finding.classification.expect("classified").component, PRIVATE_TRIAGE);
        let triaged = intake.triaged_of("/tenants/acme", "r-1").expect("triaged");
        assert_eq!(triaged.proposed.issue.labels, [Label::NotYetReproduced]);
        assert_eq!(triaged.grouped.map(|g| g.reports), Some(1));

        // A Skipper that reconnects is sent the triage's updates it missed.
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[("r-1", 1)]));
        s.body().await;
        assert_eq!(update(&mut s).await.stage, Stage::Triaged as i32);
        assert_eq!(update(&mut s).await.stage, Stage::Grouped as i32);
    }

    // Trace propagation (tacplus-dev/looper#51): the triage's calls to llmproxy are caused by the message that
    // brought the report, so each carries its trace_id.
    #[tokio::test]
    async fn the_triage_carries_the_reports_trace_id() {
        let model = Arc::new(Traces::default());
        let intake = Arc::new(Intake::with_triage(Arc::new(Triage::with_model(model.clone()))));
        let mut s = Stream::open(&intake, skipper("acme"));
        s.send(hello(&[]));
        s.body().await;
        let msg = SkipperMessage { trace_id: "trace-report-1".to_string(), ..report("r-1", 1, "GetOrder fails") };
        reported(&mut s, msg).await;
        let traces = model.0.lock().unwrap().clone();
        assert!(!traces.is_empty());
        assert!(traces.iter().all(|t| t == "trace-report-1"), "{traces:?}");
    }

    // Several reports of one symptom no Skipper could reproduce, from two tenants, escalate: every report of the
    // group is told, each on its own tenant's stream.
    #[tokio::test]
    async fn an_escalation_reaches_every_report_of_its_group() {
        let intake = Arc::new(Intake::with_triage(Arc::new(Triage::without_model())));
        let mut acme = Stream::open(&intake, skipper("acme"));
        acme.send(hello(&[]));
        acme.body().await;
        let mut beta = Stream::open(&intake, skipper("beta"));
        beta.send(hello(&[]));
        beta.body().await;

        reported(&mut acme, report("r-1", 1, "x")).await;
        reported(&mut acme, report("r-2", 1, "y")).await;
        reported(&mut beta, report("r-1", 1, "z")).await;
        let escalated = update(&mut beta).await;
        assert_eq!((escalated.seq, escalated.stage), (4, Stage::Escalated as i32));
        assert!(intake.triaged_of("/tenants/beta", "r-1").expect("triaged").escalation.is_some());

        let mut escalated = vec![update(&mut acme).await, update(&mut acme).await];
        escalated.sort_by(|a, b| a.report_id.cmp(&b.report_id));
        let got: Vec<(&str, u64, i32)> = escalated.iter().map(|u| (u.report_id.as_str(), u.seq, u.stage)).collect();
        assert_eq!(got, [("r-1", 4, Stage::Escalated as i32), ("r-2", 4, Stage::Escalated as i32)]);
    }
}
