// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Admiral's lifecycle handler and its Skipper-facing surface: the Skipper channel, served by the report
//! intake (#2, src/intake.rs).

use std::sync::Arc;

use admiral_model::s2sm::handler::Handler;
use admiral_model::s2sm::internal::grpc::servers::services::admiral::admiral as rpc;
use admiral_model::types::services::admiral::Config;
use servicemanager::auth::Caller;
use tonic::Status;

use crate::intake::{AdmiralStream, Intake, Skipper, SkipperStream};

/// SkipperChannel serves BrokerSkipperChannel, the one stream each Skipper opens and holds to its Admiral. It
/// makes no call of its own, so there is no trace_id to carry on (looper#51).
#[derive(Default)]
pub struct SkipperChannel {
    intake: Arc<Intake>,
}

#[async_trait::async_trait]
impl rpc::AdmiralHandler for SkipperChannel {
    async fn broker_skipper_channel(
        &self,
        caller: Option<&Caller>,
        req: tonic::Request<SkipperStream>,
    ) -> Result<tonic::Response<AdmiralStream>, Status> {
        let skipper = Skipper::from_caller(caller)?;
        Ok(tonic::Response::new(self.intake.serve(skipper, req.into_inner())))
    }
}

/// AdmiralHandler holds no state client, no token handle and no cluster credential; its intake holds the
/// Skippers' screened reports, in memory. The ServiceManager serves the generated surface: the config watch and
/// the Skipper channel.
#[derive(Default)]
pub struct AdmiralHandler {
    channel: Arc<SkipperChannel>,
}

impl AdmiralHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Handler for AdmiralHandler {
    fn admiral_grpc_impl(&self) -> Arc<dyn rpc::AdmiralHandler> {
        self.channel.clone()
    }

    // Config has no fields yet (the filing core, #5, gives it some).
    async fn admiral_config(&self, _cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    async fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::rpc::AdmiralHandler as _;
    use super::{AdmiralHandler, SkipperStream};
    use admiral_model::s2sm::handler::Handler;
    use admiral_model::types::sys::healthreport::healthreport::InstanceStatus;
    use admiral_model::types::services::admiral::Config;
    use tonic::Code;

    #[test]
    fn an_empty_admiral_reports_healthy() {
        assert_eq!(AdmiralHandler::new().check_health(), InstanceStatus::Healthy);
    }

    #[tokio::test]
    async fn an_empty_admiral_shuts_down() {
        AdmiralHandler::new().shutdown().await;
    }

    #[test]
    fn it_hosts_no_surface_beyond_the_generated_one() {
        assert!(AdmiralHandler::new().additional_services().is_none());
    }

    #[tokio::test]
    async fn it_takes_its_config() {
        AdmiralHandler::new().admiral_config(&Config::default()).await.expect("config applied");
    }

    // The intake's own tests (src/intake.rs) serve the stream; here, the channel takes no stream without the
    // Skipper's verified token.
    #[tokio::test]
    async fn the_channel_needs_the_skippers_token() {
        let channel = AdmiralHandler::new().admiral_grpc_impl();
        let req = tonic::Request::new(Box::pin(futures::stream::empty()) as SkipperStream);
        let err = channel.broker_skipper_channel(None, req).await.err().expect("refused");
        assert_eq!(err.code(), Code::Unauthenticated);
    }
}
