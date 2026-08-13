//! Run history: what every batch did, kept after it finished.
//!
//! The pipeline records where a call *currently* is. That single mutable value
//! cannot answer the questions a person actually asks of an ingest screen —
//! what did last night's run do, which calls failed and why, has this file been
//! attempted before. So each run and each call within it gets a row, written as
//! the run happens rather than summarised at the end: a batch that is killed
//! half way through still leaves a readable record of the half it did.

use crate::Store;
use ca_core::error::{CaError, Result};
use ca_core::ids::{CallId, JobId};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A batch, as the history list shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub target_stage: String,
    pub limit_n: Option<i64>,
    pub concurrency: i64,
    pub total: i64,
    pub processed: i64,
    pub failed: i64,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Rows actually recorded, which is what the UI pages through. `total` is
    /// what the run set out to do and the two differ when it was interrupted.
    pub item_count: i64,
}

/// One call's outcome inside one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunItem {
    pub run_id: String,
    pub call_id: String,
    pub name: String,
    pub stage: String,
    pub status: String,
    pub secs: Option<f64>,
    pub error: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Store {
    /// Open a run and return its id. Written before any work starts, so a
    /// process killed on its first call still leaves evidence it tried.
    pub fn start_run(
        &self,
        kind: &str,
        label: &str,
        target_stage: &str,
        limit_n: Option<usize>,
        concurrency: usize,
    ) -> Result<JobId> {
        let id = JobId::new();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO runs (id, kind, label, target_stage, limit_n, concurrency,
                               total, processed, failed, status, started_at)
             VALUES (?1,?2,?3,?4,?5,?6,0,0,0,'running',?7)",
            params![
                id.as_str(),
                kind,
                label,
                target_stage,
                limit_n.map(|n| n as i64),
                concurrency as i64,
                now(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("start run: {e}")))?;
        Ok(id)
    }

    /// Record how many calls the run intends to process.
    pub fn set_run_total(&self, run: &JobId, total: usize) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE runs SET total = ?2 WHERE id = ?1",
            params![run.as_str(), total as i64],
        )
        .map_err(|e| CaError::Storage(format!("run total: {e}")))?;
        Ok(())
    }

    /// A call entered the run.
    pub fn run_item_started(&self, run: &JobId, call: &CallId, name: &str) -> Result<()> {
        let ts = now();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO run_items (run_id, call_id, name, stage, status, started_at, updated_at)
             VALUES (?1,?2,?3,'queued','running',?4,?4)
             ON CONFLICT(run_id, call_id) DO UPDATE SET
                name=excluded.name, status='running', updated_at=excluded.updated_at",
            params![run.as_str(), call.as_str(), name, ts],
        )
        .map_err(|e| CaError::Storage(format!("run item start: {e}")))?;
        Ok(())
    }

    /// A call advanced. Recorded per stage so an interrupted run still shows
    /// how far each call actually got.
    pub fn run_item_stage(&self, run: &JobId, call: &CallId, stage: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE run_items SET stage = ?3, updated_at = ?4
             WHERE run_id = ?1 AND call_id = ?2",
            params![run.as_str(), call.as_str(), stage, now()],
        )
        .map_err(|e| CaError::Storage(format!("run item stage: {e}")))?;
        Ok(())
    }

    pub fn run_item_finished(&self, run: &JobId, call: &CallId, secs: Option<f64>) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE run_items SET status = 'done', secs = ?3, updated_at = ?4
             WHERE run_id = ?1 AND call_id = ?2",
            params![run.as_str(), call.as_str(), secs, now()],
        )
        .map_err(|e| CaError::Storage(format!("run item finish: {e}")))?;
        Ok(())
    }

    pub fn run_item_failed(&self, run: &JobId, call: &CallId, error: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE run_items SET status = 'failed', error = ?3, updated_at = ?4
             WHERE run_id = ?1 AND call_id = ?2",
            params![run.as_str(), call.as_str(), error, now()],
        )
        .map_err(|e| CaError::Storage(format!("run item fail: {e}")))?;
        Ok(())
    }

    pub fn finish_run(&self, run: &JobId, processed: usize, failed: usize) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE runs SET status = 'done', processed = ?2, failed = ?3, finished_at = ?4
             WHERE id = ?1",
            params![run.as_str(), processed as i64, failed as i64, now()],
        )
        .map_err(|e| CaError::Storage(format!("finish run: {e}")))?;
        Ok(())
    }

    /// Mark runs that were still `running` when the process died.
    ///
    /// Called at startup. Without it a killed batch stays "running" for ever
    /// and the history claims work is in flight that no process is doing.
    pub fn close_stale_runs(&self) -> Result<usize> {
        let ts = now();
        let conn = self.lock()?;
        let runs = conn
            .execute(
                "UPDATE runs SET status = 'interrupted', finished_at = ?1
                 WHERE status = 'running'",
                params![ts],
            )
            .map_err(|e| CaError::Storage(format!("close stale runs: {e}")))?;
        // Their in-flight calls stopped too; leaving them 'running' would show
        // a spinner against a call nothing is working on.
        conn.execute(
            "UPDATE run_items SET status = 'interrupted', updated_at = ?1
             WHERE status = 'running'",
            params![ts],
        )
        .map_err(|e| CaError::Storage(format!("close stale items: {e}")))?;
        Ok(runs)
    }

    /// Most recent runs first.
    pub fn list_runs(&self, offset: usize, limit: usize) -> Result<Vec<RunSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT r.id, r.kind, r.label, r.target_stage, r.limit_n, r.concurrency,
                        r.total, r.processed, r.failed, r.status, r.started_at, r.finished_at,
                        (SELECT count(*) FROM run_items i WHERE i.run_id = r.id)
                 FROM runs r
                 ORDER BY r.started_at DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| CaError::Storage(format!("prepare runs: {e}")))?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(RunSummary {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    label: r.get(2)?,
                    target_stage: r.get(3)?,
                    limit_n: r.get(4)?,
                    concurrency: r.get(5)?,
                    total: r.get(6)?,
                    processed: r.get(7)?,
                    failed: r.get(8)?,
                    status: r.get(9)?,
                    started_at: r.get(10)?,
                    finished_at: r.get(11)?,
                    item_count: r.get(12)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query runs: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("run row: {e}")))?);
        }
        Ok(out)
    }

    pub fn run_of(&self, run: &JobId) -> Result<Option<RunSummary>> {
        Ok(self
            .list_runs(0, 10_000)?
            .into_iter()
            .find(|r| r.id == run.as_str()))
    }

    /// One run's calls, newest activity first, paged.
    pub fn run_items(&self, run: &JobId, offset: usize, limit: usize) -> Result<Vec<RunItem>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT run_id, call_id, name, stage, status, secs, error, started_at, updated_at
                 FROM run_items
                 WHERE run_id = ?1
                 ORDER BY updated_at DESC, call_id
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| CaError::Storage(format!("prepare run items: {e}")))?;

        let rows = stmt
            .query_map(params![run.as_str(), limit as i64, offset as i64], |r| {
                Ok(RunItem {
                    run_id: r.get(0)?,
                    call_id: r.get(1)?,
                    name: r.get(2)?,
                    stage: r.get(3)?,
                    status: r.get(4)?,
                    secs: r.get(5)?,
                    error: r.get(6)?,
                    started_at: r.get(7)?,
                    updated_at: r.get(8)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query run items: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("run item row: {e}")))?);
        }
        Ok(out)
    }

    /// How many times this call has been attempted, across every run.
    pub fn call_attempts(&self, call: &CallId) -> Result<i64> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT count(*) FROM run_items WHERE call_id = ?1",
            params![call.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map(|v| v.unwrap_or(0))
        .map_err(|e| CaError::Storage(format!("call attempts: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    fn run(s: &Store) -> JobId {
        s.start_run("transcribe", "Downloads/calls", "redacted", Some(20), 2)
            .unwrap()
    }

    #[test]
    fn a_run_is_visible_the_moment_it_starts() {
        // Written before any work, so a process killed on the first call still
        // leaves evidence that it tried.
        let s = store();
        let id = run(&s);
        let runs = s.list_runs(0, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, id.as_str());
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].item_count, 0);
    }

    #[test]
    fn an_items_progress_is_recorded_stage_by_stage() {
        let s = store();
        let id = run(&s);
        let call = CallId::from("call_a");

        s.run_item_started(&id, &call, "a.m4a").unwrap();
        s.run_item_stage(&id, &call, "decoded").unwrap();
        s.run_item_stage(&id, &call, "transcribed").unwrap();
        s.run_item_finished(&id, &call, Some(42.0)).unwrap();

        let items = s.run_items(&id, 0, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].stage, "transcribed");
        assert_eq!(items[0].status, "done");
        assert_eq!(items[0].secs, Some(42.0));
    }

    #[test]
    fn a_failure_keeps_the_stage_it_died_at() {
        // "Failed at decoded" is actionable; "failed" alone is not.
        let s = store();
        let id = run(&s);
        let call = CallId::from("call_b");
        s.run_item_started(&id, &call, "b.m4a").unwrap();
        s.run_item_stage(&id, &call, "decoded").unwrap();
        s.run_item_failed(&id, &call, "unsupported codec").unwrap();

        let items = s.run_items(&id, 0, 10).unwrap();
        assert_eq!(items[0].stage, "decoded");
        assert_eq!(items[0].status, "failed");
        assert_eq!(items[0].error.as_deref(), Some("unsupported codec"));
    }

    #[test]
    fn history_survives_the_call_being_deleted() {
        // The point of a history: it records what happened, and removing a
        // call from the corpus must not rewrite it.
        let s = store();
        let id = run(&s);
        let call = CallId::from("call_gone");
        s.run_item_started(&id, &call, "gone.m4a").unwrap();
        s.run_item_finished(&id, &call, Some(9.0)).unwrap();

        {
            let conn = s.lock().unwrap();
            conn.execute("DELETE FROM calls WHERE id = ?1", params![call.as_str()])
                .unwrap();
        }

        assert_eq!(s.run_items(&id, 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn a_killed_run_does_not_stay_running_for_ever() {
        let s = store();
        let id = run(&s);
        let call = CallId::from("call_c");
        s.run_item_started(&id, &call, "c.m4a").unwrap();

        assert_eq!(s.close_stale_runs().unwrap(), 1);

        let runs = s.list_runs(0, 10).unwrap();
        assert_eq!(runs[0].status, "interrupted");
        assert!(runs[0].finished_at.is_some());
        // And nothing is left showing a spinner.
        let items = s.run_items(&id, 0, 10).unwrap();
        assert_eq!(items[0].status, "interrupted");
    }

    #[test]
    fn closing_stale_runs_leaves_finished_ones_alone() {
        let s = store();
        let id = run(&s);
        s.finish_run(&id, 3, 1).unwrap();
        assert_eq!(s.close_stale_runs().unwrap(), 0);

        let runs = s.list_runs(0, 10).unwrap();
        assert_eq!(runs[0].status, "done");
        assert_eq!(runs[0].processed, 3);
        assert_eq!(runs[0].failed, 1);
    }

    #[test]
    fn runs_are_listed_newest_first_and_page() {
        let s = store();
        for i in 0..5 {
            let id = s
                .start_run("transcribe", &format!("batch {i}"), "redacted", None, 2)
                .unwrap();
            s.finish_run(&id, 1, 0).unwrap();
        }
        let all = s.list_runs(0, 10).unwrap();
        assert_eq!(all.len(), 5);
        let page = s.list_runs(2, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, all[2].id);
    }

    #[test]
    fn re_entering_a_call_does_not_duplicate_its_row() {
        // A resumed run re-emits `started` for calls it picks up again.
        let s = store();
        let id = run(&s);
        let call = CallId::from("call_d");
        s.run_item_started(&id, &call, "d.m4a").unwrap();
        s.run_item_started(&id, &call, "d.m4a").unwrap();
        assert_eq!(s.run_items(&id, 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn attempts_are_counted_across_runs() {
        let s = store();
        let call = CallId::from("call_e");
        for _ in 0..3 {
            let id = run(&s);
            s.run_item_started(&id, &call, "e.m4a").unwrap();
        }
        assert_eq!(s.call_attempts(&call).unwrap(), 3);
        assert_eq!(s.call_attempts(&CallId::from("never")).unwrap(), 0);
    }
}
