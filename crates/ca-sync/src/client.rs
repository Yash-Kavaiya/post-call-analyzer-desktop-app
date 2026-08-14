//! Pushing local metrics to a peer and pulling back what it has.
//!
//! The transport is a trait so the whole sync loop — watermarks, batching,
//! idempotency, the guard that refuses to send content — is testable without
//! a socket. `HttpTransport` is the one real implementation.

use ca_core::error::{CaError, Result};
use ca_store::Store;
use std::sync::Arc;

use crate::record::{FindingMetric, PullResponse, PushRequest, PushResponse, SyncRecord};
use crate::BATCH;

/// How a client reaches a peer.
///
/// Implemented over HTTP in production and in memory in tests, which is what
/// lets the interesting logic be tested without a network.
pub trait Transport: Send + Sync {
    fn push(&self, req: &PushRequest) -> Result<PushResponse>;
    fn pull(&self, since: i64, limit: usize) -> Result<PullResponse>;
}

/// What one sync run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub pushed: usize,
    pub pulled: usize,
    /// Records the local guard refused to send. Never silently dropped.
    pub withheld: usize,
    /// Records the peer rejected.
    pub rejected: usize,
}

/// Which way records move.
///
/// A laptop feeding a dashboard only pushes; pulling would fetch its own
/// records straight back and fill its disk with a copy of what it just sent.
/// A dashboard only pulls. `Both` is for a peer that is genuinely two-way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Push,
    Pull,
    Both,
}

impl Direction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "push" => Some(Direction::Push),
            "pull" => Some(Direction::Pull),
            "both" => Some(Direction::Both),
            _ => None,
        }
    }

    fn pushes(&self) -> bool {
        matches!(self, Direction::Push | Direction::Both)
    }

    fn pulls(&self) -> bool {
        matches!(self, Direction::Pull | Direction::Both)
    }
}

/// One peer, and this machine's progress with it.
pub struct SyncPeer {
    store: Arc<Store>,
    transport: Box<dyn Transport>,
    /// Names the row in `sync_state`, so two peers advance independently.
    name: String,
}

impl SyncPeer {
    pub fn new(store: Arc<Store>, name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            store,
            transport,
            name: name.into(),
        }
    }

    /// Push what has changed, then pull what is new. Safe to run repeatedly.
    ///
    /// Defaults to push-only, which is what a machine feeding a dashboard
    /// wants; see [`Direction`].
    pub fn sync(&self) -> Result<SyncOutcome> {
        self.sync_with(Direction::default())
    }

    /// Sync in one direction, draining rather than sending a single batch.
    ///
    /// A first sync of a real corpus is thousands of calls and `BATCH` is 500.
    /// Sending one batch per invocation would mean running the command four
    /// times and never being told why — so this loops until nothing is left.
    /// The bound exists so a peer that keeps returning the same cursor cannot
    /// spin forever.
    pub fn sync_with(&self, direction: Direction) -> Result<SyncOutcome> {
        const MAX_BATCHES: usize = 10_000;
        let mut total = SyncOutcome::default();

        if direction.pushes() {
            for _ in 0..MAX_BATCHES {
                let batch = self.push()?;
                total.pushed += batch.pushed;
                total.withheld += batch.withheld;
                total.rejected += batch.rejected;
                // A batch that sent nothing new means the corpus is drained.
                // `withheld` alone still counts as progress: the watermark
                // moved past those records, so the next pass will not resend.
                if batch.pushed == 0 && batch.withheld == 0 {
                    break;
                }
            }
        }

        if direction.pulls() {
            for _ in 0..MAX_BATCHES {
                let pulled = self.pull()?;
                total.pulled += pulled;
                if pulled == 0 {
                    break;
                }
            }
        }

        Ok(total)
    }

    /// Send local changes since the stored watermark.
    ///
    /// The watermark only advances on a response, so a failed push is retried
    /// in full next time rather than leaving a hole.
    pub fn push(&self) -> Result<SyncOutcome> {
        let (pushed_through, _) = self.store.sync_state(&self.name)?;
        let metrics = self
            .store
            .call_metrics_since(pushed_through.as_deref(), BATCH)?;
        if metrics.is_empty() {
            return Ok(SyncOutcome::default());
        }

        let high_water = metrics
            .iter()
            .map(|m| m.updated_at.clone())
            .max()
            .unwrap_or_default();

        let mut records = Vec::with_capacity(metrics.len());
        let mut withheld = 0usize;
        for m in metrics {
            let record = to_record(m);
            // Checked here, before the socket, so a record that should never
            // leave never does — including when the peer would have accepted
            // it. The local machine is the last place this can be stopped.
            match record.assert_no_content() {
                Ok(()) => records.push(record),
                Err(reason) => {
                    withheld += 1;
                    tracing::error!(
                        call = %record.call_id,
                        reason = %reason,
                        "refusing to sync a record that carries call content"
                    );
                }
            }
        }

        let pushed = records.len();
        let response = self.transport.push(&PushRequest { records })?;

        // Advance only after the peer confirms. A partial accept still moves
        // the watermark: the rejected ids are reported, and re-sending records
        // the peer has already refused would loop forever.
        let (_, pulled_through) = self.store.sync_state(&self.name)?;
        self.store
            .set_sync_state(&self.name, Some(&high_water), pulled_through)?;

        Ok(SyncOutcome {
            pushed,
            withheld,
            rejected: response.rejected.len(),
            pulled: 0,
        })
    }

    /// Fetch records the peer has that this machine has not.
    pub fn pull(&self) -> Result<usize> {
        let (pushed_through, since) = self.store.sync_state(&self.name)?;
        let response = self.transport.pull(since, BATCH)?;

        for record in &response.records {
            // Trust nothing an inbound record claims: a peer that sends content
            // is a peer to refuse, not one to store and hope.
            if let Err(reason) = record.assert_no_content() {
                tracing::error!(
                    call = %record.call_id,
                    reason = %reason,
                    "discarding an inbound record that carries call content"
                );
                continue;
            }
            let json = serde_json::to_string(record)
                .map_err(|e| CaError::Storage(format!("encode record: {e}")))?;
            self.store.put_sync_record(&record.call_id, &json)?;
        }

        self.store
            .set_sync_state(&self.name, pushed_through.as_deref(), response.cursor)?;
        Ok(response.records.len())
    }
}

/// Project the store's metrics onto the wire type.
fn to_record(m: ca_store::CallMetrics) -> SyncRecord {
    SyncRecord {
        call_id: m.call_id,
        occurred_at: m.occurred_at.as_deref().and_then(parse_time),
        duration_secs: m.duration_secs,
        stage: m.stage,
        agent_id: m.agent_id,
        contact_id: m.contact_id,
        qa_overall: m.qa_overall,
        sentiment: m.sentiment,
        compliance_passed: m.compliance_passed,
        findings: m
            .findings
            .into_iter()
            .map(|(code, severity, count)| FindingMetric {
                code,
                severity,
                count,
            })
            .collect(),
        updated_at: parse_time(&m.updated_at).unwrap_or_else(chrono::Utc::now),
    }
}

fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::{Finding, Severity};
    use ca_core::call::Call;
    use std::sync::Mutex;

    /// A peer that keeps what it is given, so a whole sync round trip runs
    /// without a socket.
    #[derive(Default)]
    struct MemoryPeer {
        received: Mutex<Vec<SyncRecord>>,
        /// Records this peer offers back on pull.
        offers: Mutex<Vec<SyncRecord>>,
        /// Codes this peer refuses, to exercise the rejected path.
        refuse: Mutex<Vec<String>>,
    }

    impl Transport for MemoryPeer {
        fn push(&self, req: &PushRequest) -> Result<PushResponse> {
            let refuse = self.refuse.lock().unwrap().clone();
            let mut accepted = 0;
            let mut rejected = Vec::new();
            for r in &req.records {
                if refuse.contains(&r.call_id) {
                    rejected.push(r.call_id.clone());
                } else {
                    self.received.lock().unwrap().push(r.clone());
                    accepted += 1;
                }
            }
            Ok(PushResponse {
                accepted,
                rejected,
                cursor: accepted as i64,
            })
        }

        fn pull(&self, since: i64, _limit: usize) -> Result<PullResponse> {
            let offers = self.offers.lock().unwrap().clone();
            let records: Vec<_> = offers.into_iter().skip(since as usize).collect();
            Ok(PullResponse {
                cursor: since + records.len() as i64,
                records,
            })
        }
    }

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
                call_id: id.clone(),
                agent: ca_core::AgentKind::Compliance,
                code: "HOLD_NOT_TIMEBOXED".into(),
                title: "Hold placed without an expected duration".into(),
                detail: "The agent said 'give me the card number'.".into(),
                severity: Severity::High,
                citations: vec![],
            }],
        )
        .unwrap();
        Arc::new(s)
    }

    #[test]
    fn a_push_sends_metrics_and_advances_the_watermark() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        let sync = SyncPeer::new(store.clone(), "hq", Box::new(PeerHandle(peer.clone())));

        let out = sync.push().unwrap();
        assert_eq!(out.pushed, 1);
        assert_eq!(out.withheld, 0);
        assert_eq!(peer.received.lock().unwrap().len(), 1);

        // Nothing changed since, so a second push sends nothing.
        assert_eq!(sync.push().unwrap().pushed, 0);
    }

    /// The whole point of the phase, asserted end to end.
    #[test]
    fn what_reaches_the_peer_holds_no_call_content() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        SyncPeer::new(store, "hq", Box::new(PeerHandle(peer.clone())))
            .push()
            .unwrap();

        let json = serde_json::to_string(&*peer.received.lock().unwrap()).unwrap();
        for leaked in [
            "918485823280",
            "card number",
            "Hold placed without",
            "Call recording",
        ] {
            assert!(!json.contains(leaked), "`{leaked}` reached the peer");
        }
        assert!(
            json.contains("HOLD_NOT_TIMEBOXED"),
            "the code should travel"
        );
    }

    #[test]
    fn a_pull_stores_what_the_peer_offers() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        peer.offers.lock().unwrap().push(SyncRecord {
            call_id: "call_remote_1".into(),
            occurred_at: None,
            duration_secs: Some(30.0),
            stage: "analyzed".into(),
            agent_id: None,
            contact_id: None,
            qa_overall: Some(88.0),
            sentiment: None,
            compliance_passed: Some(true),
            findings: vec![],
            updated_at: chrono::Utc::now(),
        });

        let sync = SyncPeer::new(store.clone(), "hq", Box::new(PeerHandle(peer.clone())));
        assert_eq!(sync.pull().unwrap(), 1);
        assert_eq!(store.sync_records_after(0, 10).unwrap().len(), 1);

        // The cursor advanced, so pulling again fetches nothing.
        assert_eq!(sync.pull().unwrap(), 0);
    }

    /// A peer that sends content is refused rather than stored.
    #[test]
    fn an_inbound_record_carrying_content_is_discarded() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        peer.offers.lock().unwrap().push(SyncRecord {
            call_id: "call_remote_2".into(),
            occurred_at: None,
            duration_secs: None,
            stage: "analyzed".into(),
            agent_id: None,
            // A name where an opaque id belongs.
            contact_id: Some("Babulal Kavaiya".into()),
            qa_overall: None,
            sentiment: None,
            compliance_passed: None,
            findings: vec![],
            updated_at: chrono::Utc::now(),
        });

        let sync = SyncPeer::new(store.clone(), "hq", Box::new(PeerHandle(peer.clone())));
        sync.pull().unwrap();
        assert!(
            store.sync_records_after(0, 10).unwrap().is_empty(),
            "a record carrying a name must not be stored"
        );
    }

    #[test]
    fn a_rejected_record_is_reported_rather_than_hidden() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        let call_id = store
            .call_metrics_since(None, 10)
            .unwrap()
            .remove(0)
            .call_id;
        peer.refuse.lock().unwrap().push(call_id);

        let out = SyncPeer::new(store, "hq", Box::new(PeerHandle(peer)))
            .push()
            .unwrap();
        assert_eq!(out.rejected, 1);
    }

    #[test]
    fn two_peers_advance_independently() {
        let store = corpus();
        let hq = Arc::new(MemoryPeer::default());
        let backup = Arc::new(MemoryPeer::default());

        SyncPeer::new(store.clone(), "hq", Box::new(PeerHandle(hq.clone())))
            .push()
            .unwrap();
        // The second peer has seen nothing yet, so it gets the full corpus.
        let out = SyncPeer::new(store, "backup", Box::new(PeerHandle(backup.clone())))
            .push()
            .unwrap();

        assert_eq!(out.pushed, 1);
        assert_eq!(hq.received.lock().unwrap().len(), 1);
        assert_eq!(backup.received.lock().unwrap().len(), 1);
    }

    /// A first sync of a real corpus is several batches. Requiring the user
    /// to run the command four times, with no hint that more remains, is the
    /// behaviour this guards against.
    #[test]
    fn a_sync_drains_rather_than_sending_one_batch() {
        let store = Store::in_memory().unwrap();
        for i in 0..(BATCH + 25) {
            store
                .upsert_call(&Call::new(format!("k{i}"), "local", format!("c{i}.m4a")))
                .unwrap();
        }
        let peer = Arc::new(MemoryPeer::default());
        let sync = SyncPeer::new(Arc::new(store), "hq", Box::new(PeerHandle(peer.clone())));

        let out = sync.sync_with(Direction::Push).unwrap();
        assert_eq!(out.pushed, BATCH + 25, "every call was sent in one run");
        assert_eq!(peer.received.lock().unwrap().len(), BATCH + 25);
    }

    /// A laptop feeding a dashboard must not pull its own records back.
    #[test]
    fn push_only_is_the_default_and_pulls_nothing() {
        let store = corpus();
        let peer = Arc::new(MemoryPeer::default());
        peer.offers.lock().unwrap().push(SyncRecord {
            call_id: "call_remote".into(),
            occurred_at: None,
            duration_secs: None,
            stage: "analyzed".into(),
            agent_id: None,
            contact_id: None,
            qa_overall: None,
            sentiment: None,
            compliance_passed: None,
            findings: vec![],
            updated_at: chrono::Utc::now(),
        });

        let sync = SyncPeer::new(store.clone(), "hq", Box::new(PeerHandle(peer)));
        let out = sync.sync().unwrap();

        assert_eq!(out.pushed, 1);
        assert_eq!(out.pulled, 0, "the default direction does not pull");
        assert!(store.sync_records_after(0, 10).unwrap().is_empty());
    }

    #[test]
    fn a_direction_round_trips_through_its_name() {
        for d in [Direction::Push, Direction::Pull, Direction::Both] {
            let name = match d {
                Direction::Push => "push",
                Direction::Pull => "pull",
                Direction::Both => "both",
            };
            assert_eq!(Direction::parse(name), Some(d));
        }
        assert_eq!(Direction::parse("sideways"), None);
    }

    /// `Box<dyn Transport>` needs an owned value; the tests want to inspect the
    /// peer afterwards, so they share one through an `Arc`.
    struct PeerHandle(Arc<MemoryPeer>);

    impl Transport for PeerHandle {
        fn push(&self, req: &PushRequest) -> Result<PushResponse> {
            self.0.push(req)
        }
        fn pull(&self, since: i64, limit: usize) -> Result<PullResponse> {
            self.0.pull(since, limit)
        }
    }
}
