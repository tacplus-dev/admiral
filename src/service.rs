// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Admiral's lifecycle handler. Empty until the Skipper-Admiral API (#3) gives it a surface to host.

use admiral_model::s2sm::handler::Handler;

/// AdmiralHandler holds nothing: no state client, no token handle, no tenant data and no cluster
/// credential. The ServiceManager serves the generated config surface; the Admiral's own comes with #3.
#[derive(Default)]
pub struct AdmiralHandler;

impl AdmiralHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Handler for AdmiralHandler {
    async fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::AdmiralHandler;
    use admiral_model::s2sm::handler::Handler;
    use admiral_model::types::sys::healthreport::healthreport::InstanceStatus;

    #[test]
    fn an_empty_admiral_reports_healthy() {
        assert_eq!(AdmiralHandler::new().check_health(), InstanceStatus::Healthy);
    }

    #[tokio::test]
    async fn an_empty_admiral_shuts_down() {
        AdmiralHandler::new().shutdown().await;
    }

    #[test]
    fn it_hosts_no_surface_of_its_own_yet() {
        assert!(AdmiralHandler::new().additional_services().is_none());
    }
}
