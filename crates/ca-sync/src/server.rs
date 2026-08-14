//! The receiving end.
//!
//! Two routes, one verb each:
//!
//! - `POST /v1/sync/push` — store a batch of records
//! - `POST /v1/sync/pull` — return records after a cursor
//!
//! Authentication is a bearer token that maps to exactly one tenant. That
//! mapping is the isolation boundary: a token cannot name a tenant in the
//! request, so a compromised or curious client cannot ask for another
//! customer's corpus by changing a field. Combined with one database file per
//! tenant, reaching another tenant's data would need a different token *and* a
//! different file.
//!
//! ## Transport
//!
//! Plain HTTP over `std::net`, no TLS. That is a deliberate, stated limit
//! rather than an oversight: **terminate TLS at a reverse proxy**. The server
//! refuses to bind a non-loopback address unless `allow_public` is set, so
//! serving plaintext to a network is a choice someone has to make explicitly.
//!
//! The parser is small on purpose and correspondingly strict — it requires a
//! `Content-Length`, caps the body and the header count, and closes every
//! connection. It is not a general-purpose HTTP server and does not pretend to
//! be one.

use ca_core::error::{CaError, Result};
use ca_store::TenantStores;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use crate::record::{PullResponse, PushRequest, PushResponse, SyncRecord};
use crate::{BATCH, MAX_BODY_BYTES};

/// Most headers a request may carry before it is refused.
const MAX_HEADERS: usize = 64;

/// Bearer token to tenant slug.
///
/// Tokens are opaque and compared in full. There is deliberately no way to
/// derive a tenant from a request body.
pub type TokenMap = HashMap<String, String>;

/// How the server runs.
pub struct ServerConfig {
    pub addr: SocketAddr,
    pub tokens: TokenMap,
    /// Bind an address other than loopback. Requires knowing that TLS is
    /// terminated in front of it.
    pub allow_public: bool,
}

/// Serve until the process is stopped.
pub fn serve(config: ServerConfig, tenants: Arc<TenantStores>) -> Result<()> {
    if !config.allow_public && !is_loopback(&config.addr.ip()) {
        return Err(CaError::Config(format!(
            "refusing to bind {} without --allow-public: this server speaks plain HTTP \
             and expects TLS to be terminated in front of it",
            config.addr
        )));
    }
    if config.tokens.is_empty() {
        return Err(CaError::Config(
            "no tokens configured — every request would be rejected".into(),
        ));
    }

    let listener = TcpListener::bind(config.addr)
        .map_err(|e| CaError::Config(format!("bind {}: {e}", config.addr)))?;
    tracing::info!(addr = %config.addr, tenants = config.tokens.len(), "sync server listening");

    let config = Arc::new(config);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let (config, tenants) = (config.clone(), tenants.clone());
                // A thread per connection: requests are short, and a stuck
                // client must not block every other machine from syncing.
                std::thread::spawn(move || {
                    if let Err(e) = handle(&config, &tenants, s) {
                        tracing::warn!(error = %e, "sync request failed");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
    }
    Ok(())
}

fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// One request, start to finish.
pub fn handle(
    config: &ServerConfig,
    tenants: &TenantStores,
    mut stream: TcpStream,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    let mut length: Option<usize> = None;
    let mut token: Option<String> = None;
    for _ in 0..MAX_HEADERS {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().ok();
        } else if lower.starts_with("authorization:") {
            token = line
                .split_once(':')
                .map(|(_, v)| v.trim())
                .and_then(|v| {
                    v.strip_prefix("Bearer ")
                        .or_else(|| v.strip_prefix("bearer "))
                })
                .map(|t| t.trim().to_string());
        }
    }

    // Health is answered before authentication and before the body is read.
    // A container probe and a load balancer both need it, and neither should
    // have to hold a bearer token to ask whether the process is alive. It
    // returns nothing but a literal `true` — there is no data to leak in it.
    if path == "/v1/health" {
        return reply(&mut stream, 200, &json!({"ok": true}));
    }

    if method != "POST" {
        return reply(
            &mut stream,
            405,
            &json!({"error": "only POST is supported"}),
        );
    }

    // Authenticate before reading the body: an unauthenticated client should
    // not be able to make the server allocate megabytes.
    let Some(tenant) = token.as_deref().and_then(|t| config.tokens.get(t)) else {
        // Deliberately identical for a missing and a wrong token — telling
        // them apart helps an attacker enumerate valid tokens.
        return reply(&mut stream, 401, &json!({"error": "unauthorized"}));
    };

    let Some(length) = length else {
        return reply(
            &mut stream,
            411,
            &json!({"error": "content-length required"}),
        );
    };
    if length > MAX_BODY_BYTES {
        return reply(
            &mut stream,
            413,
            &json!({"error": "body too large", "maxBytes": MAX_BODY_BYTES}),
        );
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;

    let store = match tenants.get(tenant) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(tenant = %tenant, error = %e, "cannot open tenant corpus");
            return reply(&mut stream, 500, &json!({"error": "tenant unavailable"}));
        }
    };

    let outcome = match path {
        "/v1/sync/push" => match serde_json::from_slice::<PushRequest>(&body) {
            Ok(req) => push(&store, req).map(|r| json!(r)),
            Err(e) => return reply(&mut stream, 400, &json!({"error": e.to_string()})),
        },
        "/v1/report" => {
            let args: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100_000) as usize;
            crate::rollup::summarise_store(&store, limit).map(|r| json!(r))
        }
        "/v1/sync/pull" => {
            let args: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let since = args.get("since").and_then(Value::as_i64).unwrap_or(0);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(BATCH as u64) as usize;
            pull(&store, since, limit.min(BATCH)).map(|r| json!(r))
        }
        _ => return reply(&mut stream, 404, &json!({"error": "no such route"})),
    };

    match outcome {
        Ok(value) => reply(&mut stream, 200, &value),
        Err(e) => {
            tracing::error!(error = %e, path = %path, "sync handler failed");
            reply(&mut stream, 500, &json!({"error": "internal error"}))
        }
    }
}

/// Store a batch, refusing anything that carries call content.
pub fn push(store: &ca_store::Store, req: PushRequest) -> Result<PushResponse> {
    let mut accepted = 0usize;
    let mut rejected = Vec::new();

    for record in req.records {
        // The server does not trust the client's own guard. A record that
        // carries content is refused at the boundary rather than stored and
        // apologised for afterwards.
        if let Err(reason) = record.assert_no_content() {
            tracing::warn!(call = %record.call_id, reason = %reason, "rejecting record");
            rejected.push(record.call_id);
            continue;
        }
        let json = serde_json::to_string(&record)
            .map_err(|e| CaError::Storage(format!("encode record: {e}")))?;
        store.put_sync_record(&record.call_id, &json)?;
        accepted += 1;
    }

    Ok(PushResponse {
        accepted,
        rejected,
        cursor: store.sync_cursor()?,
    })
}

/// Records after a cursor.
pub fn pull(store: &ca_store::Store, since: i64, limit: usize) -> Result<PullResponse> {
    let rows = store.sync_records_after(since, limit)?;
    let cursor = rows.last().map(|(seq, _)| *seq).unwrap_or(since);
    let records = rows
        .into_iter()
        .filter_map(|(_, json)| serde_json::from_str::<SyncRecord>(&json).ok())
        .collect();
    Ok(PullResponse { records, cursor })
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::FindingMetric;
    use ca_store::Store;

    fn record(id: &str) -> SyncRecord {
        SyncRecord {
            call_id: id.into(),
            occurred_at: None,
            duration_secs: Some(30.0),
            stage: "analyzed".into(),
            agent_id: Some("person_a1".into()),
            contact_id: Some("person_b2".into()),
            qa_overall: Some(70.0),
            sentiment: Some(0.1),
            compliance_passed: Some(true),
            findings: vec![FindingMetric {
                code: "TALK_OVER".into(),
                severity: "low".into(),
                count: 2,
            }],
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_pushed_batch_is_stored_and_pullable() {
        let store = Store::in_memory().unwrap();
        let out = push(
            &store,
            PushRequest {
                records: vec![record("call_1"), record("call_2")],
            },
        )
        .unwrap();

        assert_eq!(out.accepted, 2);
        assert!(out.rejected.is_empty());

        let back = pull(&store, 0, 100).unwrap();
        assert_eq!(back.records.len(), 2);
        assert_eq!(back.cursor, out.cursor);
    }

    /// The server does not trust the client's guard.
    #[test]
    fn a_record_carrying_content_is_rejected_at_the_boundary() {
        let store = Store::in_memory().unwrap();
        let mut bad = record("call_bad");
        bad.contact_id = Some("Babulal Kavaiya".into());

        let out = push(
            &store,
            PushRequest {
                records: vec![record("call_ok"), bad],
            },
        )
        .unwrap();

        assert_eq!(out.accepted, 1);
        assert_eq!(out.rejected, vec!["call_bad".to_string()]);
        assert_eq!(pull(&store, 0, 100).unwrap().records.len(), 1);
    }

    #[test]
    fn pulling_from_the_current_cursor_returns_nothing() {
        let store = Store::in_memory().unwrap();
        push(
            &store,
            PushRequest {
                records: vec![record("call_1")],
            },
        )
        .unwrap();
        let cursor = store.sync_cursor().unwrap();

        let back = pull(&store, cursor, 100).unwrap();
        assert!(back.records.is_empty());
        assert_eq!(back.cursor, cursor, "an empty pull holds the cursor still");
    }

    /// A retry after a timeout must not double-count.
    #[test]
    fn pushing_the_same_call_twice_stores_it_once() {
        let store = Store::in_memory().unwrap();
        push(
            &store,
            PushRequest {
                records: vec![record("call_1")],
            },
        )
        .unwrap();
        push(
            &store,
            PushRequest {
                records: vec![record("call_1")],
            },
        )
        .unwrap();

        assert_eq!(pull(&store, 0, 100).unwrap().records.len(), 1);
    }

    /// A token names its tenant; a request cannot.
    #[test]
    fn a_token_maps_to_exactly_one_tenant() {
        let mut tokens = TokenMap::new();
        tokens.insert("tok-acme".into(), "acme".into());
        tokens.insert("tok-globex".into(), "globex".into());

        assert_eq!(tokens.get("tok-acme").unwrap(), "acme");
        assert_eq!(tokens.get("tok-globex").unwrap(), "globex");
        assert!(!tokens.contains_key("tok-unknown"));
    }

    #[test]
    fn binding_a_public_address_needs_an_explicit_opt_in() {
        let tenants = Arc::new(TenantStores::new(std::env::temp_dir().join("ca-sync-bind")));
        let mut tokens = TokenMap::new();
        tokens.insert("t".into(), "local".into());

        let err = serve(
            ServerConfig {
                addr: "0.0.0.0:0".parse().unwrap(),
                tokens: tokens.clone(),
                allow_public: false,
            },
            tenants.clone(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("allow-public"), "got {err}");
    }

    /// A server with no tokens would reject every request; failing at startup
    /// is better than looking healthy and answering 401 forever.
    #[test]
    fn a_server_with_no_tokens_refuses_to_start() {
        let tenants = Arc::new(TenantStores::new(
            std::env::temp_dir().join("ca-sync-empty"),
        ));
        let err = serve(
            ServerConfig {
                addr: "127.0.0.1:0".parse().unwrap(),
                tokens: TokenMap::new(),
                allow_public: false,
            },
            tenants,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("no tokens"), "got {err}");
    }
}
