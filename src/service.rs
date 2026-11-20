// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The Admiral's lifecycle handler and its Skipper-facing surface: the Skipper channel, served by the report
//! intake (#2, src/intake.rs), which triages what it receives (#4, src/core).

use std::sync::Arc;

use admiral_model::s2sm::handler::Handler;
use admiral_model::s2sm::internal::grpc::servers::services::admiral::admiral as rpc;
use admiral_model::types::services::admiral::Config;
use servicemanager::auth::Caller;
use tonic::Status;

use admiral::core::Triage;
use admiral::filing::credentials::{GcpCredentials, Sources};
use admiral::filing::Filing;

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
/// Skippers' screened reports, in memory, and their triage, its filing core the forges' clients, and its
/// credentials the names Config gives their sources. The ServiceManager serves the generated surface: the config
/// watch and the Skipper channel.
#[derive(Default)]
pub struct AdmiralHandler {
    channel: Arc<SkipperChannel>,
    filing: Option<Arc<Filing>>,
    credentials: Option<Arc<GcpCredentials>>,
}

impl AdmiralHandler {
    /// An Admiral that holds reports and triages none.
    pub fn new() -> Self {
        Self::default()
    }

    /// An Admiral whose intake triages each report it receives and files it.
    pub fn with_filing(triage: Arc<Triage>, filing: Arc<Filing>) -> Self {
        let intake = Arc::new(Intake::with_filing(triage, filing.clone()));
        Self { channel: Arc::new(SkipperChannel { intake }), filing: Some(filing), credentials: None }
    }

    /// The filing core's forges' credentials, whose sources Config names.
    pub fn with_credentials(self, credentials: Arc<GcpCredentials>) -> Self {
        Self { credentials: Some(credentials), ..self }
    }
}

#[async_trait::async_trait]
impl Handler for AdmiralHandler {
    fn admiral_grpc_impl(&self) -> Arc<dyn rpc::AdmiralHandler> {
        self.channel.clone()
    }

    // The filing core's credentials' sources, all named or none (a config naming some, or a name that is not one,
    // is refused and changes nothing), and its allow-list: the tacplus-dev repositories a platform cause may be filed
    // in, in public. A name that is not one is refused, and the config with it.
    async fn admiral_config(&self, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
        let sources = Sources::from_config(cfg)?;
        if let Some(credentials) = &self.credentials {
            credentials.configure(sources);
        }
        let Some(filing) = &self.filing else { return Ok(()) };
        let dropped = filing.allow_public(cfg.public_repositories.iter().cloned());
        if !dropped.is_empty() {
            return Err(format!("public_repositories: not tacplus-dev repositories: {dropped:?}").into());
        }
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
    use admiral::core::Triage;
    use admiral::filing::credentials::{Endpoints, ExternalIdentity, GcpCredentials};
    use admiral::filing::forge::ForgeError;
    use admiral::filing::memory::{MemoryProject, MemoryTracker};
    use admiral::filing::Filing;
    use admiral_model::types::services::admiral::Config;
    use std::sync::Arc;
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

    #[tokio::test]
    async fn its_config_names_the_public_repositories_and_only_tacplus_devs() {
        let filing = Arc::new(Filing::new(Arc::new(MemoryProject::default()), Arc::new(MemoryTracker::default())));
        let handler = AdmiralHandler::with_filing(Arc::new(Triage::without_model()), filing);
        let cfg = Config { public_repositories: vec!["tacplus-dev/gateway".to_string()], ..Default::default() };
        handler.admiral_config(&cfg).await.expect("config applied");
        let cfg = Config { public_repositories: vec!["someone/gateway".to_string()], ..Default::default() };
        assert!(handler.admiral_config(&cfg).await.is_err());
    }

    struct NoKernel;

    #[async_trait::async_trait]
    impl ExternalIdentity for NoKernel {
        async fn token_for_external(&self, _audience: &str) -> Result<String, ForgeError> {
            Err(ForgeError("no kernel in this test".to_string()))
        }
    }

    #[tokio::test]
    async fn its_config_names_the_credentials_sources_all_or_none() {
        let credentials = Arc::new(GcpCredentials::new(Arc::new(NoKernel), Endpoints::default()));
        let filing = Arc::new(Filing::new(Arc::new(MemoryProject::default()), Arc::new(MemoryTracker::default())));
        let triage = Arc::new(Triage::without_model());
        let handler = AdmiralHandler::with_filing(triage, filing).with_credentials(credentials.clone());
        let named = Config {
            wif_provider: "projects/1/locations/global/workloadIdentityPools/p/providers/k".to_string(),
            service_account: "admiral@tacplus-ci.iam.gserviceaccount.com".to_string(),
            github_app_key: "projects/c/locations/global/keyRings/r/cryptoKeys/k/cryptoKeyVersions/1".to_string(),
            github_app_id: 5853599,
            github_installation_id: 184144753,
            triage_token_secret: "projects/c/secrets/s/versions/latest".to_string(),
            ..Default::default()
        };
        handler.admiral_config(&named).await.expect("config applied");
        assert!(credentials.configured());
        // Some names but not all: refused, and the sources stay as they were.
        let some = Config { triage_token_secret: String::new(), ..named.clone() };
        assert!(handler.admiral_config(&some).await.is_err());
        assert!(credentials.configured());
        // None: the Admiral files nothing.
        let none = Config { public_repositories: vec!["tacplus-dev/gateway".to_string()], ..Default::default() };
        handler.admiral_config(&none).await.expect("config applied");
        assert!(!credentials.configured());
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
