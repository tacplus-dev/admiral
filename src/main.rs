// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod api_tests;
mod intake;
mod screen;
mod service;

use std::error::Error;
use std::sync::Arc;

use admiral::core::llmproxy::LlmProxyModel;
use admiral::core::proposal::PRIVATE_TRIAGE;
use admiral::core::Triage;
use admiral::filing::credentials::{Endpoints, GcpCredentials};
use admiral::filing::github::{self, GitHub};
use admiral::filing::gitlab::{self, GitLab};
use admiral::filing::{Filing, SWEEP_EVERY};
use admiral_model::s2sm::ServiceManager;

#[tokio::main]
async fn main() {
    // The chassis logger, NOT tracing_subscriber::fmt(): it redacts. Untrusted text (Skippers' messages,
    // users' reports) arrives here, and an unredacted log line is where a secret in one would leave.
    if let Err(e) = logger::install_bootstrap() {
        eprintln!("FATAL: could not install the redacting logger: {e}");
        std::process::exit(1);
    }

    if let Err(e) = run().await {
        tracing::error!(error = %e, "admiral failed");
        let mut source = e.source();
        while let Some(cause) = source {
            tracing::error!(cause = %cause, "caused by");
            source = cause.source();
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    // ServiceManager::new() reads BOOTSTRAP_CONFIG, connects to kernel over mTLS, and returns once the
    // service has its /system identity and config. sm.run() then serves the generated gRPC surface and
    // blocks until shutdown.
    let sm = Arc::new(ServiceManager::new().await?);
    // Triage's core holds the ServiceManager, and with it the llmproxy client; its advisor reaches the model through
    // it (Opus, and Fable for hard triage) and gets nothing else (src/advisor, scripts/llm-rule-gate.sh).
    let triage = Arc::new(Triage::with_model(Arc::new(LlmProxyModel::new(sm.clone()))));
    // The filing core (#5, admiral::filing) alone holds the forges' clients, and their credentials are minted from
    // the Admiral's own identity through GCP, from the sources Config names (src/filing/credentials.rs). Until it
    // names them, every filing fails before any forge call and the report stays TRIAGED.
    let credentials = Arc::new(GcpCredentials::new(sm.clone(), Endpoints::default()));
    let project = GitLab::new(gitlab::API, PRIVATE_TRIAGE, credentials.triage_project());
    let tracker = GitHub::new(github::API, credentials.github_app());
    let filing = Arc::new(Filing::new(Arc::new(project), Arc::new(tracker)));
    // trace: unrelated: the retention sweep runs on a timer, caused by no request.
    tokio::spawn(filing.clone().sweep_every(SWEEP_EVERY));
    let handler = service::AdmiralHandler::with_filing(triage, filing).with_credentials(credentials);
    sm.run(Arc::new(handler)).await
}
