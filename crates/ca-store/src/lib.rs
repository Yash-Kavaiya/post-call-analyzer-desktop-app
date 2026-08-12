//! Persistence.
//!
//! A single SQLite file holds every call, transcript, analysis and the resume
//! state behind them. The [`Store`] is `Send`-safe via an internal mutex, so a
//! batch running across many worker threads shares one connection rather than
//! opening a file handle per call.

pub mod corpus;
pub mod observability;
pub mod runs;
pub mod schema;
pub mod settings;

pub use observability::{AgentHealth, AgentRun};
pub use runs::{RunItem, RunSummary};

use ca_core::analysis::{Analysis, Finding};
use ca_core::audio::{AcousticFeatures, AudioMeta, WaveformPeaks};
use ca_core::call::{Call, CallStage};
use ca_core::error::{CaError, Result};
use ca_core::ids::{CallId, SegmentId};
use ca_core::transcript::{Segment, SpeakerRole, Transcript, Word};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;


/// Read models for the UI. `camelCase` on the wire so the TypeScript side
/// needs no field translation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSummary {
    pub id: String,
    pub display_name: String,
    pub stage: String,
    pub duration_secs: Option<f64>,
    pub created_at: String,
    /// QA score, if the scorecard agent has run.
    pub overall: Option<f64>,
    pub speaker_count: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total_calls: usize,
    pub outstanding: usize,
    pub by_stage: std::collections::BTreeMap<String, usize>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub audio_secs: f64,
}


/// What the app resolved at startup. Surfaced so a missing model or binary
/// produces a clear warning in the UI rather than an opaque failure later.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Startup {
    pub whisper_binary: String,
    pub whisper_binary_present: bool,
    pub whisper_model: String,
    pub whisper_model_present: bool,
    pub english_only: bool,
}


/// The database both frontends use by default.
///
/// The CLI and the desktop app are two views of one product; defaulting them
/// to different files silently splits a corpus in half — you ingest 1,968
/// calls in the terminal and the window shows none.
///
/// Matches Tauri's `app_data_dir()` for this app's identifier.
pub fn default_db_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".local/share")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("com.callanalyzer.desktop").join("call-analyzer.db")
}

/// Where remote objects are cached before decoding.
///
/// Beside the database on purpose: the CLI and the desktop app share one
/// corpus, so they should share one set of downloads rather than each pulling
/// the same bucket.
pub fn default_cache_dir() -> std::path::PathBuf {
    default_db_path()
        .parent()
        .map(|p| p.join("cache").join("objects"))
        .unwrap_or_else(|| std::path::PathBuf::from("cache/objects"))
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(schema::open(path)?),
        })
    }

    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(schema::open_in_memory()?),
        })
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| CaError::Storage("store mutex poisoned".into()))
    }

    // ---------------------------------------------------------------- calls

    /// Insert a call, or return the existing one for the same source key.
    ///
    /// Returning the existing row rather than erroring is what makes a
    /// re-scan of a folder cheap: already-known files are recognised and skip
    /// straight to whatever stage they reached.
    pub fn upsert_call(&self, call: &Call) -> Result<CallId> {
        let conn = self.lock()?;

        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM calls WHERE source_name = ?1 AND source_key = ?2",
                params![call.source_name, call.source_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("lookup call: {e}")))?;

        if let Some(id) = existing {
            return Ok(CallId::from(id));
        }

        conn.execute(
            "INSERT INTO calls (id, source_key, source_name, display_name, media_path,
                                stage, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                call.id.as_str(),
                call.source_key,
                call.source_name,
                call.display_name,
                call.media_path,
                call.stage.as_str(),
                call.created_at.to_rfc3339(),
                call.updated_at.to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("insert call: {e}")))?;

        Ok(call.id.clone())
    }

    pub fn set_stage(&self, id: &CallId, stage: CallStage, error: Option<&str>) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calls SET stage = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                id.as_str(),
                stage.as_str(),
                error,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| CaError::Storage(format!("set stage: {e}")))?;
        Ok(())
    }

    pub fn stage_of(&self, id: &CallId) -> Result<Option<CallStage>> {
        let conn = self.lock()?;
        let s: Option<String> = conn
            .query_row("SELECT stage FROM calls WHERE id = ?1", [id.as_str()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| CaError::Storage(format!("stage_of: {e}")))?;
        Ok(s.and_then(|s| parse_stage(&s)))
    }

    pub fn save_audio_meta(
        &self,
        id: &CallId,
        meta: &AudioMeta,
        features: &AcousticFeatures,
        peaks: &WaveformPeaks,
    ) -> Result<()> {
        let conn = self.lock()?;
        let features_json = serde_json::to_string(features)
            .map_err(|e| CaError::Storage(format!("encode features: {e}")))?;
        // Peaks are bincode-free JSON for now: readable, and the volume is
        // small next to the audio itself.
        let peaks_json = serde_json::to_vec(peaks)
            .map_err(|e| CaError::Storage(format!("encode peaks: {e}")))?;

        conn.execute(
            "UPDATE calls SET duration_secs=?2, source_sample_rate=?3, source_channels=?4,
                              codec=?5, bytes=?6, input_lufs=?7, gain_db=?8, denoised=?9,
                              features_json=?10, peaks_blob=?11, updated_at=?12
             WHERE id = ?1",
            params![
                id.as_str(),
                meta.duration_secs,
                meta.source_sample_rate,
                meta.source_channels,
                meta.codec,
                // SQLite integers are signed 64-bit; u64 has no ToSql impl.
                meta.bytes as i64,
                meta.input_lufs,
                meta.gain_db,
                meta.denoised as i32,
                features_json,
                peaks_json,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("save audio meta: {e}")))?;
        Ok(())
    }

    /// Where the call's media lives on disk, if it has been materialised.
    pub fn media_path(&self, id: &CallId) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT media_path FROM calls WHERE id = ?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map(|o| o.flatten())
        .map_err(|e| CaError::Storage(format!("media_path: {e}")))
    }

    pub fn count_calls(&self) -> Result<usize> {
        let conn = self.lock()?;
        let n: i64 = conn
            .query_row("SELECT count(*) FROM calls", [], |r| r.get(0))
            .map_err(|e| CaError::Storage(format!("count: {e}")))?;
        Ok(n as usize)
    }

    /// Calls that have not yet reached `stage`, oldest first.
    ///
    /// This is the resume query: a batch asks for the work still outstanding
    /// rather than tracking progress in memory, so a crash costs nothing.
    pub fn calls_below_stage(&self, stage: CallStage, limit: usize) -> Result<Vec<CallId>> {
        self.calls_in_stage_range(None, stage, limit)
    }

    /// Calls that have reached `at_least` (when given) but not `below`.
    ///
    /// The lower bound is what keeps an analysis-only run from picking up
    /// untranscribed calls and quietly spending an hour on whisper at
    /// cloud-batch concurrency.
    pub fn calls_in_stage_range(
        &self,
        at_least: Option<CallStage>,
        below: CallStage,
        limit: usize,
    ) -> Result<Vec<CallId>> {
        let conn = self.lock()?;
        // The limit counts *matches*, not rows scanned. Pushing it into SQL
        // would take the first N calls by age and only then filter, so
        // `--limit 20` against a corpus whose oldest 20 calls are already done
        // would report nothing left to do.
        let mut stmt = conn
            .prepare("SELECT id, stage FROM calls ORDER BY created_at")
            .map_err(|e| CaError::Storage(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| CaError::Storage(format!("query: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            if out.len() >= limit {
                break;
            }
            let (id, s) = row.map_err(|e| CaError::Storage(format!("row: {e}")))?;
            let reached = parse_stage(&s).unwrap_or(CallStage::Discovered);
            let above_floor = at_least.map(|f| reached.is_at_least(f)).unwrap_or(true);
            if above_floor && !reached.is_at_least(below) {
                out.push(CallId::from(id));
            }
        }
        Ok(out)
    }

    // ----------------------------------------------------------- transcripts

    pub fn save_transcript(&self, t: &Transcript) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction()
            .map_err(|e| CaError::Storage(format!("begin: {e}")))?;

        tx.execute(
            "INSERT INTO transcripts (call_id, engine, model, primary_language, languages_json,
                                      duration_secs, speakers_json, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(call_id) DO UPDATE SET
                engine=excluded.engine, model=excluded.model,
                primary_language=excluded.primary_language,
                languages_json=excluded.languages_json,
                duration_secs=excluded.duration_secs,
                speakers_json=excluded.speakers_json",
            params![
                t.call_id.as_str(),
                t.engine,
                t.model,
                t.primary_language,
                serde_json::to_string(&t.languages).unwrap_or_else(|_| "[]".into()),
                t.duration_secs,
                serde_json::to_string(&t.speakers).unwrap_or_else(|_| "[]".into()),
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("save transcript: {e}")))?;

        // Replace segments wholesale — a re-transcribe must not leave the old
        // ones behind, and their ids are what citations resolve against.
        tx.execute("DELETE FROM segments WHERE call_id = ?1", [t.call_id.as_str()])
            .map_err(|e| CaError::Storage(format!("clear segments: {e}")))?;

        for (i, s) in t.segments.iter().enumerate() {
            tx.execute(
                "INSERT INTO segments (id, call_id, idx, speaker_index, role, start_secs,
                                       end_secs, text, redacted_text, words_json, confidence, language)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    s.id.as_str(),
                    t.call_id.as_str(),
                    i as i64,
                    s.speaker_index,
                    role_str(s.role),
                    s.start,
                    s.end,
                    s.text,
                    s.redacted_text,
                    serde_json::to_string(&s.words).unwrap_or_else(|_| "[]".into()),
                    s.confidence,
                    s.language,
                ],
            )
            .map_err(|e| CaError::Storage(format!("insert segment: {e}")))?;
        }

        tx.commit()
            .map_err(|e| CaError::Storage(format!("commit: {e}")))?;
        Ok(())
    }

    pub fn load_transcript(&self, call_id: &CallId) -> Result<Option<Transcript>> {
        let conn = self.lock()?;

        let head: Option<(String, String, Option<String>, String, f64, String)> = conn
            .query_row(
                "SELECT engine, model, primary_language, languages_json, duration_secs, speakers_json
                 FROM transcripts WHERE call_id = ?1",
                [call_id.as_str()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("load transcript: {e}")))?;

        let Some((engine, model, primary_language, languages_json, duration_secs, speakers_json)) =
            head
        else {
            return Ok(None);
        };

        let mut stmt = conn
            .prepare(
                "SELECT id, speaker_index, role, start_secs, end_secs, text, redacted_text,
                        words_json, confidence, language
                 FROM segments WHERE call_id = ?1 ORDER BY idx",
            )
            .map_err(|e| CaError::Storage(format!("prepare segments: {e}")))?;

        let segments = stmt
            .query_map([call_id.as_str()], |r| {
                let words_json: String = r.get(7)?;
                Ok(Segment {
                    id: SegmentId::from(r.get::<_, String>(0)?),
                    speaker_index: r.get(1)?,
                    role: parse_role(&r.get::<_, String>(2)?),
                    start: r.get(3)?,
                    end: r.get(4)?,
                    text: r.get(5)?,
                    redacted_text: r.get(6)?,
                    words: serde_json::from_str::<Vec<Word>>(&words_json).unwrap_or_default(),
                    confidence: r.get(8)?,
                    language: r.get(9)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query segments: {e}")))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read segments: {e}")))?;

        Ok(Some(Transcript {
            call_id: call_id.clone(),
            segments,
            speakers: serde_json::from_str(&speakers_json).unwrap_or_default(),
            primary_language,
            languages: serde_json::from_str(&languages_json).unwrap_or_default(),
            engine,
            model,
            duration_secs,
        }))
    }

    // -------------------------------------------------------------- privacy

    pub fn save_redaction_map(
        &self,
        call_id: &CallId,
        map: &ca_privacy::RedactionMap,
        kinds: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction()
            .map_err(|e| CaError::Storage(format!("begin: {e}")))?;
        tx.execute(
            "DELETE FROM redaction_map WHERE call_id = ?1",
            [call_id.as_str()],
        )
        .map_err(|e| CaError::Storage(format!("clear map: {e}")))?;

        for (token, value) in &map.entries {
            tx.execute(
                "INSERT INTO redaction_map (call_id, token, value, kind) VALUES (?1,?2,?3,?4)",
                params![
                    call_id.as_str(),
                    token,
                    value,
                    kinds.get(token).cloned().unwrap_or_else(|| "unknown".into())
                ],
            )
            .map_err(|e| CaError::Storage(format!("insert redaction: {e}")))?;
        }
        tx.commit()
            .map_err(|e| CaError::Storage(format!("commit: {e}")))?;
        Ok(())
    }

    pub fn load_redaction_map(&self, call_id: &CallId) -> Result<ca_privacy::RedactionMap> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT token, value FROM redaction_map WHERE call_id = ?1")
            .map_err(|e| CaError::Storage(format!("prepare: {e}")))?;
        let mut map = ca_privacy::RedactionMap::default();
        let rows = stmt
            .query_map([call_id.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| CaError::Storage(format!("query: {e}")))?;
        for row in rows {
            let (t, v) = row.map_err(|e| CaError::Storage(format!("row: {e}")))?;
            map.entries.insert(t, v);
        }
        Ok(map)
    }

    // ------------------------------------------------------------- analyses

    pub fn save_analysis(&self, a: &Analysis) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO analyses (id, call_id, agent, payload_json, provider, model,
                                   input_tokens, output_tokens, cost_usd, latency_ms, error, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(call_id, agent) DO UPDATE SET
                payload_json=excluded.payload_json, provider=excluded.provider,
                model=excluded.model, input_tokens=excluded.input_tokens,
                output_tokens=excluded.output_tokens, cost_usd=excluded.cost_usd,
                latency_ms=excluded.latency_ms, error=excluded.error,
                created_at=excluded.created_at",
            params![
                a.id.as_str(),
                a.call_id.as_str(),
                a.agent.as_str(),
                serde_json::to_string(&a.payload).unwrap_or_else(|_| "null".into()),
                a.provider,
                a.model,
                a.usage.input_tokens,
                a.usage.output_tokens,
                a.usage.cost_usd,
                a.usage.latency_ms as i64,
                a.error,
                a.created_at.to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("save analysis: {e}")))?;
        Ok(())
    }

    pub fn load_analysis(
        &self,
        call_id: &CallId,
        agent: ca_core::analysis::AgentKind,
    ) -> Result<Option<serde_json::Value>> {
        let conn = self.lock()?;
        let s: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM analyses WHERE call_id = ?1 AND agent = ?2",
                params![call_id.as_str(), agent.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("load analysis: {e}")))?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn save_findings(&self, call_id: &CallId, findings: &[Finding]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction()
            .map_err(|e| CaError::Storage(format!("begin: {e}")))?;
        tx.execute("DELETE FROM findings WHERE call_id = ?1", [call_id.as_str()])
            .map_err(|e| CaError::Storage(format!("clear findings: {e}")))?;
        for f in findings {
            tx.execute(
                "INSERT INTO findings (id, call_id, agent, code, title, detail, severity, citations_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    f.id.as_str(),
                    call_id.as_str(),
                    f.agent.as_str(),
                    f.code,
                    f.title,
                    f.detail,
                    format!("{:?}", f.severity).to_lowercase(),
                    serde_json::to_string(&f.citations).unwrap_or_else(|_| "[]".into()),
                ],
            )
            .map_err(|e| CaError::Storage(format!("insert finding: {e}")))?;
        }
        tx.commit()
            .map_err(|e| CaError::Storage(format!("commit: {e}")))?;
        Ok(())
    }

    // ------------------------------------------------------------- queries
    // Read models for the UI. These return flat, serialisable rows rather than
    // domain objects: the dashboard needs a page of 50 summaries, not 50 fully
    // hydrated transcripts.

    /// One page of calls, newest first, optionally filtered by name.
    pub fn list_calls(
        &self,
        offset: usize,
        limit: usize,
        search: Option<&str>,
    ) -> Result<Vec<CallSummary>> {
        let conn = self.lock()?;
        let like = format!("%{}%", search.unwrap_or(""));

        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.display_name, c.stage, c.duration_secs, c.created_at,
                        (SELECT json_extract(a.payload_json, '$.overall')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'qa_scorecard'),
                        (SELECT count(DISTINCT s.speaker_index)
                           FROM segments s WHERE s.call_id = c.id)
                 FROM calls c
                 WHERE (?3 = '%%' OR c.display_name LIKE ?3)
                 ORDER BY c.created_at DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| CaError::Storage(format!("prepare list: {e}")))?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64, like], |r| {
                Ok(CallSummary {
                    id: r.get(0)?,
                    display_name: r.get(1)?,
                    stage: r.get(2)?,
                    duration_secs: r.get(3)?,
                    created_at: r.get(4)?,
                    overall: r.get(5)?,
                    speaker_count: r.get::<_, Option<i64>>(6)?.map(|n| n as u32),
                })
            })
            .map_err(|e| CaError::Storage(format!("query list: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read list: {e}")))
    }

    /// Waveform render data, as stored by the audio stage.
    pub fn waveform(&self, id: &CallId) -> Result<Option<WaveformPeaks>> {
        let conn = self.lock()?;
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT peaks_blob FROM calls WHERE id = ?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map(|o| o.flatten())
            .map_err(|e| CaError::Storage(format!("waveform: {e}")))?;
        Ok(blob.and_then(|b| serde_json::from_slice(&b).ok()))
    }

    pub fn features(&self, id: &CallId) -> Result<Option<AcousticFeatures>> {
        let conn = self.lock()?;
        let s: Option<String> = conn
            .query_row(
                "SELECT features_json FROM calls WHERE id = ?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map(|o| o.flatten())
            .map_err(|e| CaError::Storage(format!("features: {e}")))?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// Corpus-level counters for the dashboard.
    pub fn stats(&self) -> Result<Stats> {
        let conn = self.lock()?;

        let mut by_stage = std::collections::BTreeMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT stage, count(*) FROM calls GROUP BY stage")
                .map_err(|e| CaError::Storage(format!("prepare stages: {e}")))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| CaError::Storage(format!("query stages: {e}")))?;
            for row in rows {
                let (s, n) = row.map_err(|e| CaError::Storage(format!("row: {e}")))?;
                by_stage.insert(s, n as usize);
            }
        }

        let total: i64 = conn
            .query_row("SELECT count(*) FROM calls", [], |r| r.get(0))
            .map_err(|e| CaError::Storage(format!("total: {e}")))?;
        let audio_secs: f64 = conn
            .query_row(
                "SELECT COALESCE(sum(duration_secs),0) FROM calls",
                [],
                |r| r.get(0),
            )
            .map_err(|e| CaError::Storage(format!("audio: {e}")))?;

        let (input_tokens, output_tokens, cost_usd) = {
            drop(conn);
            self.total_usage()?
        };

        let outstanding = self
            .calls_below_stage(CallStage::Redacted, usize::MAX)?
            .len();

        Ok(Stats {
            total_calls: total as usize,
            outstanding,
            by_stage,
            input_tokens,
            output_tokens,
            cost_usd,
            audio_secs,
        })
    }

    /// Aggregate token spend so a batch can be costed as it runs.
    pub fn total_usage(&self) -> Result<(u64, u64, f64)> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COALESCE(sum(input_tokens),0), COALESCE(sum(output_tokens),0),
                    COALESCE(sum(cost_usd),0) FROM analyses",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, f64>(2)?,
                ))
            },
        )
        .map_err(|e| CaError::Storage(format!("usage: {e}")))
    }
}

fn role_str(r: SpeakerRole) -> &'static str {
    match r {
        SpeakerRole::Agent => "agent",
        SpeakerRole::Customer => "customer",
        SpeakerRole::Unknown => "unknown",
    }
}

fn parse_role(s: &str) -> SpeakerRole {
    match s {
        "agent" => SpeakerRole::Agent,
        "customer" => SpeakerRole::Customer,
        _ => SpeakerRole::Unknown,
    }
}

fn parse_stage(s: &str) -> Option<CallStage> {
    Some(match s {
        "discovered" => CallStage::Discovered,
        "decoded" => CallStage::Decoded,
        "transcribed" => CallStage::Transcribed,
        "diarized" => CallStage::Diarized,
        "redacted" => CallStage::Redacted,
        "analyzed" => CallStage::Analyzed,
        "indexed" => CallStage::Indexed,
        "failed" => CallStage::Failed,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(key: &str) -> Call {
        Call::new(key, "local", format!("call {key}"))
    }

    fn transcript(id: &CallId) -> Transcript {
        Transcript {
            call_id: id.clone(),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 1,
                role: SpeakerRole::Customer,
                start: 1.0,
                end: 3.5,
                text: "my number is 9876543210".into(),
                redacted_text: Some("my number is [PHONE_1]".into()),
                words: vec![Word {
                    text: "my".into(),
                    start: 1.0,
                    end: 1.2,
                    confidence: Some(0.9),
                }],
                confidence: Some(0.88),
                language: Some("en".into()),
            }],
            speakers: vec![],
            primary_language: Some("en".into()),
            languages: vec!["en".into()],
            engine: "whisper-cli".into(),
            model: "small.en".into(),
            duration_secs: 10.0,
        }
    }

    #[test]
    fn inserting_the_same_source_key_twice_returns_one_call() {
        let s = Store::in_memory().unwrap();
        let a = s.upsert_call(&call("k1")).unwrap();
        let b = s.upsert_call(&call("k1")).unwrap();
        assert_eq!(a, b, "re-scan must not create a duplicate");
        assert_eq!(s.count_calls().unwrap(), 1);
    }

    #[test]
    fn stage_round_trips() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        s.set_stage(&id, CallStage::Transcribed, None).unwrap();
        assert_eq!(s.stage_of(&id).unwrap(), Some(CallStage::Transcribed));
    }

    #[test]
    fn resume_query_returns_only_unfinished_work() {
        let s = Store::in_memory().unwrap();
        let a = s.upsert_call(&call("a")).unwrap();
        let b = s.upsert_call(&call("b")).unwrap();
        s.set_stage(&a, CallStage::Analyzed, None).unwrap();

        let todo = s.calls_below_stage(CallStage::Analyzed, 100).unwrap();
        assert_eq!(todo, vec![b], "finished calls must not be reprocessed");
    }

    #[test]
    fn failed_calls_are_returned_for_retry() {
        let s = Store::in_memory().unwrap();
        let a = s.upsert_call(&call("a")).unwrap();
        s.set_stage(&a, CallStage::Failed, Some("boom")).unwrap();
        let todo = s.calls_below_stage(CallStage::Analyzed, 100).unwrap();
        assert_eq!(todo, vec![a]);
    }

    #[test]
    fn transcript_round_trips_with_segments_and_words() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        let t = transcript(&id);
        s.save_transcript(&t).unwrap();

        let back = s.load_transcript(&id).unwrap().expect("missing transcript");
        assert_eq!(back.segments.len(), 1);
        let seg = &back.segments[0];
        assert_eq!(seg.id.as_str(), "seg_1");
        assert_eq!(seg.speaker_index, 1);
        assert_eq!(seg.role, SpeakerRole::Customer);
        assert_eq!(seg.redacted_text.as_deref(), Some("my number is [PHONE_1]"));
        assert_eq!(seg.words.len(), 1);
        assert_eq!(back.engine, "whisper-cli");
    }

    #[test]
    fn re_saving_a_transcript_replaces_rather_than_appends() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        s.save_transcript(&transcript(&id)).unwrap();
        s.save_transcript(&transcript(&id)).unwrap();
        let back = s.load_transcript(&id).unwrap().unwrap();
        assert_eq!(back.segments.len(), 1, "segments duplicated on re-save");
    }

    #[test]
    fn missing_transcript_reads_as_none() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        assert!(s.load_transcript(&id).unwrap().is_none());
    }

    #[test]
    fn redaction_map_round_trips() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        let mut map = ca_privacy::RedactionMap::default();
        map.entries
            .insert("[PHONE_1]".into(), "9876543210".into());
        s.save_redaction_map(&id, &map, &Default::default()).unwrap();

        let back = s.load_redaction_map(&id).unwrap();
        assert_eq!(back.entries.get("[PHONE_1]").unwrap(), "9876543210");
        assert_eq!(back.restore("call [PHONE_1]"), "call 9876543210");
    }

    #[test]
    fn usage_totals_across_analyses() {
        use ca_core::analysis::{AgentKind, Usage};
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        let a = Analysis {
            id: ca_core::ids::AnalysisId::new(),
            call_id: id.clone(),
            agent: AgentKind::QaScorecard,
            payload: serde_json::json!({"overall": 82.0}),
            provider: "nvidia-nim".into(),
            model: "m".into(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 200,
                cost_usd: Some(0.01),
                latency_ms: 900,
            },
            created_at: chrono::Utc::now(),
            error: None,
        };
        s.save_analysis(&a).unwrap();

        let (i, o, c) = s.total_usage().unwrap();
        assert_eq!((i, o), (1000, 200));
        assert!((c - 0.01).abs() < 1e-9);

        let payload = s.load_analysis(&id, AgentKind::QaScorecard).unwrap().unwrap();
        assert_eq!(payload["overall"], 82.0);
    }

    #[test]
    fn re_running_an_agent_overwrites_its_previous_result() {
        use ca_core::analysis::{AgentKind, Usage};
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k")).unwrap();
        let mk = |score: f64| Analysis {
            id: ca_core::ids::AnalysisId::new(),
            call_id: id.clone(),
            agent: AgentKind::QaScorecard,
            payload: serde_json::json!({ "overall": score }),
            provider: "p".into(),
            model: "m".into(),
            usage: Usage::default(),
            created_at: chrono::Utc::now(),
            error: None,
        };
        s.save_analysis(&mk(50.0)).unwrap();
        s.save_analysis(&mk(90.0)).unwrap();
        let payload = s.load_analysis(&id, AgentKind::QaScorecard).unwrap().unwrap();
        assert_eq!(payload["overall"], 90.0, "second run should win");
    }
}
