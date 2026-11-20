// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod api_tests;
mod intake;
mod screen;
mod service;

use std::error::Error;
use std::sync::Arc;

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
    sm.run(Arc::new(service::AdmiralHandler::new())).await
}
