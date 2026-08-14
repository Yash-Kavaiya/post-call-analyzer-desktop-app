//! What a custom agent is allowed to be.
//!
//! **A plugin is data, never code.** It declares a prompt, the fields it wants
//! back, and how to turn those into findings. There is no script, no
//! expression language and no hook that runs on this machine.
//!
//! That is the whole security design. This product handles recordings of other
//! people's phone calls; a plugin format that executed anything would mean a
//! downloaded TOML file could read the corpus, the redaction map, or the
//! credential store. A declarative plugin cannot: the worst a bad one can do
//! is ask a model a poor question about an already-redacted transcript and get
//! an answer that fails validation.

use ca_core::error::{CaError, Result};
use serde::{Deserialize, Serialize};

/// A field the plugin expects the model to return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FieldSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    /// Shown to the model, so it knows what the field means.
    #[serde(default)]
    pub description: String,
    /// A response missing a required field is rejected rather than filled in
    /// with a default that would read as a real measurement.
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Number,
    Boolean,
    /// A list of strings.
    List,
}

impl FieldKind {
    fn json_type(&self) -> &'static str {
        match self {
            FieldKind::Text => "string",
            FieldKind::Number => "number",
            FieldKind::Boolean => "boolean",
            FieldKind::List => "array of strings",
        }
    }

    fn matches(&self, v: &serde_json::Value) -> bool {
        match self {
            FieldKind::Text => v.is_string(),
            FieldKind::Number => v.is_number(),
            FieldKind::Boolean => v.is_boolean(),
            FieldKind::List => v
                .as_array()
                .is_some_and(|a| a.iter().all(|x| x.is_string())),
        }
    }
}

/// A custom agent, as written in a TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentSpec {
    /// Stable identifier. Becomes the `agent` column, so it must not collide
    /// with a built-in.
    pub name: String,
    /// Shown in the UI and the CLI.
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// What the model is told it is.
    pub instructions: String,
    /// The shape of the answer.
    ///
    /// Defaulted so a plugin that omits it reaches `validate` and gets the
    /// explanation, rather than a serde message about a missing field.
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    /// Whether this agent may file findings into the risk register.
    #[serde(default)]
    pub findings: bool,
    /// Require every finding to cite a real segment.
    ///
    /// Defaults to **true**: an unevidenced finding in a compliance register is
    /// worse than no finding, because someone will act on it.
    #[serde(default = "yes")]
    pub require_citations: bool,
}

fn yes() -> bool {
    true
}

/// Names the built-in battery already uses.
///
/// A plugin taking one of these would overwrite a built-in agent's stored
/// output, because `analyses` is keyed by `(call_id, agent)`.
const RESERVED: [&str; 10] = [
    "qa_scorecard",
    "sentiment",
    "compliance",
    "root_cause",
    "action_items",
    "coaching",
    "customer_experience",
    "executive_summary",
    "rules",
    "live_monitor",
];

impl AgentSpec {
    /// Read and check a plugin file.
    pub fn parse(toml_text: &str) -> Result<Self> {
        let spec: AgentSpec = toml::from_str(toml_text)
            .map_err(|e| CaError::Config(format!("plugin is not valid TOML: {e}")))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Everything that must hold before a plugin is allowed to run.
    ///
    /// Checked at load rather than at first use: a broken plugin should be a
    /// startup message naming the file, not a failure three hours into a batch.
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(CaError::Config(format!(
                "plugin name `{}` must be lowercase letters, digits and underscores",
                self.name
            )));
        }
        if RESERVED.contains(&self.name.as_str()) {
            return Err(CaError::Config(format!(
                "`{}` is a built-in agent — a plugin using that name would overwrite its results",
                self.name
            )));
        }
        if self.title.trim().is_empty() {
            return Err(CaError::Config("plugin needs a title".into()));
        }
        if self.instructions.trim().len() < 20 {
            return Err(CaError::Config(format!(
                "plugin `{}` needs instructions — a one-word prompt produces noise",
                self.name
            )));
        }
        if self.fields.is_empty() {
            return Err(CaError::Config(format!(
                "plugin `{}` declares no fields, so there is nothing to store",
                self.name
            )));
        }

        let mut seen = std::collections::HashSet::new();
        for f in &self.fields {
            if f.name.trim().is_empty() {
                return Err(CaError::Config("a field has no name".into()));
            }
            if !seen.insert(f.name.as_str()) {
                return Err(CaError::Config(format!(
                    "plugin `{}` declares `{}` twice",
                    self.name, f.name
                )));
            }
            // `findings` is produced by the harness, not by a declared field,
            // and letting a plugin declare it would shadow the real one.
            if f.name == "findings" {
                return Err(CaError::Config(format!(
                    "plugin `{}` cannot declare a `findings` field; set findings = true instead",
                    self.name
                )));
            }
        }
        Ok(())
    }

    /// The JSON shape shown to the model.
    pub fn response_shape(&self) -> String {
        let mut s = String::from("{");
        for (i, f) in self.fields.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "\"{}\":<{}{}>",
                f.name,
                f.kind.json_type(),
                if f.description.is_empty() {
                    String::new()
                } else {
                    format!(", {}", f.description)
                }
            ));
        }
        if self.findings {
            s.push_str(
                ",\"findings\":[{\"code\":\"<SHORT_CODE>\",\"title\":\"<short>\",\
                 \"detail\":\"<one or two sentences>\",\"severity\":\"medium\",\
                 \"citations\":[{\"segment_id\":\"<id>\",\"quote\":\"<short>\"}]}]",
            );
        }
        s.push('}');
        s
    }

    /// Check a model response against the declared fields.
    ///
    /// Returns the accepted payload. A missing required field is an error
    /// rather than a default: a plugin measuring "policy adherence" that
    /// silently reports 0 when the model said nothing is worse than one that
    /// admits it failed.
    pub fn validate_payload(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        let obj = value
            .as_object()
            .ok_or_else(|| CaError::InvalidModelOutput("response is not an object".into()))?;

        let mut out = serde_json::Map::new();
        for f in &self.fields {
            match obj.get(&f.name) {
                Some(v) if f.kind.matches(v) => {
                    out.insert(f.name.clone(), v.clone());
                }
                Some(v) if v.is_null() && !f.required => {}
                Some(_) => {
                    return Err(CaError::InvalidModelOutput(format!(
                        "field `{}` should be {}",
                        f.name,
                        f.kind.json_type()
                    )))
                }
                None if f.required => {
                    return Err(CaError::InvalidModelOutput(format!(
                        "required field `{}` is missing",
                        f.name
                    )))
                }
                None => {}
            }
        }
        Ok(serde_json::Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "upsell_check"
title = "Upsell opportunity"
description = "Did the agent notice a chance to offer the annual plan?"
instructions = "Decide whether the customer showed any signal that they would accept an annual plan, and whether the agent offered one."
findings = false

[[fields]]
name = "opportunity"
type = "boolean"
required = true

[[fields]]
name = "confidence"
type = "number"
description = "0..1"

[[fields]]
name = "signals"
type = "list"
"#;

    #[test]
    fn a_well_formed_plugin_parses() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        assert_eq!(spec.name, "upsell_check");
        assert_eq!(spec.fields.len(), 3);
        assert!(spec.require_citations, "citations are required by default");
    }

    /// `analyses` is keyed by (call_id, agent), so a plugin taking a built-in
    /// name would overwrite that agent's stored result.
    #[test]
    fn a_plugin_cannot_take_a_built_in_name() {
        let toml = GOOD.replace(r#"name = "upsell_check""#, r#"name = "compliance""#);
        let err = AgentSpec::parse(&toml).unwrap_err();
        assert!(format!("{err}").contains("built-in"), "got {err}");
    }

    #[test]
    fn a_plugin_name_is_restricted_to_a_safe_alphabet() {
        for bad in ["Upsell", "up sell", "../etc", "up-sell", ""] {
            let toml = GOOD.replace(r#"name = "upsell_check""#, &format!(r#"name = "{bad}""#));
            assert!(
                AgentSpec::parse(&toml).is_err(),
                "`{bad}` must be rejected as a plugin name"
            );
        }
    }

    #[test]
    fn a_plugin_with_no_fields_is_refused() {
        let toml = GOOD.split("[[fields]]").next().unwrap().to_string();
        let err = AgentSpec::parse(&toml).unwrap_err();
        assert!(format!("{err}").contains("no fields"), "got {err}");
    }

    #[test]
    fn a_one_word_prompt_is_refused() {
        let toml = GOOD.replace(
            r#"instructions = "Decide whether the customer showed any signal that they would accept an annual plan, and whether the agent offered one.""#,
            r#"instructions = "check""#,
        );
        assert!(AgentSpec::parse(&toml).is_err());
    }

    #[test]
    fn a_duplicate_field_is_refused() {
        let toml = format!("{GOOD}\n[[fields]]\nname = \"confidence\"\ntype = \"number\"\n");
        let err = AgentSpec::parse(&toml).unwrap_err();
        assert!(format!("{err}").contains("twice"), "got {err}");
    }

    /// The harness produces `findings`; a declared one would shadow it.
    #[test]
    fn a_plugin_cannot_declare_a_findings_field() {
        let toml = format!("{GOOD}\n[[fields]]\nname = \"findings\"\ntype = \"list\"\n");
        let err = AgentSpec::parse(&toml).unwrap_err();
        assert!(format!("{err}").contains("findings = true"), "got {err}");
    }

    #[test]
    fn a_response_matching_the_declaration_is_accepted() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        let out = spec
            .validate_payload(&serde_json::json!({
                "opportunity": true,
                "confidence": 0.8,
                "signals": ["asked about pricing"],
                "extra": "ignored"
            }))
            .unwrap();

        assert_eq!(out["opportunity"], serde_json::json!(true));
        assert!(
            out.get("extra").is_none(),
            "undeclared fields are dropped rather than stored"
        );
    }

    /// A plugin that silently reports a default when the model said nothing is
    /// producing a fabricated measurement.
    #[test]
    fn a_missing_required_field_fails_rather_than_defaulting() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        let err = spec
            .validate_payload(&serde_json::json!({ "confidence": 0.8 }))
            .unwrap_err();
        assert!(format!("{err}").contains("opportunity"), "got {err}");
    }

    #[test]
    fn a_field_of_the_wrong_type_is_refused() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        let err = spec
            .validate_payload(&serde_json::json!({ "opportunity": "yes" }))
            .unwrap_err();
        assert!(format!("{err}").contains("boolean"), "got {err}");
    }

    #[test]
    fn an_optional_field_may_be_absent_or_null() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        assert!(spec
            .validate_payload(&serde_json::json!({ "opportunity": false }))
            .is_ok());
        assert!(spec
            .validate_payload(&serde_json::json!({ "opportunity": false, "confidence": null }))
            .is_ok());
    }

    #[test]
    fn the_response_shape_names_every_declared_field() {
        let spec = AgentSpec::parse(GOOD).unwrap();
        let shape = spec.response_shape();
        for f in ["opportunity", "confidence", "signals"] {
            assert!(shape.contains(f), "`{f}` missing from {shape}");
        }
        assert!(
            !shape.contains("\"findings\""),
            "findings are not offered when the plugin does not file them"
        );
    }
}
