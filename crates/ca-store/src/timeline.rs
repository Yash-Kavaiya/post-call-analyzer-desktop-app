//! Who was speaking when, as captured rather than inferred.
//!
//! A live call arrives on two devices — the microphone is the agent, the system
//! loopback is the customer — so the speaker of every utterance is known at
//! capture time. That is information the acoustic diarizer can only ever
//! approximate, and throwing it away to re-guess it later would be perverse.
//!
//! Stored as one JSON blob per call rather than a row per span. A ten-minute
//! call produces a few hundred spans, they are always read together, and they
//! are never queried by time in SQL.

use crate::Store;
use ca_core::error::{CaError, Result};
use ca_core::ids::CallId;
use ca_core::transcript::ChannelTimeline;
use rusqlite::{params, OptionalExtension};

impl Store {
    pub fn save_channel_timeline(
        &self,
        call_id: &CallId,
        timeline: &ChannelTimeline,
    ) -> Result<()> {
        let json = serde_json::to_string(timeline)
            .map_err(|e| CaError::Storage(format!("encode timeline: {e}")))?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO channel_timelines (call_id, spans_json) VALUES (?1, ?2)
             ON CONFLICT(call_id) DO UPDATE SET spans_json = excluded.spans_json",
            params![call_id.as_str(), json],
        )
        .map_err(|e| CaError::Storage(format!("save timeline: {e}")))?;
        Ok(())
    }

    /// `None` for any call that was not captured live, which is most of them.
    pub fn channel_timeline(&self, call_id: &CallId) -> Result<Option<ChannelTimeline>> {
        let conn = self.lock()?;
        let json: Option<String> = conn
            .query_row(
                "SELECT spans_json FROM channel_timelines WHERE call_id = ?1",
                params![call_id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("load timeline: {e}")))?;
        // A blob that no longer parses reads as absent rather than failing the
        // call: the transcript is still usable, it just falls back to diarization.
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::call::Call;
    use ca_core::transcript::SpeechSpan;

    fn store_with_call() -> (Store, CallId) {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&Call::new("k", "live", "session")).unwrap();
        (s, id)
    }

    fn timeline() -> ChannelTimeline {
        ChannelTimeline {
            spans: vec![
                SpeechSpan {
                    channel: 0,
                    start: 0.0,
                    end: 2.5,
                },
                SpeechSpan {
                    channel: 1,
                    start: 3.0,
                    end: 6.25,
                },
            ],
        }
    }

    #[test]
    fn a_timeline_round_trips() {
        let (s, id) = store_with_call();
        s.save_channel_timeline(&id, &timeline()).unwrap();
        assert_eq!(s.channel_timeline(&id).unwrap(), Some(timeline()));
    }

    #[test]
    fn an_ingested_call_has_no_timeline() {
        let (s, id) = store_with_call();
        assert_eq!(s.channel_timeline(&id).unwrap(), None);
    }

    #[test]
    fn re_saving_replaces_rather_than_failing() {
        // A session that is written incrementally saves its timeline many times.
        let (s, id) = store_with_call();
        s.save_channel_timeline(&id, &ChannelTimeline::default())
            .unwrap();
        s.save_channel_timeline(&id, &timeline()).unwrap();
        assert_eq!(s.channel_timeline(&id).unwrap().unwrap().spans.len(), 2);
    }

    #[test]
    fn an_unreadable_blob_reads_as_absent_rather_than_failing() {
        // Falling back to diarization is worse than the truth but far better
        // than refusing to open the call.
        let (s, id) = store_with_call();
        s.save_channel_timeline(&id, &timeline()).unwrap();
        {
            let conn = s.lock().unwrap();
            conn.execute("UPDATE channel_timelines SET spans_json = 'nope'", [])
                .unwrap();
        }
        assert_eq!(s.channel_timeline(&id).unwrap(), None);
    }
}
