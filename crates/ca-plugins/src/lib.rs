//! Custom agents, declared rather than coded.
//!
//! Phase 2 shipped eight agents. Every team wants a ninth that nobody else
//! wants — "did we mention the loyalty scheme", "was the callback promised
//! within SLA" — and each one is the same shape: ask a model a question about
//! a transcript, get a structured answer back, store it beside the others.
//!
//! A plugin is a TOML file describing that question. It is **data, never
//! code**: no script, no expression language, no hook that executes here. That
//! is deliberate and load-bearing. This product handles recordings of other
//! people's phone calls, so a plugin format that ran anything would mean a
//! downloaded file could reach the corpus, the redaction map or the credential
//! store. A declarative plugin cannot; the worst a bad one does is ask a poor
//! question and fail validation.
//!
//! What a plugin sees is what every agent sees: the **redacted** transcript.
//! The redact-before-cloud guard is upstream of this crate and is not
//! weakened by it.
//!
//! ```toml
//! name = "callback_sla"
//! title = "Callback within SLA"
//! instructions = "Decide whether the agent promised a callback, and whether the promise named a time inside two working days."
//! findings = true
//!
//! [[fields]]
//! name = "promised"
//! type = "boolean"
//! required = true
//!
//! [[fields]]
//! name = "within_sla"
//! type = "boolean"
//! ```

pub mod runner;
pub mod spec;

pub use runner::{run_plugin, PluginResult};
pub use spec::{AgentSpec, FieldKind, FieldSpec};

use ca_core::error::{CaError, Result};
use std::path::{Path, PathBuf};

/// Where plugins live by default, beside the database.
pub fn default_plugin_dir(base: &Path) -> PathBuf {
    base.join("plugins")
}

/// A plugin file, and whether it loaded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedPlugin {
    pub path: String,
    /// `None` when the file failed to load; `error` says why.
    pub spec: Option<AgentSpec>,
    pub error: Option<String>,
}

/// Load every plugin in a directory.
///
/// A broken file is reported rather than thrown away, and never stops the
/// others loading: one plugin with a typo should not silently disable a
/// team's other four.
pub fn load_dir(dir: &Path) -> Result<Vec<LoadedPlugin>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // No directory is not an error — most installs have no plugins.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(CaError::Config(format!("read {}: {e}", dir.display()))),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let display = path.display().to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => match AgentSpec::parse(&text) {
                Ok(spec) => out.push(LoadedPlugin {
                    path: display,
                    spec: Some(spec),
                    error: None,
                }),
                Err(e) => out.push(LoadedPlugin {
                    path: display,
                    spec: None,
                    error: Some(e.to_string()),
                }),
            },
            Err(e) => out.push(LoadedPlugin {
                path: display,
                spec: None,
                error: Some(format!("unreadable: {e}")),
            }),
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));

    // Two plugins claiming one name would overwrite each other's stored
    // results, so the collision is reported at load rather than discovered as
    // missing data later.
    let mut seen: std::collections::HashMap<&str, &str> = Default::default();
    let mut clashes = Vec::new();
    for p in &out {
        if let Some(spec) = &p.spec {
            if let Some(first) = seen.insert(&spec.name, &p.path) {
                clashes.push(format!(
                    "`{}` is declared by both {first} and {}",
                    spec.name, p.path
                ));
            }
        }
    }
    for (p, clash) in out
        .iter_mut()
        .filter(|p| p.spec.is_some())
        .zip(clashes.iter())
    {
        // Only the later duplicate is disabled; the first one still works.
        if p.error.is_none() && clash.contains(&p.path) {
            p.spec = None;
            p.error = Some(clash.clone());
        }
    }

    Ok(out)
}

/// Just the plugins that loaded.
pub fn usable(loaded: &[LoadedPlugin]) -> Vec<&AgentSpec> {
    loaded.iter().filter_map(|p| p.spec.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ca-plugins-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plugin(name: &str) -> String {
        format!(
            r#"
name = "{name}"
title = "A test plugin"
instructions = "Decide something specific about this call and report it back."

[[fields]]
name = "verdict"
type = "boolean"
required = true
"#
        )
    }

    #[test]
    fn no_plugin_directory_is_not_an_error() {
        let dir = std::env::temp_dir().join("ca-plugins-absent");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_dir(&dir).unwrap().is_empty());
    }

    #[test]
    fn every_valid_plugin_in_a_directory_loads() {
        let dir = scratch("load");
        std::fs::write(dir.join("a.toml"), plugin("alpha")).unwrap();
        std::fs::write(dir.join("b.toml"), plugin("beta")).unwrap();
        // Not a plugin; must be ignored rather than reported as broken.
        std::fs::write(dir.join("notes.md"), "hello").unwrap();

        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(usable(&loaded).len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One typo must not silently disable a team's other plugins.
    #[test]
    fn a_broken_plugin_is_reported_and_the_others_still_load() {
        let dir = scratch("broken");
        std::fs::write(dir.join("good.toml"), plugin("alpha")).unwrap();
        std::fs::write(
            dir.join("bad.toml"),
            "name = \"oops\"\nthis is not toml [[[",
        )
        .unwrap();

        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(usable(&loaded).len(), 1, "the good one still loads");

        let broken = loaded.iter().find(|p| p.spec.is_none()).unwrap();
        assert!(broken.error.is_some(), "the failure names itself");
        assert!(broken.path.ends_with("bad.toml"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two plugins with one name would overwrite each other's results, because
    /// `analyses` is keyed by (call_id, agent).
    #[test]
    fn a_duplicate_name_disables_the_later_plugin_and_says_so() {
        let dir = scratch("clash");
        std::fs::write(dir.join("a.toml"), plugin("same")).unwrap();
        std::fs::write(dir.join("z.toml"), plugin("same")).unwrap();

        let loaded = load_dir(&dir).unwrap();
        assert_eq!(usable(&loaded).len(), 1, "only one may own the name");
        let disabled = loaded.iter().find(|p| p.spec.is_none()).unwrap();
        assert!(
            disabled
                .error
                .as_ref()
                .unwrap()
                .contains("declared by both"),
            "got {:?}",
            disabled.error
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
