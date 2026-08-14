//! A small HTTP client for the two sync routes.
//!
//! Hand-rolled over `std::net` for the same reason the server is: two routes,
//! one verb, no redirects, no cookies, no keep-alive. The workspace's async
//! HTTP client exists for talking to model vendors and would drag a runtime
//! into a synchronous CLI command for no benefit here.
//!
//! No TLS. The URL is expected to be a reverse proxy on the same host, or a
//! loopback address — see the module docs on [`crate::server`].

use ca_core::error::{CaError, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::client::Transport;
use crate::record::{PullResponse, PushRequest, PushResponse};

/// How long to wait on a peer before giving up.
///
/// A sync that hangs is worse than one that fails: the watermark does not
/// advance either way, but a hung one blocks the next attempt indefinitely.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A peer reachable over HTTP.
///
/// `Debug` omits the token: a transport ends up in log lines and error
/// messages, and a bearer token printed once is a token to rotate.
pub struct HttpTransport {
    host: String,
    port: u16,
    token: String,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl HttpTransport {
    /// Build from a `host:port` and a bearer token.
    pub fn new(host: impl Into<String>, port: u16, token: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            token: token.into(),
        }
    }

    /// Parse the `http://host:port` form the CLI accepts.
    pub fn parse(url: &str, token: impl Into<String>) -> Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| CaError::Config(format!("`{url}` must start with http://")))?;
        let rest = rest.trim_end_matches('/');
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| CaError::Config(format!("`{p}` is not a port")))?,
            ),
            None => (rest, 80),
        };
        if host.is_empty() {
            return Err(CaError::Config(format!("`{url}` has no host")));
        }
        Ok(Self::new(host, port, token))
    }

    fn post(&self, path: &str, body: &str) -> Result<String> {
        self.request("POST", path, body)
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Result<String> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr)
            .map_err(|e| CaError::Config(format!("connect {addr}: {e}")))?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();

        let mut stream = stream;
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\n\
             Host: {}\r\n\
             Authorization: Bearer {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            self.host,
            self.token,
            body.len()
        )
        .map_err(|e| CaError::Config(format!("write {path}: {e}")))?;
        stream
            .flush()
            .map_err(|e| CaError::Config(format!("flush {path}: {e}")))?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| CaError::Config(format!("read {path}: {e}")))?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Headers are skipped rather than parsed: the server always closes the
        // connection, so reading to EOF gives the whole body.
        loop {
            let mut line = String::new();
            if reader
                .read_line(&mut line)
                .map_err(|e| CaError::Config(format!("read {path}: {e}")))?
                == 0
                || line.trim().is_empty()
            {
                break;
            }
        }

        let mut body = String::new();
        reader
            .read_to_string(&mut body)
            .map_err(|e| CaError::Config(format!("read body {path}: {e}")))?;

        if status != 200 {
            // The peer's own message is included: "unauthorized" and "body too
            // large" need different fixes, and hiding which is unhelpful.
            return Err(CaError::Config(format!(
                "peer returned {status} for {path}: {}",
                body.trim()
            )));
        }
        Ok(body)
    }
}

impl HttpTransport {
    /// Fetch the peer's rollup of everything it holds for this tenant.
    pub fn report(&self, limit: usize) -> Result<crate::rollup::Rollup> {
        let body = serde_json::json!({ "limit": limit }).to_string();
        let response = self.post("/v1/report", &body)?;
        serde_json::from_str(&response).map_err(|e| CaError::Config(format!("decode report: {e}")))
    }

    /// Ask whether a peer is alive.
    ///
    /// Deliberately unauthenticated on both sides: a container healthcheck
    /// holding a bearer token is a token in a process list.
    pub fn health(&self) -> Result<()> {
        self.request("GET", "/v1/health", "").map(|_| ())
    }
}

impl Transport for HttpTransport {
    fn push(&self, req: &PushRequest) -> Result<PushResponse> {
        let body =
            serde_json::to_string(req).map_err(|e| CaError::Config(format!("encode push: {e}")))?;
        let response = self.post("/v1/sync/push", &body)?;
        serde_json::from_str(&response)
            .map_err(|e| CaError::Config(format!("decode push response: {e}")))
    }

    fn pull(&self, since: i64, limit: usize) -> Result<PullResponse> {
        let body = serde_json::json!({ "since": since, "limit": limit }).to_string();
        let response = self.post("/v1/sync/pull", &body)?;
        serde_json::from_str(&response)
            .map_err(|e| CaError::Config(format!("decode pull response: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_without_a_port_defaults_to_eighty() {
        let t = HttpTransport::parse("http://sync.example", "tok").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("sync.example", 80));
    }

    #[test]
    fn a_url_with_a_port_and_trailing_slash_parses() {
        let t = HttpTransport::parse("http://127.0.0.1:8480/", "tok").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("127.0.0.1", 8480));
    }

    /// https would silently not be TLS, which is worse than refusing it.
    #[test]
    fn an_https_url_is_refused_rather_than_downgraded() {
        let err = HttpTransport::parse("https://sync.example", "tok").unwrap_err();
        assert!(format!("{err}").contains("http://"), "got {err}");
    }

    /// A token in a log line is a token to rotate.
    #[test]
    fn debug_output_never_shows_the_token() {
        let t = HttpTransport::new("host", 80, "super-secret-token");
        let shown = format!("{t:?}");
        assert!(!shown.contains("super-secret-token"), "got {shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn a_malformed_url_is_reported() {
        assert!(HttpTransport::parse("http://", "tok").is_err());
        assert!(HttpTransport::parse("http://host:notaport", "tok").is_err());
    }
}
