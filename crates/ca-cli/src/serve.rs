//! A development bridge between the browser and the real database.
//!
//! `npm run dev` runs the UI in a plain browser, where there is no Tauri
//! runtime and therefore no `invoke`. Until now the answer was a file of
//! invented fixtures, which is fine for checking a layout and useless for
//! everything else: you cannot see how a screen behaves against 1,972 calls
//! and 771 contacts by looking at nine hand-written rows.
//!
//! This serves the **real** corpus over loopback so the dev UI shows real
//! data. It is not part of the shipped app — the desktop binary talks to the
//! same `Store` directly, in-process, and never over a socket.
//!
//! Deliberately hand-rolled over `std::net`: this needs one route and one
//! verb, and adding a web framework to a workspace that has no server in it
//! would be a large dependency for a development convenience.
//!
//! ## Safety
//!
//! Binds `127.0.0.1` only, never `0.0.0.0`, so nothing outside this machine can
//! reach the corpus. It is still a hole in the local privacy story — anything
//! running as this user could read call data through it — so it runs only when
//! started by hand and prints what it is exposing.

use ca_core::identity::PersonKind;
use ca_core::ids::{CallId, PersonId};
use ca_store::Store;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;

/// Serve the corpus on loopback until interrupted.
pub fn serve(store: Arc<Store>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    println!("dev bridge on http://127.0.0.1:{port}  (loopback only)");
    println!("the UI at `npm run dev` will now show real data from your corpus");
    println!("press Ctrl-C to stop");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                // Serial rather than threaded: one browser tab, and a `Store`
                // behind a mutex would serialise the work anyway.
                if let Err(e) = handle(&store, s) {
                    tracing::warn!(error = %e, "dev bridge request failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "dev bridge accept failed"),
        }
    }
    Ok(())
}

fn handle(store: &Store, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }

    // The browser preflights a cross-origin POST from the vite dev server.
    if request_line.starts_with("OPTIONS") {
        return respond(&mut stream, 204, "");
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;

    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let cmd = parsed
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = parsed.get("args").cloned().unwrap_or(json!({}));

    let payload = match dispatch(store, cmd, &args) {
        Ok(Some(value)) => json!({ "ok": true, "value": value }),
        // A command this bridge does not implement is reported as such, so the
        // UI can say which screen it is faking rather than silently mixing
        // real and invented data.
        Ok(None) => json!({ "ok": false, "unsupported": true, "cmd": cmd }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    };

    respond(&mut stream, 200, &payload.to_string())
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = if status == 204 { "No Content" } else { "OK" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Headers: content-type\r\n\
         Access-Control-Allow-Methods: POST, OPTIONS\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Map a command name to the same `Store` call the desktop app makes.
///
/// `Ok(None)` means "not implemented here", which is different from an error
/// and is reported differently to the browser.
fn dispatch(store: &Store, cmd: &str, args: &Value) -> ca_core::error::Result<Option<Value>> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| args.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);

    let value = match cmd {
        "get_stats" => json!(store.stats()?),
        "list_calls" => {
            json!(store.list_calls(n("offset"), n("limit").max(1), s("search").as_deref())?)
        }
        "get_call" => json!(store.call_detail(&CallId::from(s("callId").unwrap_or_default()))?),
        "get_waveform" => json!(store.waveform(&CallId::from(s("callId").unwrap_or_default()))?),
        "get_scorecard" => json!(store.load_analysis(
            &CallId::from(s("callId").unwrap_or_default()),
            ca_core::analysis::AgentKind::QaScorecard,
        )?),
        "get_corpus_report" => {
            json!(ca_insights::analyse_corpus(&store.corpus_facts(50_000)?))
        }
        "get_findings" => json!(store.findings(&CallId::from(s("callId").unwrap_or_default()))?),
        "get_risk_register" => json!(store.findings_summary(n("limit").max(20))?),
        "list_runs" => json!(store.list_runs(n("offset"), n("limit").max(20))?),
        "get_agent_runs" => {
            json!(store.agent_runs(&CallId::from(s("callId").unwrap_or_default()))?)
        }
        "get_agent_health" => json!(store.agent_health()?),

        "list_people" => {
            let kind = s("kind").as_deref().and_then(PersonKind::parse);
            json!(store.list_people(kind, s("search").as_deref())?)
        }
        "get_person" => {
            match store.person_detail(&PersonId::from(s("personId").unwrap_or_default()))? {
                Some(d) => json!(d),
                None => return Ok(None),
            }
        }
        "get_call_people" => {
            json!(store.call_people(&CallId::from(s("callId").unwrap_or_default()))?)
        }
        "attribute_calls" => json!(store.attribute_calls(
            &ca_identity::builtin_patterns(),
            ca_store::AttributeOptions {
                dry_run: b("dryRun"),
                force: b("force"),
            },
        )?),
        "assign_call_person" => {
            let resolve = |kind: PersonKind,
                           name: Option<String>|
             -> ca_core::error::Result<Option<PersonId>> {
                let Some(name) = name.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
                else {
                    return Ok(None);
                };
                let handle = ca_identity::normalise_handle(&name);
                store.upsert_person(kind, &name, &handle).map(Some)
            };
            let agent = resolve(PersonKind::Agent, s("agentName"))?;
            let contact = resolve(PersonKind::Contact, s("contactName"))?;
            store.assign_call_people(
                &CallId::from(s("callId").unwrap_or_default()),
                agent.as_ref(),
                contact.as_ref(),
                ca_core::identity::AttributionSource::Manual,
            )?;
            Value::Null
        }

        "preview_reset" => {
            let Some(scope) = ca_store::ResetScope::parse(&s("scope").unwrap_or_default()) else {
                return Ok(None);
            };
            json!(store.reset_preview(scope)?)
        }
        "reset_data" => {
            let Some(scope) = ca_store::ResetScope::parse(&s("scope").unwrap_or_default()) else {
                return Ok(None);
            };
            let backup_path = if b("backup") {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                store.backup(&stamp)?.map(|p| p.display().to_string())
            } else {
                None
            };
            json!({ "report": store.reset(scope)?, "backupPath": backup_path })
        }

        _ => return Ok(None),
    };
    Ok(Some(value))
}
