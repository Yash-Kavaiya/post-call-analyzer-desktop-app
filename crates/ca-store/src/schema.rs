//! Database schema and migrations.
//!
//! One SQLite file holds everything: calls, transcripts, analyses, and the
//! resume state that lets a multi-day batch survive a reboot. Migrations are
//! applied in order and recorded, so an existing database upgrades in place
//! rather than needing a rebuild.

use ca_core::error::{CaError, Result};
use rusqlite::Connection;

/// Each entry runs once, in order, and is recorded in `schema_migrations`.
/// Never edit a shipped migration — append a new one.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_initial",
        r#"
CREATE TABLE calls (
    id                  TEXT PRIMARY KEY,
    source_key          TEXT NOT NULL,
    source_name         TEXT NOT NULL,
    display_name        TEXT NOT NULL,
    media_path          TEXT,
    stage               TEXT NOT NULL,
    error               TEXT,
    duration_secs       REAL,
    source_sample_rate  INTEGER,
    source_channels     INTEGER,
    codec               TEXT,
    bytes               INTEGER,
    input_lufs          REAL,
    gain_db             REAL,
    denoised            INTEGER NOT NULL DEFAULT 0,
    features_json       TEXT,
    peaks_blob          BLOB,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

-- A batch re-run must not re-ingest the same file, and a source may be
-- re-scanned many times, so identity is (source, key) rather than path.
CREATE UNIQUE INDEX idx_calls_source ON calls(source_name, source_key);
CREATE INDEX idx_calls_stage ON calls(stage);

CREATE TABLE transcripts (
    call_id          TEXT PRIMARY KEY REFERENCES calls(id) ON DELETE CASCADE,
    engine           TEXT NOT NULL,
    model            TEXT NOT NULL,
    primary_language TEXT,
    languages_json   TEXT NOT NULL DEFAULT '[]',
    duration_secs    REAL NOT NULL DEFAULT 0,
    speakers_json    TEXT NOT NULL DEFAULT '[]',
    created_at       TEXT NOT NULL
);

CREATE TABLE segments (
    id            TEXT PRIMARY KEY,
    call_id       TEXT NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    idx           INTEGER NOT NULL,
    speaker_index INTEGER NOT NULL,
    role          TEXT NOT NULL,
    start_secs    REAL NOT NULL,
    end_secs      REAL NOT NULL,
    text          TEXT NOT NULL,
    redacted_text TEXT,
    words_json    TEXT NOT NULL DEFAULT '[]',
    confidence    REAL,
    language      TEXT
);

CREATE INDEX idx_segments_call ON segments(call_id, idx);

-- Reversible PII mapping. Stays local; never leaves the machine.
CREATE TABLE redaction_map (
    call_id TEXT NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    token   TEXT NOT NULL,
    value   TEXT NOT NULL,
    kind    TEXT NOT NULL,
    PRIMARY KEY (call_id, token)
);

CREATE TABLE analyses (
    id            TEXT PRIMARY KEY,
    call_id       TEXT NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    agent         TEXT NOT NULL,
    payload_json  TEXT NOT NULL,
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd      REAL,
    latency_ms    INTEGER NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    TEXT NOT NULL
);

-- One result per agent per call; re-running an agent replaces its row.
CREATE UNIQUE INDEX idx_analyses_call_agent ON analyses(call_id, agent);

CREATE TABLE findings (
    id         TEXT PRIMARY KEY,
    call_id    TEXT NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    agent      TEXT NOT NULL,
    code       TEXT NOT NULL,
    title      TEXT NOT NULL,
    detail     TEXT NOT NULL,
    severity   TEXT NOT NULL,
    citations_json TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX idx_findings_call ON findings(call_id);
CREATE INDEX idx_findings_severity ON findings(severity);

-- Batch bookkeeping, so a run can be resumed or reported on after a crash.
CREATE TABLE jobs (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    total       INTEGER NOT NULL DEFAULT 0,
    completed   INTEGER NOT NULL DEFAULT 0,
    failed      INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT
);
"#,
    ),
    (
        "0002_run_history",
        r#"
-- What happened, and when, across every batch ever started.
--
-- The pipeline already records where each call *currently* is, but that is a
-- single mutable value: it cannot say which run touched a call, how long the
-- call took, or that something failed two runs ago and was quietly fixed by the
-- next one. Without this table the ingest screen can only show the run it is
-- watching live, and loses even that on navigation.
CREATE TABLE runs (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    label        TEXT NOT NULL,
    target_stage TEXT NOT NULL,
    limit_n      INTEGER,
    concurrency  INTEGER NOT NULL DEFAULT 1,
    total        INTEGER NOT NULL DEFAULT 0,
    processed    INTEGER NOT NULL DEFAULT 0,
    failed       INTEGER NOT NULL DEFAULT 0,
    -- running | done | interrupted
    status       TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT
);

CREATE INDEX idx_runs_started ON runs(started_at DESC);

-- One row per call per run.
--
-- `call_id` is deliberately *not* a foreign key. History is a record of what
-- happened; cascading a delete would rewrite it, so removing a call from the
-- corpus leaves its past runs intact and readable.
CREATE TABLE run_items (
    run_id     TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    call_id    TEXT NOT NULL,
    name       TEXT NOT NULL,
    stage      TEXT NOT NULL,
    -- running | done | failed
    status     TEXT NOT NULL,
    secs       REAL,
    error      TEXT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, call_id)
);

CREATE INDEX idx_run_items_run ON run_items(run_id, updated_at);
"#,
    ),
    (
        "0003_settings",
        r#"
-- User configuration, as JSON values under named keys.
--
-- Deliberately a key/value table rather than typed columns: settings gain
-- fields constantly, and a migration per checkbox is a bad trade. What must
-- never appear here is an API key — those go to the OS credential store, which
-- is encrypted per user and does not travel with a copy of this file.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#,
    ),
    (
        "0004_merge_duplicate_local_paths",
        r#"
-- Collapse recordings that were registered more than once.
--
-- Identity is (source_name, source_key), and source_key used to be the path
-- string exactly as it arrived. The CLI produced `C:/Users/...` while the file
-- dialog produced `C:\Users\...`, so scanning one folder through both entry
-- points registered every file twice. The duplicate had no audio processed
-- against it, which is what a Calls list full of rows with no waveform is.
--
-- Order matters: the losing rows are removed *before* the keys are normalised,
-- because normalising first would violate the unique index.

-- Keep one row per normalised key: whichever holds real work. Peaks first
-- (that row has decoded audio, and a cascade would delete its transcript and
-- analyses), then the furthest stage, then the oldest.
DELETE FROM calls WHERE id IN (
    SELECT id FROM (
        SELECT id, ROW_NUMBER() OVER (
            PARTITION BY source_name, replace(source_key, '/', '\')
            ORDER BY
                (peaks_blob IS NOT NULL) DESC,
                CASE stage
                    WHEN 'indexed'     THEN 6
                    WHEN 'analyzed'    THEN 5
                    WHEN 'redacted'    THEN 4
                    WHEN 'diarized'    THEN 3
                    WHEN 'transcribed' THEN 2
                    WHEN 'decoded'     THEN 1
                    ELSE 0
                END DESC,
                created_at ASC
        ) AS rn
        FROM calls
        -- Only local paths. A cloud key is a URL, where `/` is structural.
        WHERE source_name = 'local'
    )
    WHERE rn > 1
);

UPDATE calls
   SET source_key = replace(source_key, '/', '\')
 WHERE source_name = 'local' AND source_key LIKE '%/%';

UPDATE calls
   SET media_path = replace(media_path, '/', '\')
 WHERE media_path IS NOT NULL
   AND media_path NOT LIKE 'http://%'
   AND media_path NOT LIKE 'https://%'
   AND media_path LIKE '%/%';
"#,
    ),
    (
        "0005_channel_timeline",
        r#"
-- Who was speaking when, taken from the capture device rather than inferred.
--
-- A live call arrives on two devices: the microphone is the agent and the
-- system loopback is the customer. That makes the speaker of every utterance
-- known at capture time, which is something the acoustic diarizer can only
-- approximate. Stored as one blob per call because the spans are always read
-- together and never queried by time in SQL.
CREATE TABLE channel_timelines (
    call_id    TEXT PRIMARY KEY REFERENCES calls(id) ON DELETE CASCADE,
    spans_json TEXT NOT NULL
);
"#,
    ),
    (
        "0006_people",
        r#"
-- Who was on the call.
--
-- The corpus was keyed by file path, so it could say what a call scored but
-- never whose call it was. Every per-agent rollup and every repeat-contact
-- question needs this table to exist first.
--
-- Local-only, the same class of data as `redaction_map`: these are exactly the
-- names and numbers the transcript pipeline scrubs before a provider sees a
-- call, so identity must not become a second way for them to leave.
CREATE TABLE teams (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_teams_name ON teams(name);

CREATE TABLE people (
    id           TEXT PRIMARY KEY,
    -- agent | contact
    kind         TEXT NOT NULL,
    display_name TEXT NOT NULL,
    -- Normalised identity key: digits for numbers, folded text for names.
    handle       TEXT NOT NULL,
    team_id      TEXT REFERENCES teams(id) ON DELETE SET NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- The same number may be an agent on one call and a contact on another, so
-- identity is the pair rather than the handle alone.
CREATE UNIQUE INDEX idx_people_handle ON people(kind, handle);
CREATE INDEX idx_people_team ON people(team_id);

-- SET NULL rather than the CASCADE used elsewhere: removing someone from the
-- roster must not erase their calls. The recording is the record.
ALTER TABLE calls ADD COLUMN agent_id   TEXT REFERENCES people(id) ON DELETE SET NULL;
ALTER TABLE calls ADD COLUMN contact_id TEXT REFERENCES people(id) ON DELETE SET NULL;

-- When the call happened, as opposed to when it was ingested. `created_at`
-- keeps its meaning and is never overwritten: 1,967 of the 1,972 calls in the
-- reference corpus share one ingest date, which is the whole reason a separate
-- column has to exist.
ALTER TABLE calls ADD COLUMN occurred_at TEXT;

-- none | rule | manual | live. Load-bearing rather than decorative: the
-- backfill skips `manual`, so a correction made by hand survives the next run.
ALTER TABLE calls ADD COLUMN attribution TEXT NOT NULL DEFAULT 'none';

CREATE INDEX idx_calls_agent    ON calls(agent_id, occurred_at);
CREATE INDEX idx_calls_contact  ON calls(contact_id, occurred_at);
CREATE INDEX idx_calls_occurred ON calls(occurred_at DESC);
"#,
    ),
    (
        "0007_roles",
        r#"
-- What an agent is allowed to see.
--
-- Least privilege by default: a row with no role reads as `agent`, which sees
-- only its own calls. A missing or unparseable value must never widen access.
--
-- This is a *view* filter while everything runs against one local file — the
-- operating-system user can open the database directly. It becomes a real
-- boundary only where the scope is applied before data leaves a host, which is
-- what the sync server does.
ALTER TABLE people ADD COLUMN role TEXT NOT NULL DEFAULT 'agent';

CREATE INDEX idx_people_role ON people(role);
"#,
    ),
];

pub fn open(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| CaError::Storage(format!("open {}: {e}", path.display())))?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn =
        Connection::open_in_memory().map_err(|e| CaError::Storage(format!("open memory: {e}")))?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL lets the UI read while a batch writes — without it the dashboard
    // blocks every time the pipeline commits a call.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| CaError::Storage(format!("journal_mode: {e}")))?;
    // NORMAL is the right durability trade for a resumable batch: losing the
    // last few commits after a power cut costs a re-run of those calls, not
    // corruption, and it is markedly faster than FULL.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| CaError::Storage(format!("synchronous: {e}")))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| CaError::Storage(format!("foreign_keys: {e}")))?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name       TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )
    .map_err(|e| CaError::Storage(format!("migrations table: {e}")))?;

    for (name, sql) in MIGRATIONS {
        let done: bool = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE name = ?1",
                [name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if done {
            continue;
        }
        conn.execute_batch(sql)
            .map_err(|e| CaError::Storage(format!("migration {name}: {e}")))?;
        conn.execute(
            "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, ?2)",
            rusqlite::params![name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| CaError::Storage(format!("record migration {name}: {e}")))?;
        tracing::info!(migration = name, "applied");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_every_table() {
        let conn = open_in_memory().unwrap();
        for t in [
            "calls",
            "transcripts",
            "segments",
            "redaction_map",
            "analyses",
            "findings",
            "jobs",
            "channel_timelines",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} missing");
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        // Re-opening an existing database must not re-run migrations.
        let dir = std::env::temp_dir().join(format!("ca-store-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("idem.db");
        let _ = std::fs::remove_file(&path);

        let c1 = open(&path).unwrap();
        drop(c1);
        let c2 = open(&path).unwrap();
        let n: i64 = c2
            .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, MIGRATIONS.len() as i64);

        drop(c2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_local_paths_are_merged_keeping_the_processed_row() {
        // The same recording registered through two entry points: one spelling
        // with forward slashes (the CLI), one with backslashes (the file
        // dialog). Only one of them had audio processed against it.
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            r"INSERT INTO calls (id, source_key, source_name, display_name, media_path, stage, peaks_blob, created_at, updated_at)
             VALUES ('processed', 'C:/x/a.m4a', 'local', 'a.m4a', 'C:/x/a.m4a', 'redacted', x'00', 't1', 't1');
             INSERT INTO calls (id, source_key, source_name, display_name, media_path, stage, created_at, updated_at)
             VALUES ('bare', 'C:\x\a.m4a', 'local', 'a.m4a', 'C:\x\a.m4a', 'discovered', 't2', 't2');",
        )
        .unwrap();

        // Re-running migrations applies 0004 to the rows just inserted.
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = '0004_merge_duplicate_local_paths'",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();

        let ids: Vec<String> = conn
            .prepare("SELECT id FROM calls ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ids, vec!["processed".to_string()], "kept the wrong row");

        let key: String = conn
            .query_row("SELECT source_key FROM calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(key, r"C:\x\a.m4a", "the surviving key was not normalised");
    }

    #[test]
    fn cloud_keys_keep_their_slashes() {
        // A bucket URL is not a path; rewriting its separators would make every
        // object unreachable.
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO calls (id, source_key, source_name, display_name, media_path, stage, created_at, updated_at)
             VALUES ('c1', 'https://b.s3.amazonaws.com/a/b.m4a', 's3:b', 'b.m4a',
                     'https://b.s3.amazonaws.com/a/b.m4a', 'discovered', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = '0004_merge_duplicate_local_paths'",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();

        let (key, path): (String, String) = conn
            .query_row("SELECT source_key, media_path FROM calls", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(key.starts_with("https://"), "got {key}");
        assert!(path.starts_with("https://"), "got {path}");
    }

    #[test]
    fn foreign_keys_cascade_on_delete() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO calls (id, source_key, source_name, display_name, stage, created_at, updated_at)
             VALUES ('c1','k','local','d','discovered','t','t')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO segments (id, call_id, idx, speaker_index, role, start_secs, end_secs, text)
             VALUES ('s1','c1',0,0,'unknown',0,1,'hi')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM calls WHERE id='c1'", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "segments should cascade with their call");
    }

    #[test]
    fn a_source_key_cannot_be_ingested_twice() {
        let conn = open_in_memory().unwrap();
        let ins = "INSERT INTO calls (id, source_key, source_name, display_name, stage, created_at, updated_at)
                   VALUES (?1,'same','local','d','discovered','t','t')";
        conn.execute(ins, ["a"]).unwrap();
        assert!(
            conn.execute(ins, ["b"]).is_err(),
            "duplicate source key must be rejected"
        );
    }

    #[test]
    fn one_analysis_row_per_agent_per_call() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO calls (id, source_key, source_name, display_name, stage, created_at, updated_at)
             VALUES ('c1','k','local','d','discovered','t','t')",
            [],
        )
        .unwrap();
        let ins =
            "INSERT INTO analyses (id, call_id, agent, payload_json, provider, model, created_at)
                   VALUES (?1,'c1','qa_scorecard','{}','p','m','t')";
        conn.execute(ins, ["a1"]).unwrap();
        assert!(conn.execute(ins, ["a2"]).is_err());
    }
}
