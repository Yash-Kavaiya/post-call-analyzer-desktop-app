//! A real client, a real server, a real socket.
//!
//! The unit tests prove the logic; this proves the wiring — that a corpus on
//! one machine reaches a dashboard on another, that a token decides which
//! tenant is written, and that the transport carries no call content.

use ca_core::analysis::{Finding, Severity};
use ca_core::call::Call;
use ca_store::{Store, TenantStores};
use ca_sync::client::Transport;
use ca_sync::http::HttpTransport;
use ca_sync::server::{ServerConfig, TokenMap};
use ca_sync::SyncPeer;
use std::net::TcpListener;
use std::sync::Arc;

/// Bind port 0, ask the OS what it gave us, and hand the listener back.
///
/// Picking a fixed port makes a test suite fail when two runs overlap, which
/// is exactly the kind of flake that gets tests deleted.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ca-sync-e2e-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A local corpus with one analysed call that has a finding.
fn corpus() -> Arc<Store> {
    let s = Store::in_memory().unwrap();
    let id = s
        .upsert_call(&Call::new(
            "C:/calls/Call recording +918485823280_251112_142326.m4a",
            "local",
            "Call recording +918485823280_251112_142326.m4a",
        ))
        .unwrap();
    s.attribute_calls(&ca_identity::builtin_patterns(), Default::default())
        .unwrap();
    s.save_findings(
        &id,
        &[Finding {
            id: ca_core::ids::FindingId::new(),
            plugin: None,
            call_id: id.clone(),
            agent: ca_core::AgentKind::Compliance,
            code: "PCI_PAN_SPOKEN".into(),
            title: "A card number was read aloud".into(),
            detail: "The customer read 4111 1111 1111 1111 at 2:14.".into(),
            severity: Severity::Critical,
            citations: vec![],
        }],
    )
    .unwrap();
    Arc::new(s)
}

fn start_server(dir: &std::path::Path, port: u16, tokens: TokenMap) -> Arc<TenantStores> {
    let tenants = Arc::new(TenantStores::new(dir));
    let serving = tenants.clone();
    std::thread::spawn(move || {
        let _ = ca_sync::server::serve(
            ServerConfig {
                addr: format!("127.0.0.1:{port}").parse().unwrap(),
                tokens,
                allow_public: false,
            },
            serving,
        );
    });
    // The listener binds before the first request; a short wait beats a
    // retry loop that hides a genuine bind failure.
    std::thread::sleep(std::time::Duration::from_millis(200));
    tenants
}

#[test]
fn a_corpus_reaches_the_server_carrying_metrics_but_no_content() {
    let dir = scratch("round-trip");
    let port = free_port();
    let mut tokens = TokenMap::new();
    tokens.insert("tok-acme".into(), "acme".into());
    let tenants = start_server(&dir, port, tokens);

    let local = corpus();
    let peer = SyncPeer::new(
        local.clone(),
        "hq",
        Box::new(HttpTransport::new("127.0.0.1", port, "tok-acme")),
    );

    let outcome = peer.sync().unwrap();
    assert_eq!(outcome.pushed, 1, "the call was sent");
    assert_eq!(outcome.withheld, 0);
    assert_eq!(outcome.rejected, 0);

    // The server wrote it into the tenant the token names.
    let acme = tenants.get("acme").unwrap();
    let stored = acme.sync_records_after(0, 100).unwrap();
    assert_eq!(stored.len(), 1);

    let json = &stored[0].1;
    assert!(json.contains("PCI_PAN_SPOKEN"), "the finding code travels");
    for leaked in [
        "4111 1111 1111 1111",
        "918485823280",
        "A card number was read aloud",
        "Call recording",
    ] {
        assert!(!json.contains(leaked), "`{leaked}` reached the server");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_sync_sends_nothing_and_is_harmless() {
    let dir = scratch("idempotent");
    let port = free_port();
    let mut tokens = TokenMap::new();
    tokens.insert("tok".into(), "acme".into());
    let tenants = start_server(&dir, port, tokens);

    let local = corpus();
    let peer = SyncPeer::new(
        local,
        "hq",
        Box::new(HttpTransport::new("127.0.0.1", port, "tok")),
    );

    assert_eq!(peer.sync().unwrap().pushed, 1);
    assert_eq!(peer.sync().unwrap().pushed, 0, "nothing changed since");
    assert_eq!(
        tenants
            .get("acme")
            .unwrap()
            .sync_records_after(0, 100)
            .unwrap()
            .len(),
        1,
        "one row per call, however many times it syncs"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The isolation boundary: the token decides the tenant, the request cannot.
#[test]
fn two_tokens_write_to_two_separate_corpora() {
    let dir = scratch("tenants");
    let port = free_port();
    let mut tokens = TokenMap::new();
    tokens.insert("tok-acme".into(), "acme".into());
    tokens.insert("tok-globex".into(), "globex".into());
    let tenants = start_server(&dir, port, tokens);

    for (token, key) in [("tok-acme", "acme-call"), ("tok-globex", "globex-call")] {
        let store = Store::in_memory().unwrap();
        store
            .upsert_call(&Call::new(key, "local", format!("{key}.m4a")))
            .unwrap();
        SyncPeer::new(
            Arc::new(store),
            "hq",
            Box::new(HttpTransport::new("127.0.0.1", port, token)),
        )
        .sync()
        .unwrap();
    }

    let acme = tenants
        .get("acme")
        .unwrap()
        .sync_records_after(0, 100)
        .unwrap();
    let globex = tenants
        .get("globex")
        .unwrap()
        .sync_records_after(0, 100)
        .unwrap();
    assert_eq!(acme.len(), 1);
    assert_eq!(globex.len(), 1);
    assert_ne!(
        acme[0].1, globex[0].1,
        "each tenant holds only its own record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_token_is_rejected() {
    let dir = scratch("auth");
    let port = free_port();
    let mut tokens = TokenMap::new();
    tokens.insert("tok-real".into(), "acme".into());
    start_server(&dir, port, tokens);

    let transport = HttpTransport::new("127.0.0.1", port, "tok-forged");
    let err = transport
        .pull(0, 10)
        .expect_err("a forged token must not be served");
    assert!(format!("{err}").contains("401"), "got {err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// What the dashboard is for: one server, several machines, one view.
#[test]
fn records_from_two_machines_land_in_one_tenant() {
    let dir = scratch("fan-in");
    let port = free_port();
    let mut tokens = TokenMap::new();
    tokens.insert("tok".into(), "acme".into());
    let tenants = start_server(&dir, port, tokens);

    for machine in ["laptop-a", "laptop-b"] {
        let store = Store::in_memory().unwrap();
        store
            .upsert_call(&Call::new(machine, "local", format!("{machine}.m4a")))
            .unwrap();
        SyncPeer::new(
            Arc::new(store),
            "hq",
            Box::new(HttpTransport::new("127.0.0.1", port, "tok")),
        )
        .sync()
        .unwrap();
    }

    assert_eq!(
        tenants
            .get("acme")
            .unwrap()
            .sync_records_after(0, 100)
            .unwrap()
            .len(),
        2,
        "both machines are visible in one corpus"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
