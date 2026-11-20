// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex};

use admiral_model::types::services::admiral::Config;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{Endpoints, ExternalIdentity, GcpCredentials, Sources};
use crate::filing::forge::ForgeError;

const PROVIDER: &str = "projects/123/locations/global/workloadIdentityPools/fleet-kernels/providers/seatrials";
const ACCOUNT: &str = "admiral@tacplus-ci.iam.gserviceaccount.com";
const KEY: &str = "projects/tacplus-ci/locations/global/keyRings/looper/cryptoKeys/tacplus-admiral/cryptoKeyVersions/1";
const SECRET: &str = "projects/tacplus-ci/secrets/admiral-triage-gitlab/versions/latest";

fn config() -> Config {
    Config {
        wif_provider: PROVIDER.to_string(),
        service_account: ACCOUNT.to_string(),
        github_app_key: KEY.to_string(),
        github_app_id: 5853599,
        github_installation_id: 184144753,
        triage_token_secret: SECRET.to_string(),
        ..Default::default()
    }
}

#[test]
fn config_names_all_the_sources_or_none() {
    let none = Config { github_app_id: 5853599, github_installation_id: 184144753, ..Default::default() };
    assert_eq!(Sources::from_config(&none), Ok(None));

    let sources = Sources::from_config(&config()).unwrap().expect("sources");
    assert_eq!(sources.wif_audience, format!("//iam.googleapis.com/{PROVIDER}"));
    assert_eq!((sources.github_app_id, sources.github_installation_id), (5853599, 184144753));
    let own = Config { wif_audience: "admiral".to_string(), ..config() };
    assert_eq!(Sources::from_config(&own).unwrap().unwrap().wif_audience, "admiral");

    // tacplus-dev/cloud#84's ci stack names the secret, not a version: its latest is read.
    let secret = Config { triage_token_secret: "projects/tacplus-ci/secrets/admiral-triage-gitlab".to_string(), ..config() };
    assert_eq!(Sources::from_config(&secret).unwrap().unwrap().triage_token_secret, SECRET);

    for refused in [
        Config { triage_token_secret: String::new(), ..config() },
        Config { github_app_key: "projects/tacplus-ci/secrets/s/versions/1".to_string(), ..config() },
        Config { triage_token_secret: format!("{SECRET}?alt=media"), ..config() },
        Config { triage_token_secret: "projects/tacplus-ci/secrets/".to_string(), ..config() },
        Config { triage_token_secret: "projects/tacplus-ci/secrets/s/versions/".to_string(), ..config() },
        Config { wif_provider: format!("{PROVIDER}/../../x"), ..config() },
        Config { service_account: "admiral".to_string(), ..config() },
        Config { github_app_id: 0, ..config() },
        Config { github_installation_id: -1, ..config() },
    ] {
        assert!(Sources::from_config(&refused).is_err(), "{refused:?}");
    }
}

/// The kernel's external tokens: the audiences asked for.
#[derive(Default)]
struct Kernel {
    audiences: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ExternalIdentity for Kernel {
    async fn token_for_external(&self, audience: &str) -> Result<String, ForgeError> {
        self.audiences.lock().unwrap().push(audience.to_string());
        Ok("kernel-jwt".to_string())
    }
}

/// One request the fake APIs answered.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: String,
}

/// Google's and GitHub's APIs, as far as the credentials use them, on one local port.
struct Apis {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Answer every request with this status, when set.
    fail: Arc<Mutex<Option<u16>>>,
}

impl Apis {
    async fn start() -> Apis {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(None)));
        let (log, failing) = (seen.clone(), fail.clone());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (log, failing) = (log.clone(), failing.clone());
                tokio::spawn(async move {
                    let Some(request) = read_request(&mut stream).await else { return };
                    let failed = *failing.lock().unwrap();
                    let (status, body) = match failed {
                        Some(status) => (status, "{\"error\":\"refused\"}".to_string()),
                        None => (200, answer(&request).to_string()),
                    };
                    log.lock().unwrap().push(request);
                    let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n");
                    let response = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Apis { base, seen, fail }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            sts: self.base.clone(),
            iam_credentials: self.base.clone(),
            kms: self.base.clone(),
            secret_manager: self.base.clone(),
            github: self.base.clone(),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn calls_to(&self, suffix: &str) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.path.ends_with(suffix)).collect()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Seen> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let header = |name: &str| {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    };
    let length: usize = header("content-length").parse().unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let mut first = head.lines().next()?.split_whitespace();
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let body = String::from_utf8_lossy(&data[head_end..]).to_string();
    Some(Seen { method, path, authorization: header("authorization"), body })
}

fn answer(request: &Seen) -> Value {
    let path = request.path.as_str();
    if path == "/v1/token" {
        json!({"access_token": "federated-token", "token_type": "Bearer", "expires_in": 3600})
    } else if path.ends_with(":generateAccessToken") {
        json!({"accessToken": "account-token", "expireTime": "2099-01-01T00:00:00Z"})
    } else if path.ends_with(":asymmetricSign") {
        json!({"signature": STANDARD.encode(b"kms-signature"), "name": KEY})
    } else if path.ends_with("/access_tokens") {
        json!({"token": "installation-token", "expires_at": "2099-01-01T00:00:00Z"})
    } else if path.ends_with(":access") {
        json!({"name": SECRET, "payload": {"data": STANDARD.encode(b"triage-token\n")}})
    } else {
        json!({})
    }
}

async fn credentials(apis: &Apis) -> (Arc<GcpCredentials>, Arc<Kernel>) {
    let kernel = Arc::new(Kernel::default());
    let credentials = Arc::new(GcpCredentials::new(kernel.clone(), apis.endpoints()));
    credentials.configure(Sources::from_config(&config()).unwrap());
    (credentials, kernel)
}

#[tokio::test]
async fn the_github_apps_token_is_signed_by_kms_through_workload_identity_federation() {
    let apis = Apis::start().await;
    let (credentials, kernel) = credentials(&apis).await;

    let token = credentials.github_app().bearer().await.expect("installation token");
    assert_eq!(token, "installation-token");

    // The Admiral's own token, for the provider's audience, exchanged at STS for the provider's...
    assert_eq!(*kernel.audiences.lock().unwrap(), vec![format!("//iam.googleapis.com/{PROVIDER}")]);
    let sts = &apis.calls_to("/v1/token")[0];
    assert_eq!(sts.method, "POST");
    for field in ["subject_token=kernel-jwt", "grant-type%3Atoken-exchange", "token-type%3Ajwt"] {
        assert!(sts.body.contains(field), "{field} in {}", sts.body);
    }
    assert!(sts.body.contains("audience=%2F%2Fiam.googleapis.com%2Fprojects%2F123%2F"), "{}", sts.body);
    // ...then the service account's...
    let account = &apis.calls_to(":generateAccessToken")[0];
    assert_eq!(account.path, format!("/v1/projects/-/serviceAccounts/{ACCOUNT}:generateAccessToken"));
    assert_eq!(account.authorization, "Bearer federated-token");
    // ...with which KMS signs the App's JWT with its key...
    let sign = &apis.calls_to(":asymmetricSign")[0];
    assert_eq!(sign.path, format!("/v1/{KEY}:asymmetricSign"));
    assert_eq!(sign.authorization, "Bearer account-token");
    // ...which GitHub takes for the installation's token.
    let installation = &apis.calls_to("/access_tokens")[0];
    assert_eq!(installation.path, "/app/installations/184144753/access_tokens");
    let jwt = installation.authorization.strip_prefix("Bearer ").unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(header["alg"], "RS256");
    assert_eq!(claims["iss"], "5853599");
    let lifetime = claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap();
    assert!(lifetime <= 600, "GitHub takes a JWT of ten minutes at most: {lifetime}");
    assert_eq!(URL_SAFE_NO_PAD.decode(parts[2]).unwrap(), b"kms-signature");
    // KMS signed the digest of exactly that header and those claims.
    let sent: Value = serde_json::from_str(&sign.body).unwrap();
    let digest = STANDARD.encode(&Sha256::digest(format!("{}.{}", parts[0], parts[1]).as_bytes())[..]);
    assert_eq!(sent["digest"]["sha256"], digest.as_str());

    // The token is held until shortly before it expires: no call mints another.
    let calls = apis.seen().len();
    assert_eq!(credentials.github_app().bearer().await.unwrap(), "installation-token");
    assert_eq!(apis.seen().len(), calls);
}

#[tokio::test]
async fn the_triage_projects_token_is_read_from_secret_manager_with_the_same_account() {
    let apis = Apis::start().await;
    let (credentials, kernel) = credentials(&apis).await;

    assert_eq!(credentials.triage_project().bearer().await.expect("triage token"), "triage-token");
    let read = &apis.calls_to(":access")[0];
    assert_eq!((read.method.as_str(), read.path.clone()), ("GET", format!("/v1/{SECRET}:access")));
    assert_eq!(read.authorization, "Bearer account-token");

    // The account's token is shared: the App's token needs no second exchange.
    credentials.github_app().bearer().await.unwrap();
    assert_eq!(apis.calls_to("/v1/token").len(), 1);
    assert_eq!(kernel.audiences.lock().unwrap().len(), 1);
    // And the triage token is held too.
    credentials.triage_project().bearer().await.unwrap();
    assert_eq!(apis.calls_to(":access").len(), 1);
}

#[tokio::test]
async fn no_token_without_config_and_a_change_of_config_mints_anew() {
    let apis = Apis::start().await;
    let credentials = Arc::new(GcpCredentials::new(Arc::new(Kernel::default()), apis.endpoints()));
    assert!(!credentials.configured());
    let e = credentials.triage_project().bearer().await.unwrap_err();
    assert!(e.0.contains("not configured"), "{e}");
    assert!(apis.seen().is_empty());

    credentials.configure(Sources::from_config(&config()).unwrap());
    assert!(credentials.configured());
    credentials.triage_project().bearer().await.unwrap();
    // The same sources keep the tokens; others drop them.
    credentials.configure(Sources::from_config(&config()).unwrap());
    credentials.triage_project().bearer().await.unwrap();
    assert_eq!(apis.calls_to(":access").len(), 1);
    let other = Config { triage_token_secret: "projects/tacplus-ci/secrets/other/versions/2".to_string(), ..config() };
    credentials.configure(Sources::from_config(&other).unwrap());
    credentials.triage_project().bearer().await.unwrap();
    assert_eq!(apis.calls_to(":access").len(), 2);
    assert_eq!(apis.calls_to("/v1/token").len(), 2);
}

#[tokio::test]
async fn a_refused_exchange_fails_and_names_no_token() {
    let apis = Apis::start().await;
    let (credentials, _) = credentials(&apis).await;
    *apis.fail.lock().unwrap() = Some(403);
    let e = credentials.github_app().bearer().await.unwrap_err();
    assert!(e.0.starts_with("sts: 403"), "{e}");
    assert!(!e.0.contains("kernel-jwt"), "{e}");
    // Nothing was cached: once the exchange is allowed, the token comes.
    *apis.fail.lock().unwrap() = None;
    assert_eq!(credentials.github_app().bearer().await.unwrap(), "installation-token");
}
