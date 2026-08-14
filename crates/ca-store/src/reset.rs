//! Clearing the corpus.
//!
//! Three scopes, because "start again" usually means one of three quite
//! different things and picking the widest by default throws away hours of
//! transcription to fix a scoring problem.
//!
//! Every reset is destructive and none of it is undoable from inside the
//! database, so [`backup_db`] exists to make the whole thing recoverable from
//! a file copy taken immediately beforehand.

use ca_core::error::{CaError, Result};
use serde::{Deserialize, Serialize};

use crate::Store;

/// How much to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetScope {
    /// Scores, findings and run history. Calls and transcripts survive, so a
    /// re-score costs model calls but no transcription.
    Analyses,
    /// Also transcripts, segments, redaction maps and channel timelines. Calls
    /// stay registered and drop back to `discovered`; the audio is untouched
    /// and can be re-processed.
    Transcripts,
    /// Everything: calls, people, teams and history. Settings are kept — they
    /// are configuration, not corpus, and wiping them would sign the user out
    /// of their own provider setup as a side effect of clearing call data.
    All,
}

impl ResetScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResetScope::Analyses => "analyses",
            ResetScope::Transcripts => "transcripts",
            ResetScope::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "analyses" => Some(ResetScope::Analyses),
            "transcripts" => Some(ResetScope::Transcripts),
            "all" => Some(ResetScope::All),
            _ => None,
        }
    }

    /// One line describing exactly what goes, for a confirmation prompt.
    pub fn describe(&self) -> &'static str {
        match self {
            ResetScope::Analyses => {
                "scores, findings and run history — calls and transcripts are kept"
            }
            ResetScope::Transcripts => {
                "scores, findings, transcripts and redaction maps — calls stay registered \
                 and return to `discovered`"
            }
            ResetScope::All => {
                "every call, transcript, score, finding and person — settings are kept"
            }
        }
    }
}

/// What a reset removed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetReport {
    pub calls: usize,
    pub transcripts: usize,
    pub analyses: usize,
    pub findings: usize,
    pub people: usize,
    pub runs: usize,
}

impl Store {
    /// Count what a reset of this scope would remove, without removing it.
    ///
    /// The confirmation prompt is only meaningful if it can say "1,972 calls",
    /// so this runs before every reset rather than being an optional extra.
    pub fn reset_preview(&self, scope: ResetScope) -> Result<ResetReport> {
        let conn = self.lock()?;
        let count = |table: &str| -> Result<usize> {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n.max(0) as usize)
            .map_err(|e| CaError::Storage(format!("count {table}: {e}")))
        };

        Ok(ResetReport {
            analyses: count("analyses")?,
            findings: count("findings")?,
            runs: count("runs")?,
            transcripts: match scope {
                ResetScope::Analyses => 0,
                _ => count("transcripts")?,
            },
            calls: match scope {
                ResetScope::All => count("calls")?,
                _ => 0,
            },
            people: match scope {
                ResetScope::All => count("people")?,
                _ => 0,
            },
        })
    }

    /// Clear the corpus. Irreversible from here — take a backup first.
    pub fn reset(&self, scope: ResetScope) -> Result<ResetReport> {
        let report = self.reset_preview(scope)?;
        let conn = self.lock()?;

        // Ordered widest-last, and run as one batch so a failure part-way
        // cannot leave a call whose transcript is gone but whose stage still
        // claims it was analysed.
        let sql = match scope {
            ResetScope::Analyses => {
                "BEGIN;
                 DELETE FROM analyses;
                 DELETE FROM findings;
                 DELETE FROM run_items;
                 DELETE FROM runs;
                 DELETE FROM jobs;
                 -- A call that was scored is now merely redacted; anything that
                 -- had not reached redaction keeps the stage it was at.
                 UPDATE calls SET stage = 'redacted'
                  WHERE stage IN ('analyzed', 'indexed');
                 COMMIT;"
            }
            ResetScope::Transcripts => {
                "BEGIN;
                 DELETE FROM analyses;
                 DELETE FROM findings;
                 DELETE FROM segments;
                 DELETE FROM transcripts;
                 DELETE FROM redaction_map;
                 DELETE FROM channel_timelines;
                 DELETE FROM run_items;
                 DELETE FROM runs;
                 DELETE FROM jobs;
                 UPDATE calls SET stage = 'discovered', error = NULL;
                 COMMIT;"
            }
            ResetScope::All => {
                // `calls` cascades to transcripts, segments, analyses,
                // findings, redaction_map and channel_timelines. The rest have
                // no foreign key to it by design, so they are named here.
                "BEGIN;
                 DELETE FROM calls;
                 DELETE FROM people;
                 DELETE FROM teams;
                 DELETE FROM run_items;
                 DELETE FROM runs;
                 DELETE FROM jobs;
                 COMMIT;"
            }
        };

        conn.execute_batch(sql)
            .map_err(|e| CaError::Storage(format!("reset {}: {e}", scope.as_str())))?;

        // Reclaim the space rather than leaving a 7 MB file holding nothing.
        // Outside the transaction: SQLite refuses to VACUUM inside one.
        conn.execute_batch("VACUUM;")
            .map_err(|e| CaError::Storage(format!("vacuum after reset: {e}")))?;

        Ok(report)
    }
}

/// Copy the database beside itself before something destructive.
///
/// Returns the backup path. This is the only thing standing between a mistaken
/// reset and a corpus that took hours to build, so it runs by default and has
/// to be opted out of rather than into.
pub fn backup_db(db: &std::path::Path, stamp: &str) -> Result<std::path::PathBuf> {
    let name = db
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "call-analyzer.db".into());
    let target = db.with_file_name(format!("{name}.backup-{stamp}"));
    std::fs::copy(db, &target)
        .map_err(|e| CaError::Storage(format!("backup {}: {e}", db.display())))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::call::{Call, CallStage};

    fn seeded() -> Store {
        let s = Store::in_memory().unwrap();
        let call = Call::new(
            "C:/calls/Call recording +918485823280_251112_142326.m4a",
            "local",
            "Call recording +918485823280_251112_142326.m4a",
        );
        let id = s.upsert_call(&call).unwrap();
        s.attribute_calls(&ca_identity::builtin_patterns(), Default::default())
            .unwrap();
        s.set_stage(&id, CallStage::Analyzed, None).unwrap();
        s
    }

    #[test]
    fn clearing_analyses_keeps_the_calls_and_the_people() {
        let s = seeded();
        let r = s.reset(ResetScope::Analyses).unwrap();

        assert_eq!(r.calls, 0, "no call was counted as removed");
        assert_eq!(s.count_calls().unwrap(), 1);
        assert_eq!(s.list_people(None, None).unwrap().len(), 1);
        // A scored call is now merely redacted, not still claiming a score.
        assert_eq!(
            s.list_calls(0, 10, None).unwrap()[0].stage,
            "redacted",
            "a cleared call must not still say it was analysed"
        );
    }

    #[test]
    fn clearing_transcripts_returns_calls_to_the_start_of_the_pipeline() {
        let s = seeded();
        s.reset(ResetScope::Transcripts).unwrap();

        assert_eq!(s.count_calls().unwrap(), 1);
        assert_eq!(s.list_calls(0, 10, None).unwrap()[0].stage, "discovered");
        // Identity survives: it comes from the filename, not the transcript.
        assert_eq!(s.list_people(None, None).unwrap().len(), 1);
    }

    #[test]
    fn clearing_everything_leaves_an_empty_corpus() {
        let s = seeded();
        let r = s.reset(ResetScope::All).unwrap();

        assert_eq!(r.calls, 1);
        assert_eq!(r.people, 1);
        assert_eq!(s.count_calls().unwrap(), 0);
        assert!(s.list_people(None, None).unwrap().is_empty());
    }

    /// Settings are configuration, not corpus. Wiping a provider setup as a
    /// side effect of clearing call data would be a surprise, and an expensive
    /// one if the key came from somewhere the user no longer has.
    #[test]
    fn a_full_reset_keeps_settings() {
        let s = seeded();
        s.put_setting("local_agent_handle", &"priya sharma".to_string())
            .unwrap();

        s.reset(ResetScope::All).unwrap();

        let kept: Option<String> = s.get_setting("local_agent_handle").unwrap();
        assert_eq!(kept.as_deref(), Some("priya sharma"));
    }

    #[test]
    fn a_preview_counts_without_deleting() {
        let s = seeded();
        let preview = s.reset_preview(ResetScope::All).unwrap();

        assert_eq!(preview.calls, 1);
        assert_eq!(preview.people, 1);
        assert_eq!(s.count_calls().unwrap(), 1, "preview deleted nothing");
    }

    #[test]
    fn resetting_an_empty_corpus_is_not_an_error() {
        let s = Store::in_memory().unwrap();
        let r = s.reset(ResetScope::All).unwrap();
        assert_eq!(r.calls, 0);
    }
}
