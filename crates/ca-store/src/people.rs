//! The people on either side of a call.
//!
//! Local-only, like [`crate::Store::save_redaction_map`]: a contact's name and
//! number are the same data the pipeline redacts before a provider sees a
//! transcript.

use ca_core::error::{CaError, Result};
use ca_core::identity::{AttributionSource, Person, PersonKind};
use ca_core::ids::{CallId, PersonId, TeamId};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::Store;

/// A person plus the counts the list view needs, so the screen is one query
/// rather than one per row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonSummary {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub handle: String,
    pub calls: usize,
    /// First and last call by `occurred_at`. `None` when no call of theirs
    /// carries a time, which is different from having no calls.
    pub first_call: Option<String>,
    pub last_call: Option<String>,
}

impl Store {
    /// Find or create a person. Identity is `(kind, handle)`.
    ///
    /// Returning the existing row rather than erroring is what makes the
    /// backfill idempotent: a second run over the same corpus finds the same
    /// people instead of duplicating all 771 of them.
    pub fn upsert_person(
        &self,
        kind: PersonKind,
        display_name: &str,
        handle: &str,
    ) -> Result<PersonId> {
        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM people WHERE kind = ?1 AND handle = ?2",
                params![kind.as_str(), handle],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("lookup person: {e}")))?;

        if let Some(id) = existing {
            return Ok(PersonId::from(id));
        }

        let id = PersonId::new();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO people (id, kind, display_name, handle, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?5)",
            params![id.as_str(), kind.as_str(), display_name, handle, now],
        )
        .map_err(|e| CaError::Storage(format!("insert person: {e}")))?;
        Ok(id)
    }

    pub fn person(&self, id: &PersonId) -> Result<Option<Person>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, kind, display_name, handle, team_id, created_at, updated_at
               FROM people WHERE id = ?1",
            [id.as_str()],
            |r| {
                let kind: String = r.get(1)?;
                let team: Option<String> = r.get(4)?;
                let created: String = r.get(5)?;
                let updated: String = r.get(6)?;
                Ok(Person {
                    id: PersonId::from(r.get::<_, String>(0)?),
                    kind: PersonKind::parse(&kind).unwrap_or(PersonKind::Contact),
                    display_name: r.get(2)?,
                    handle: r.get(3)?,
                    team_id: team.map(TeamId::from),
                    created_at: parse_time(&created),
                    updated_at: parse_time(&updated),
                })
            },
        )
        .optional()
        .map_err(|e| CaError::Storage(format!("load person: {e}")))
    }

    /// People with their call counts, busiest first.
    pub fn list_people(
        &self,
        kind: Option<PersonKind>,
        search: Option<&str>,
    ) -> Result<Vec<PersonSummary>> {
        let conn = self.lock()?;
        let kind_filter = kind.map(|k| k.as_str().to_string()).unwrap_or_default();
        let like = format!("%{}%", search.unwrap_or(""));

        let mut stmt = conn
            .prepare(
                // A person may be on either side of a call, so every count
                // matches on both columns rather than assuming a side.
                "SELECT p.id, p.kind, p.display_name, p.handle,
                        (SELECT count(*) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id),
                        (SELECT min(c.occurred_at) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id),
                        (SELECT max(c.occurred_at) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id)
                   FROM people p
                  WHERE (?1 = '' OR p.kind = ?1)
                    AND (?2 = '%%' OR p.display_name LIKE ?2 OR p.handle LIKE ?2)
                  ORDER BY 5 DESC, p.display_name ASC",
            )
            .map_err(|e| CaError::Storage(format!("prepare people: {e}")))?;

        let rows = stmt
            .query_map(params![kind_filter, like], |r| {
                Ok(PersonSummary {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    display_name: r.get(2)?,
                    handle: r.get(3)?,
                    calls: r.get::<_, i64>(4)?.max(0) as usize,
                    first_call: r.get(5)?,
                    last_call: r.get(6)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query people: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read people: {e}")))
    }

    /// Attach participants and the real call time to a call.
    pub fn set_call_participants(
        &self,
        call_id: &CallId,
        agent: Option<&PersonId>,
        contact: Option<&PersonId>,
        occurred_at: Option<DateTime<Utc>>,
        source: AttributionSource,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calls
                SET agent_id = ?2, contact_id = ?3, occurred_at = ?4,
                    attribution = ?5, updated_at = ?6
              WHERE id = ?1",
            params![
                call_id.as_str(),
                agent.map(|p| p.as_str()),
                contact.map(|p| p.as_str()),
                occurred_at.map(|t| t.to_rfc3339()),
                source.as_str(),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("set participants: {e}")))?;
        Ok(())
    }

    /// Change only who was on a call, leaving its time alone.
    ///
    /// Separate from [`Store::set_call_participants`] on purpose. A hand-made
    /// reassignment knows the people but not the time, and routing it through
    /// the other method would pass `None` and erase an `occurred_at` that had
    /// been correctly derived from the filename.
    pub fn assign_call_people(
        &self,
        call_id: &CallId,
        agent: Option<&PersonId>,
        contact: Option<&PersonId>,
        source: AttributionSource,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calls
                SET agent_id = ?2, contact_id = ?3, attribution = ?4, updated_at = ?5
              WHERE id = ?1",
            params![
                call_id.as_str(),
                agent.map(|p| p.as_str()),
                contact.map(|p| p.as_str()),
                source.as_str(),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("assign people: {e}")))?;
        Ok(())
    }

    pub fn call_attribution(&self, call_id: &CallId) -> Result<AttributionSource> {
        let conn = self.lock()?;
        let s: Option<String> = conn
            .query_row(
                "SELECT attribution FROM calls WHERE id = ?1",
                [call_id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("call attribution: {e}")))?;
        Ok(s.and_then(|s| AttributionSource::parse(&s))
            .unwrap_or_default())
    }

    /// Every call this person was on, in the shape the rollups take.
    ///
    /// The measures come out of the stored payloads with `json_extract`, the
    /// same way [`Store::list_calls`] reads them, and each is scoped to
    /// `error IS NULL` so a failed agent leaves the measure absent rather than
    /// extracting from an error envelope.
    pub fn person_call_refs(&self, id: &PersonId) -> Result<Vec<ca_insights::CallRef>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.contact_id, c.occurred_at,
                        (SELECT json_extract(a.payload_json, '$.overall')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'qa_scorecard'
                            AND a.error IS NULL),
                        (SELECT json_extract(a.payload_json, '$.overall')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'sentiment'
                            AND a.error IS NULL),
                        (SELECT json_extract(a.payload_json, '$.passed')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'compliance'
                            AND a.error IS NULL)
                   FROM calls c
                  WHERE c.agent_id = ?1 OR c.contact_id = ?1
                  ORDER BY c.occurred_at ASC",
            )
            .map_err(|e| CaError::Storage(format!("prepare person calls: {e}")))?;

        let rows = stmt
            .query_map([id.as_str()], |r| {
                let when: Option<String> = r.get(2)?;
                let passed: Option<i64> = r.get(5)?;
                Ok(ca_insights::CallRef {
                    call_id: r.get(0)?,
                    contact_id: r.get(1)?,
                    occurred_at: when.as_deref().and_then(|s| {
                        DateTime::parse_from_rfc3339(s)
                            .ok()
                            .map(|t| t.with_timezone(&Utc))
                    }),
                    qa_overall: r.get::<_, Option<f64>>(3)?.map(|v| v as f32),
                    sentiment: r.get::<_, Option<f64>>(4)?.map(|v| v as f32),
                    compliance_passed: passed.map(|v| v != 0),
                })
            })
            .map_err(|e| CaError::Storage(format!("query person calls: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read person calls: {e}")))
    }
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Who was on one call, and where that call sits in the contact's history.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallPeople {
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub contact_id: Option<String>,
    pub contact_name: Option<String>,
    /// none | rule | manual | live.
    pub attribution: String,
    /// Position in this contact's run of calls, oldest first. `1` is a first
    /// contact; anything higher is a repeat. `None` when there is no contact
    /// or no call time to order by.
    pub sequence: Option<usize>,
    /// How many calls this contact has in total.
    pub contact_calls: Option<usize>,
}

/// A person, everything their calls add up to, and the calls themselves.
///
/// Composed here rather than in the command layer so the desktop app and the
/// dev bridge cannot drift into showing different numbers for the same person.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonDetail {
    pub summary: PersonSummary,
    pub facts: ca_insights::PersonFacts,
    pub qa_trend: Vec<ca_insights::MonthPoint>,
    pub sentiment_trend: Vec<ca_insights::MonthPoint>,
    /// `None` when no call carries a time — unanswerable, not zero.
    pub repeat_contact_rate: Option<f32>,
    /// Newest first.
    pub calls: Vec<PersonCall>,
}

/// One row in a person's call list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonCall {
    pub call_id: String,
    pub occurred_at: Option<String>,
    pub qa_overall: Option<f32>,
    pub sentiment: Option<f32>,
    /// Position in this contact's run of calls, oldest first. `1` is a first
    /// contact; anything above it is a repeat.
    pub sequence: usize,
}

/// A contact who calls again inside a month is a repeat.
///
/// A lifetime definition would climb forever and read as a worsening trend
/// when nothing had changed, so the window is fixed and stated.
pub const REPEAT_WINDOW_DAYS: i64 = 30;

/// The identity columns on `calls`, named so the row type stays readable.
struct CallRow {
    agent_id: Option<String>,
    contact_id: Option<String>,
    attribution: String,
    occurred_at: Option<String>,
}

impl Store {
    /// Everything the person screen draws, in one call.
    pub fn person_detail(&self, id: &PersonId) -> Result<Option<PersonDetail>> {
        let Some(summary) = self
            .list_people(None, None)?
            .into_iter()
            .find(|p| p.id == id.as_str())
        else {
            return Ok(None);
        };

        // Ordered oldest first by the query, which is what makes `sequence`
        // simply the position in the list.
        let refs = self.person_call_refs(id)?;
        let mut calls: Vec<PersonCall> = refs
            .iter()
            .enumerate()
            .map(|(i, c)| PersonCall {
                call_id: c.call_id.clone(),
                occurred_at: c.occurred_at.map(|t| t.to_rfc3339()),
                qa_overall: c.qa_overall,
                sentiment: c.sentiment,
                sequence: i + 1,
            })
            .collect();
        calls.reverse();

        Ok(Some(PersonDetail {
            facts: ca_insights::person_facts(id.as_str(), &refs),
            qa_trend: ca_insights::monthly_trend(&refs, |c| c.qa_overall),
            sentiment_trend: ca_insights::monthly_trend(&refs, |c| c.sentiment),
            repeat_contact_rate: ca_insights::repeat_contact_rate(&refs, REPEAT_WINDOW_DAYS),
            summary,
            calls,
        }))
    }

    /// The participants on a call, for the detail screen.
    pub fn call_people(&self, call_id: &CallId) -> Result<CallPeople> {
        let conn = self.lock()?;
        let row: Option<CallRow> = conn
            .query_row(
                "SELECT c.agent_id, c.contact_id, c.attribution, c.occurred_at
                   FROM calls c WHERE c.id = ?1",
                [call_id.as_str()],
                |r| {
                    Ok(CallRow {
                        agent_id: r.get(0)?,
                        contact_id: r.get(1)?,
                        attribution: r.get(2)?,
                        occurred_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("call people: {e}")))?;

        let Some(CallRow {
            agent_id,
            contact_id,
            attribution,
            occurred_at,
        }) = row
        else {
            return Ok(CallPeople::default());
        };

        let name = |id: &Option<String>| -> Result<Option<String>> {
            let Some(id) = id else { return Ok(None) };
            conn.query_row(
                "SELECT display_name FROM people WHERE id = ?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("person name: {e}")))
        };

        // Counting calls strictly earlier than this one makes the position
        // stable: it does not shift when a later call is added, which a
        // "3 of 5" phrasing would.
        let (sequence, contact_calls) = match (&contact_id, &occurred_at) {
            (Some(cid), Some(when)) => {
                let earlier: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM calls
                          WHERE contact_id = ?1 AND occurred_at IS NOT NULL
                            AND occurred_at < ?2",
                        params![cid, when],
                        |r| r.get(0),
                    )
                    .map_err(|e| CaError::Storage(format!("sequence: {e}")))?;
                let total: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM calls WHERE contact_id = ?1",
                        [cid.as_str()],
                        |r| r.get(0),
                    )
                    .map_err(|e| CaError::Storage(format!("contact calls: {e}")))?;
                (
                    Some(earlier.max(0) as usize + 1),
                    Some(total.max(0) as usize),
                )
            }
            _ => (None, None),
        };

        Ok(CallPeople {
            agent_name: name(&agent_id)?,
            contact_name: name(&contact_id)?,
            agent_id,
            contact_id,
            attribution,
            sequence,
            contact_calls,
        })
    }
}
