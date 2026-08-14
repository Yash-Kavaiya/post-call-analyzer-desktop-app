//! Participant identity, for the People screen and the call-detail assignment.
//!
//! Thin over `ca-store` and `ca-insights`, like every other command module
//! here. Nothing in this file reaches a network: identity is local data.

use ca_core::identity::{AttributionSource, PersonKind};
use ca_core::ids::{CallId, PersonId};
use ca_store::{AttributeOptions, AttributeReport, CallPeople, PersonSummary, Store};
use std::sync::Arc;
use tauri::State;

/// A person plus everything the detail screen draws, in one round trip.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonDetail {
    pub summary: PersonSummary,
    pub facts: ca_insights::PersonFacts,
    pub qa_trend: Vec<ca_insights::MonthPoint>,
    pub sentiment_trend: Vec<ca_insights::MonthPoint>,
    /// `None` when no call carries a time — the question is unanswerable, not
    /// zero.
    pub repeat_contact_rate: Option<f32>,
    /// This person's calls, newest first.
    pub calls: Vec<PersonCall>,
}

/// One row in a person's call list.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonCall {
    pub call_id: String,
    pub occurred_at: Option<String>,
    pub qa_overall: Option<f32>,
    pub sentiment: Option<f32>,
    /// Which call this is in the run of calls with this contact, oldest first.
    /// `1` is a first contact; anything above it is a repeat.
    pub sequence: usize,
}

#[tauri::command]
pub fn list_people(
    store: State<'_, Arc<Store>>,
    kind: Option<String>,
    search: Option<String>,
) -> Result<Vec<PersonSummary>, String> {
    let kind = kind.as_deref().and_then(PersonKind::parse);
    store
        .list_people(kind, search.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_person(store: State<'_, Arc<Store>>, person_id: String) -> Result<PersonDetail, String> {
    let id = PersonId::from(person_id);
    let summary = store
        .list_people(None, None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.id == id.as_str())
        .ok_or_else(|| format!("no such person: {}", id.as_str()))?;

    // Ordered oldest first by the query, which is what makes `sequence` simply
    // the position in the list.
    let refs = store.person_call_refs(&id).map_err(|e| e.to_string())?;

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

    Ok(PersonDetail {
        facts: ca_insights::person_facts(id.as_str(), &refs),
        qa_trend: ca_insights::monthly_trend(&refs, |c| c.qa_overall),
        sentiment_trend: ca_insights::monthly_trend(&refs, |c| c.sentiment),
        repeat_contact_rate: ca_insights::repeat_contact_rate(&refs, REPEAT_WINDOW_DAYS),
        summary,
        calls,
    })
}

/// A contact who calls again inside a month is a repeat.
///
/// A lifetime definition would climb forever and read as a worsening trend when
/// nothing had changed, so the window is fixed and stated rather than implied.
const REPEAT_WINDOW_DAYS: i64 = 30;

#[tauri::command]
pub fn attribute_calls(
    store: State<'_, Arc<Store>>,
    dry_run: bool,
    force: bool,
) -> Result<AttributeReport, String> {
    store
        .attribute_calls(
            &ca_identity::builtin_patterns(),
            AttributeOptions { dry_run, force },
        )
        .map_err(|e| e.to_string())
}

/// Who was on one call, for the detail screen.
#[tauri::command]
pub fn get_call_people(
    store: State<'_, Arc<Store>>,
    call_id: String,
) -> Result<CallPeople, String> {
    store
        .call_people(&CallId::from(call_id))
        .map_err(|e| e.to_string())
}

/// Assign participants by hand.
///
/// Sets provenance to `manual`, which is what protects the choice from the next
/// backfill. A blank name on either side clears that side.
#[tauri::command]
pub fn assign_call_person(
    store: State<'_, Arc<Store>>,
    call_id: String,
    agent_name: Option<String>,
    contact_name: Option<String>,
) -> Result<(), String> {
    let agent = resolve(&store, PersonKind::Agent, agent_name.as_deref())?;
    let contact = resolve(&store, PersonKind::Contact, contact_name.as_deref())?;

    store
        .assign_call_people(
            &CallId::from(call_id),
            agent.as_ref(),
            contact.as_ref(),
            AttributionSource::Manual,
        )
        .map_err(|e| e.to_string())
}

/// Find or create a person from a typed name.
///
/// Going through the same normalisation the rules use means typing a name that
/// already exists selects it rather than creating a near-duplicate.
fn resolve(
    store: &Store,
    kind: PersonKind,
    name: Option<&str>,
) -> Result<Option<PersonId>, String> {
    let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let handle = ca_identity::normalise_handle(name);
    store
        .upsert_person(kind, name, &handle)
        .map(Some)
        .map_err(|e| e.to_string())
}
