//! Plugin results, running custom agents, and the collector's rollup.
//!
//! Three surfaces that close the same gap in different places: work the
//! product already did that nothing rendered. Plugin analyses were written and
//! never shown; plugins could only be run from the CLI; the sync collector
//! received metrics and had no reader outside the terminal.
//!
//! The sync token goes to the OS credential store, like every other secret
//! here, and is never returned to the UI — the screen is told only whether one
//! is present.

use ca_config::{AppSettings, SETTINGS_KEY};
use ca_core::ids::CallId;
use ca_llm::secrets;
use ca_store::{PluginAnalysis, Store};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

/// Name the sync token is filed under in the credential store.
const SYNC_TOKEN: &str = "sync-token";
/// Settings key holding the peer URL. Not a secret, so it lives in the db.
const SYNC_URL_KEY: &str = "sync_peer_url";

/// Every custom agent's result for a call.
#[tauri::command]
pub fn get_plugin_analyses(
    store: State<'_, Arc<Store>>,
    call_id: String,
) -> Result<Vec<PluginAnalysis>, String> {
    store
        .plugin_analyses(&CallId::from(call_id))
        .map_err(|e| e.to_string())
}

/// Where this machine syncs to, and whether a token is stored.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSettings {
    pub url: Option<String>,
    /// Whether a token exists. The value itself never crosses this boundary.
    pub has_token: bool,
}

#[tauri::command]
pub fn get_sync_settings(store: State<'_, Arc<Store>>) -> Result<SyncSettings, String> {
    Ok(SyncSettings {
        url: store.get_setting(SYNC_URL_KEY).map_err(|e| e.to_string())?,
        has_token: secrets::read_named(SYNC_TOKEN).is_some(),
    })
}

/// Save the peer, and the token if one was typed.
///
/// An absent `token` leaves the stored one alone, so re-saving the URL does
/// not silently wipe a working token. An empty string clears it.
#[tauri::command]
pub fn save_sync_settings(
    store: State<'_, Arc<Store>>,
    url: Option<String>,
    token: Option<String>,
) -> Result<SyncSettings, String> {
    match url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => store
            .put_setting(SYNC_URL_KEY, &u.to_string())
            .map_err(|e| e.to_string())?,
        None => store.delete_setting(SYNC_URL_KEY).map_err(|e| e.to_string())?,
    }
    if let Some(token) = token {
        secrets::store_named(SYNC_TOKEN, &token).map_err(|e| e.to_string())?;
    }
    get_sync_settings(store)
}

fn transport(store: &Store) -> Result<ca_sync::HttpTransport, String> {
    let url: Option<String> = store.get_setting(SYNC_URL_KEY).map_err(|e| e.to_string())?;
    let url = url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .ok_or("no collector configured")?;
    let token = secrets::read_named(SYNC_TOKEN).ok_or("no sync token saved")?;
    ca_sync::HttpTransport::parse(&url, token).map_err(|e| e.to_string())
}

/// Push what has changed to the collector.
///
/// Push-only, like the CLI default: this machine produces calls, and pulling
/// would fetch its own records straight back.
#[tauri::command]
pub fn run_sync(store: State<'_, Arc<Store>>) -> Result<ca_sync::SyncOutcome, String> {
    let transport = transport(&store)?;
    ca_sync::SyncPeer::new((*store).clone(), "desktop", Box::new(transport))
        .sync_with(ca_sync::Direction::Push)
        .map_err(|e| e.to_string())
}

/// What the collector holds for this tenant.
#[tauri::command]
pub fn get_sync_report(store: State<'_, Arc<Store>>) -> Result<ca_sync::Rollup, String> {
    transport(&store)?.report(100_000).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- plugins

/// Where this install reads plugin files from — beside the database, same
/// place the CLI uses, so a file dropped for one is visible to the other.
fn plugin_dir() -> PathBuf {
    let db = ca_store::default_db_path();
    let base = db.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    ca_plugins::default_plugin_dir(&base)
}

/// One file in the plugins directory, whether it loaded or not.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub name: Option<String>,
    pub title: Option<String>,
    pub path: String,
    pub findings: bool,
    pub error: Option<String>,
}

/// The directory and everything in it. The path is shown so a person knows
/// where to drop a file without opening the docs.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsSnapshot {
    pub dir: String,
    pub plugins: Vec<PluginInfo>,
}

#[tauri::command]
pub fn list_plugins() -> Result<PluginsSnapshot, String> {
    let dir = plugin_dir();
    let loaded = ca_plugins::load_dir(&dir).map_err(|e| e.to_string())?;
    Ok(PluginsSnapshot {
        dir: dir.display().to_string(),
        plugins: loaded
            .into_iter()
            .map(|p| PluginInfo {
                name: p.spec.as_ref().map(|s| s.name.clone()),
                title: p.spec.as_ref().map(|s| s.title.clone()),
                path: p.path,
                findings: p.spec.as_ref().is_some_and(|s| s.findings),
                error: p.error,
            })
            .collect(),
    })
}

/// What one run of the loaded plugins did on a call.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRunOutcome {
    pub ran: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Run every usable plugin against one call, using the Settings provider.
///
/// Same rules as `ca plugin-run`: redacted transcript only, a prior success
/// is skipped unless `force`, and a failure is recorded rather than silent.
#[tauri::command]
pub async fn run_plugins(
    store: State<'_, Arc<Store>>,
    call_id: String,
    force: bool,
) -> Result<PluginRunOutcome, String> {
    let store = (*store).clone();
    let id = CallId::from(call_id);

    let cfg: AppSettings = store
        .get_setting(SETTINGS_KEY)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    cfg.validate()?;

    let (key, _) = secrets::resolve_key(cfg.provider);
    if cfg.provider.requires_api_key() && key.is_none() {
        return Err(format!(
            "No API key for {}. Add one in Settings before running custom agents.",
            cfg.provider.label()
        ));
    }
    let llm = ca_llm::build_provider(&cfg.spec(), key.as_deref()).map_err(|e| e.to_string())?;

    let loaded = ca_plugins::load_dir(&plugin_dir()).map_err(|e| e.to_string())?;
    let specs: Vec<ca_plugins::AgentSpec> =
        ca_plugins::usable(&loaded).into_iter().cloned().collect();
    if specs.is_empty() {
        return Err(format!(
            "no usable plugins in {}",
            plugin_dir().display()
        ));
    }

    let Some(transcript) = store.load_transcript(&id).map_err(|e| e.to_string())? else {
        return Err("this call has no transcript yet — process it first".into());
    };

    let mut ran = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for spec in specs {
        if !force
            && store
                .plugin_analyses(&id)
                .map_err(|e| e.to_string())?
                .iter()
                .any(|a| a.plugin == spec.name && a.error.is_none())
        {
            skipped += 1;
            continue;
        }

        match ca_plugins::run_plugin(&spec, llm.as_ref(), &transcript).await {
            Ok((result, resp)) => {
                store
                    .save_plugin_analysis(
                        &id,
                        &spec.name,
                        &result.payload,
                        cfg.provider.as_str(),
                        &cfg.model,
                        &ca_core::analysis::Usage {
                            input_tokens: resp.input_tokens,
                            output_tokens: resp.output_tokens,
                            cost_usd: None,
                            latency_ms: resp.latency_ms,
                        },
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                if !result.findings.is_empty() {
                    store
                        .save_findings(&id, &result.findings)
                        .map_err(|e| e.to_string())?;
                }
                ran += 1;
            }
            Err(e) => {
                store
                    .save_plugin_analysis(
                        &id,
                        &spec.name,
                        &serde_json::Value::Null,
                        cfg.provider.as_str(),
                        &cfg.model,
                        &ca_core::analysis::Usage::default(),
                        Some(&e.to_string()),
                    )
                    .map_err(|e| e.to_string())?;
                failed += 1;
            }
        }
    }

    Ok(PluginRunOutcome {
        ran,
        skipped,
        failed,
    })
}
