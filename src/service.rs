// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Admiral's lifecycle handler and its Skipper-facing surface. The API is declared (#3, the stream's typed
//! messages); serving it, report intake, is #2. Until then the stream refuses every opening.

use std::pin::Pin;
use std::sync::Arc;

use admiral_model::s2sm::handler::Handler;
use admiral_model::s2sm::internal::grpc::servers::services::admiral::admiral as rpc;
use admiral_model::types::services::admiral::{AdmiralMessage, Config, SkipperMessage};
use servicemanager::auth::Caller;
use tonic::Status;

type SkipperStream = Pin<Box<dyn futures::Stream<Item = Result<SkipperMessage, Status>> + Send>>;
type AdmiralStream = Pin<Box<dyn futures::Stream<Item = Result<AdmiralMessage, Status>> + Send>>;

/// SkipperChannel serves BrokerSkipperChannel, the one stream each Skipper opens and holds to its Admiral. It
/// makes no call of its own, so there is no trace_id to carry on (looper#51).
#[derive(Default)]
pub struct SkipperChannel;

#[async_trait::async_trait]
impl rpc::AdmiralHandler for SkipperChannel {
    async fn broker_skipper_channel(
        &self,
        _caller: Option<&Caller>,
        _req: tonic::Request<SkipperStream>,
    ) -> Result<tonic::Response<AdmiralStream>, Status> {
        // UNIMPLEMENTED, not an empty stream: a Skipper backs off and reconnects, and keeps its reports until an
        // Admiral accepts them.
        Err(Status::unimplemented("the Admiral does not take reports yet (report intake, admiral#2)"))
    }
}

/// AdmiralHandler holds nothing: no state client, no token handle, no tenant data and no cluster
/// credential. The ServiceManager serves the generated surface: the config watch and the Skipper channel.
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

    // Config has no fields yet (intake, #2, and the filing core, #5, give it some).
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

    // Until intake (#2) serves it, the stream refuses at once, so a Skipper backs off and keeps its reports.
    #[tokio::test]
    async fn the_channel_refuses_until_intake_serves_it() {
        let channel = AdmiralHandler::new().admiral_grpc_impl();
        let req = tonic::Request::new(Box::pin(futures::stream::empty()) as SkipperStream);
        let err = channel.broker_skipper_channel(None, req).await.err().expect("refused");
        assert_eq!(err.code(), Code::Unimplemented);
    }
}
