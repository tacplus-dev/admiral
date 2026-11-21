// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! A forge's REST API on a local port, for the HTTP clients' tests (`github`, `gitlab`): it records each request,
//! with the trace it carried, and answers it as the test says.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::filing::forge::{Credential, ForgeError};
use crate::filing::github::TRACE_HEADER;

/// The bearer token the fake credential hands the clients.
pub const TOKEN: &str = "forge-token";

/// A credential that is always `TOKEN`.
pub struct Token;

#[async_trait::async_trait]
impl Credential for Token {
    async fn bearer(&self) -> Result<String, ForgeError> {
        Ok(TOKEN.to_string())
    }
}

/// One request the fake API answered.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    /// The path with its query.
    pub path: String,
    pub authorization: String,
    /// Its `X-Tacplus-Trace-Id`, empty when it carried none.
    pub trace_id: String,
    pub body: String,
}

type Answer = Arc<dyn Fn(&Seen) -> Value + Send + Sync>;

pub struct Api {
    pub base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Api {
    /// Serves on a local port, answering each request with `answer`'s JSON.
    pub async fn start(answer: impl Fn(&Seen) -> Value + Send + Sync + 'static) -> Api {
        let answer: Answer = Arc::new(answer);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (log, answer) = (log.clone(), answer.clone());
                tokio::spawn(async move {
                    let Some(request) = read_request(&mut stream).await else { return };
                    let body = answer(&request).to_string();
                    log.lock().unwrap().push(request);
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n";
                    let response = format!("{head}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Api { base, seen }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
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
    Some(Seen { method, path, authorization: header("authorization"), trace_id: header(TRACE_HEADER), body })
}
