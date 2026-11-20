// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! The filing core's credentials (#5, option A, Jody 2026-11-20): no secret in the Admiral's config, no key in its
//! hands. Its kernel-signed external token (`ServiceManager::token_for_external`, sub = its SPIFFE id) is exchanged
//! through GCP workload identity federation (STS) for one service account's access token, and with it:
//!
//! - **the public tracker's:** Cloud KMS signs the `tacplus-admiral` GitHub App's JWT with the App's private key,
//!   imported non-exportable, and GitHub answers an installation token (an hour);
//! - **the triage project's:** Secret Manager holds a `seabeam/triage` project access token, read when it is needed.
//!
//! The deployment's Config names the provider, the account, the key version, the App and the secret
//! (tacplus-dev/cloud#84 makes them): names, never secret values. Every token is cached until shortly before it
//! expires and shared across reports, so the calls that mint one carry no report's trace.

use std::sync::{Arc, Mutex, RwLock};

use admiral_model::s2sm::ServiceManager;
use admiral_model::types::services::admiral::Config;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::filing::forge::{Credential, ForgeError};
use crate::filing::now_ms;
use crate::filing::render::parse_time;

pub const STS: &str = "https://sts.googleapis.com";
pub const IAM_CREDENTIALS: &str = "https://iamcredentials.googleapis.com";
pub const KMS: &str = "https://cloudkms.googleapis.com";
pub const SECRET_MANAGER: &str = "https://secretmanager.googleapis.com";

const CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";
/// A token is renewed this long before it expires.
const RENEW_BEFORE_MS: i64 = 5 * 60_000;
/// How long the triage project's token is held before it is read again (it rotates by hand).
const SECRET_FOR_MS: i64 = 3_600_000;

/// The names Config gives the credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// `projects/<number>/locations/global/workloadIdentityPools/<pool>/providers/<provider>`.
    pub wif_provider: String,
    /// The audience of the Admiral's token the provider accepts.
    pub wif_audience: String,
    /// The service account's email.
    pub service_account: String,
    /// The App key's version: `projects/<p>/locations/<l>/keyRings/<r>/cryptoKeys/<k>/cryptoKeyVersions/<v>`.
    pub github_app_key: String,
    pub github_app_id: u64,
    pub github_installation_id: u64,
    /// The triage project's token: `projects/<p>/secrets/<s>/versions/<v>` (`latest` when Config names no version).
    pub triage_token_secret: String,
}

impl Sources {
    /// The sources Config names: none when it names none (the Admiral files nothing), an error when it names some
    /// but not all, or a name that is not one.
    pub fn from_config(cfg: &Config) -> Result<Option<Sources>, String> {
        let names = [
            ("wif_provider", &cfg.wif_provider),
            ("service_account", &cfg.service_account),
            ("github_app_key", &cfg.github_app_key),
            ("triage_token_secret", &cfg.triage_token_secret),
        ];
        let missing: Vec<&str> = names.iter().filter(|(_, v)| v.is_empty()).map(|(n, _)| *n).collect();
        if missing.len() == names.len() {
            return Ok(None);
        }
        if !missing.is_empty() {
            return Err(format!("the filing core's credentials name some sources but not {missing:?}"));
        }
        // A secret named without a version (tacplus-dev/cloud#84's output) is read at its latest.
        let triage_token_secret = if cfg.triage_token_secret.contains("/versions/") {
            cfg.triage_token_secret.clone()
        } else {
            format!("{}/versions/latest", cfg.triage_token_secret)
        };
        let shapes = [
            ("wif_provider", &cfg.wif_provider, ["projects/", "/workloadIdentityPools/", "/providers/"]),
            ("github_app_key", &cfg.github_app_key, ["projects/", "/cryptoKeys/", "/cryptoKeyVersions/"]),
            ("triage_token_secret", &triage_token_secret, ["projects/", "/secrets/", "/versions/"]),
        ];
        for (name, value, parts) in shapes {
            let shaped = value.starts_with(parts[0]) && parts[1..].iter().all(|p| value.contains(p));
            if !shaped || value.contains("//") || value.ends_with('/') || !is_name(value) {
                return Err(format!("{name}: not a resource name of its kind"));
            }
        }
        if !cfg.service_account.contains('@') || !is_name(&cfg.service_account) {
            return Err("service_account: not a service account's email".to_string());
        }
        let (Ok(app), Ok(installation)) = (u64::try_from(cfg.github_app_id), u64::try_from(cfg.github_installation_id))
        else {
            return Err("github_app_id, github_installation_id: not ids".to_string());
        };
        if app == 0 || installation == 0 {
            return Err("github_app_id, github_installation_id: the App's ids are needed".to_string());
        }
        let wif_audience = if cfg.wif_audience.is_empty() {
            format!("//iam.googleapis.com/{}", cfg.wif_provider)
        } else {
            cfg.wif_audience.clone()
        };
        Ok(Some(Sources {
            wif_provider: cfg.wif_provider.clone(),
            wif_audience,
            service_account: cfg.service_account.clone(),
            github_app_key: cfg.github_app_key.clone(),
            github_app_id: app,
            github_installation_id: installation,
            triage_token_secret,
        }))
    }
}

/// A name that may go in a URL's path as it is.
fn is_name(text: &str) -> bool {
    !text.contains("..") && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_.@/".contains(c))
}

/// The Admiral's own identity, for an off-cluster audience: the generated ServiceManager's in the binary.
#[async_trait::async_trait]
pub trait ExternalIdentity: Send + Sync {
    /// A fresh kernel-signed token for `audience`; never cached.
    async fn token_for_external(&self, audience: &str) -> Result<String, ForgeError>;
}

#[async_trait::async_trait]
impl ExternalIdentity for ServiceManager {
    async fn token_for_external(&self, audience: &str) -> Result<String, ForgeError> {
        // trace: unrelated: the token it is exchanged for is cached and shared across reports.
        ServiceManager::token_for_external(self, audience)
            .await
            .map_err(|e| ForgeError(format!("the Admiral's external token: {e}")))
    }
}

/// Where the credentials' APIs are: Google's and GitHub's, or a test's.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub sts: String,
    pub iam_credentials: String,
    pub kms: String,
    pub secret_manager: String,
    pub github: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            sts: STS.to_string(),
            iam_credentials: IAM_CREDENTIALS.to_string(),
            kms: KMS.to_string(),
            secret_manager: SECRET_MANAGER.to_string(),
            github: crate::filing::github::API.to_string(),
        }
    }
}

#[derive(Clone)]
struct Cached {
    token: String,
    until_ms: i64,
}

#[derive(Default)]
struct Cache(Mutex<Option<Cached>>);

impl Cache {
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap().as_ref().filter(|c| now_ms() < c.until_ms).map(|c| c.token.clone())
    }

    fn put(&self, token: &str, until_ms: i64) {
        *self.0.lock().unwrap() = Some(Cached { token: token.to_string(), until_ms });
    }

    fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}

/// The filing core's credentials through GCP, as Config names them: `github_app` is the public tracker's, and
/// `triage_project` the private project's.
pub struct GcpCredentials {
    identity: Arc<dyn ExternalIdentity>,
    endpoints: Endpoints,
    http: reqwest::Client,
    sources: RwLock<Option<Sources>>,
    google: Cache,
    github: Cache,
    gitlab: Cache,
}

impl GcpCredentials {
    /// Credentials with no sources yet: every token fails until Config names them.
    pub fn new(identity: Arc<dyn ExternalIdentity>, endpoints: Endpoints) -> Self {
        GcpCredentials {
            identity,
            endpoints,
            http: reqwest::Client::new(),
            sources: RwLock::default(),
            google: Cache::default(),
            github: Cache::default(),
            gitlab: Cache::default(),
        }
    }

    /// Sets the sources Config names; a change drops every cached token.
    pub fn configure(&self, sources: Option<Sources>) {
        let mut current = self.sources.write().unwrap();
        if *current != sources {
            *current = sources;
            self.google.clear();
            self.github.clear();
            self.gitlab.clear();
        }
    }

    /// Whether Config names the sources.
    pub fn configured(&self) -> bool {
        self.sources.read().unwrap().is_some()
    }

    /// The tacplus-admiral GitHub App's installation token.
    pub fn github_app(self: &Arc<Self>) -> Arc<dyn Credential> {
        Arc::new(GitHubApp(self.clone()))
    }

    /// The triage project's token.
    pub fn triage_project(self: &Arc<Self>) -> Arc<dyn Credential> {
        Arc::new(TriageToken(self.clone()))
    }

    fn sources(&self) -> Result<Sources, ForgeError> {
        self.sources
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| ForgeError("the filing core's credentials are not configured (Config)".to_string()))
    }

    async fn call(&self, what: &str, request: reqwest::RequestBuilder) -> Result<Value, ForgeError> {
        let response = request
            .header("User-Agent", "tacplus-admiral")
            .send()
            .await
            .map_err(|e| ForgeError(format!("{what}: {}", e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ForgeError(format!("{what}: {status}")));
        }
        response.json().await.map_err(|e| ForgeError(format!("{what}: the answer is not JSON: {}", e.without_url())))
    }

    /// The service account's access token: the Admiral's external token through STS, then the account's.
    async fn google_token(&self, sources: &Sources) -> Result<String, ForgeError> {
        if let Some(token) = self.google.get() {
            return Ok(token);
        }
        let external = self.identity.token_for_external(&sources.wif_audience).await?;
        let audience = format!("//iam.googleapis.com/{}", sources.wif_provider);
        let exchange = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
            ("audience", audience.as_str()),
            ("scope", CLOUD_PLATFORM),
            ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
            ("subject_token", external.as_str()),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:jwt"),
        ];
        // trace: unrelated: the token is cached and shared across reports.
        let request = self.http.post(format!("{}/v1/token", self.endpoints.sts)).form(&exchange);
        let federated = self.call("sts", request).await?;
        let federated = federated["access_token"].as_str().ok_or_else(|| ForgeError("sts: no access_token".into()))?;
        let url = format!(
            "{}/v1/projects/-/serviceAccounts/{}:generateAccessToken",
            self.endpoints.iam_credentials, sources.service_account
        );
        let body = json!({"scope": [CLOUD_PLATFORM], "lifetime": "3600s"});
        // trace: unrelated: the token is cached and shared across reports.
        let account = self.call("iamcredentials", self.http.post(url).bearer_auth(federated).json(&body)).await?;
        let token = account["accessToken"]
            .as_str()
            .ok_or_else(|| ForgeError("iamcredentials: no accessToken".to_string()))?;
        let until = account["expireTime"].as_str().and_then(parse_time).unwrap_or(now_ms() + 3_600_000);
        self.google.put(token, until - RENEW_BEFORE_MS);
        Ok(token.to_string())
    }

    /// The App's JWT (RS256), signed by KMS with the App's key: GitHub's ten minutes at most, its clock allowed a
    /// minute.
    async fn app_jwt(&self, sources: &Sources) -> Result<String, ForgeError> {
        let now = now_ms() / 1000;
        let header = json!({"alg": "RS256", "typ": "JWT"});
        let claims = json!({"iat": now - 60, "exp": now + 540, "iss": sources.github_app_id.to_string()});
        let signing_input =
            format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let digest = STANDARD.encode(&Sha256::digest(signing_input.as_bytes())[..]);
        let google = self.google_token(sources).await?;
        let url = format!("{}/v1/{}:asymmetricSign", self.endpoints.kms, sources.github_app_key);
        let body = json!({"digest": {"sha256": digest}});
        // trace: unrelated: the token it signs for is cached and shared across reports.
        let signed = self.call("kms", self.http.post(url).bearer_auth(google).json(&body)).await?;
        let signature = signed["signature"]
            .as_str()
            .and_then(|s| STANDARD.decode(s).ok())
            .ok_or_else(|| ForgeError("kms: no signature".to_string()))?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    async fn installation_token(&self) -> Result<String, ForgeError> {
        if let Some(token) = self.github.get() {
            return Ok(token);
        }
        let sources = self.sources()?;
        let jwt = self.app_jwt(&sources).await?;
        let (github, installation) = (&self.endpoints.github, sources.github_installation_id);
        let url = format!("{github}/app/installations/{installation}/access_tokens");
        let request = self
            .http
            .post(url)
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        // trace: unrelated: the token is cached and shared across reports.
        let installation = self.call("github app", request).await?;
        let token =
            installation["token"].as_str().ok_or_else(|| ForgeError("github app: no installation token".to_string()))?;
        let until = installation["expires_at"].as_str().and_then(parse_time).unwrap_or(now_ms() + 3_600_000);
        self.github.put(token, until - RENEW_BEFORE_MS);
        Ok(token.to_string())
    }

    async fn triage_token(&self) -> Result<String, ForgeError> {
        if let Some(token) = self.gitlab.get() {
            return Ok(token);
        }
        let sources = self.sources()?;
        let google = self.google_token(&sources).await?;
        let url = format!("{}/v1/{}:access", self.endpoints.secret_manager, sources.triage_token_secret);
        // trace: unrelated: the token is cached and shared across reports.
        let secret = self.call("secretmanager", self.http.get(url).bearer_auth(google)).await?;
        let token = secret["payload"]["data"]
            .as_str()
            .and_then(|d| STANDARD.decode(d).ok())
            .and_then(|d| String::from_utf8(d).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ForgeError("secretmanager: the secret holds no token".to_string()))?;
        self.gitlab.put(&token, now_ms() + SECRET_FOR_MS);
        Ok(token)
    }
}

struct GitHubApp(Arc<GcpCredentials>);

#[async_trait::async_trait]
impl Credential for GitHubApp {
    async fn bearer(&self) -> Result<String, ForgeError> {
        self.0.installation_token().await
    }
}

struct TriageToken(Arc<GcpCredentials>);

#[async_trait::async_trait]
impl Credential for TriageToken {
    async fn bearer(&self) -> Result<String, ForgeError> {
        self.0.triage_token().await
    }
}

#[cfg(test)]
mod tests;
