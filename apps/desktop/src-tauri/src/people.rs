//! Participant identity, for the People screen and the call-detail assignment.
//!
//! Thin over `ca-store` and `ca-insights`, like every other command module
//! here. Nothing in this file reaches a network: identity is local data.

use ca_core::identity::{AttributionSource, PersonKind};
use ca_core::ids::{CallId, PersonId};
use ca_store::{AttributeOptions, AttributeReport, CallPeople, PersonSummary, Store};
use std::sync::Arc;
use tauri::State;

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
pub fn get_person(
    store: State<'_, Arc<Store>>,
    person_id: String,
) -> Result<ca_store::PersonDetail, String> {
    let id = PersonId::from(person_id);
    store
        .person_detail(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no such person: {}", id.as_str()))
}

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

// ------------------------------------------------------------------- reset

/// What a reset would remove, without removing it.
///
/// The confirmation dialog is only meaningful if it can name real numbers, so
/// the UI asks for this before offering the button.
#[tauri::command]
pub fn preview_reset(
    store: State<'_, Arc<Store>>,
    scope: String,
) -> Result<ca_store::ResetReport, String> {
    let scope =
        ca_store::ResetScope::parse(&scope).ok_or_else(|| format!("unknown scope: {scope}"))?;
    store.reset_preview(scope).map_err(|e| e.to_string())
}

/// Clear the corpus.
///
/// Copies the database first unless told not to. A failed backup aborts rather
/// than proceeding — an unbacked-up reset is a materially riskier operation
/// than the one the user agreed to.
#[tauri::command]
pub fn reset_data(
    store: State<'_, Arc<Store>>,
    scope: String,
    backup: bool,
) -> Result<ResetOutcome, String> {
    let scope =
        ca_store::ResetScope::parse(&scope).ok_or_else(|| format!("unknown scope: {scope}"))?;

    let backup_path = if backup {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        // The store's own file, not the default path: those differ whenever
        // anything was opened with an explicit database, and copying the wrong
        // one looks like it worked.
        store
            .backup(&stamp)
            .map_err(|e| format!("nothing was deleted — the backup failed: {e}"))?
            .map(|p| p.display().to_string())
    } else {
        None
    };

    let report = store.reset(scope).map_err(|e| e.to_string())?;
    Ok(ResetOutcome {
        report,
        backup_path,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetOutcome {
    pub report: ca_store::ResetReport,
    /// Where the copy went, so the user can find it if the reset was a mistake.
    pub backup_path: Option<String>,
}
