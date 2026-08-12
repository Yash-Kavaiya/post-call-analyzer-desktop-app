//! Scorecard rubrics.
//!
//! Criteria are configuration, not code: every contact centre scores calls
//! differently, and a hard-coded rubric would make the product useless to
//! anyone but its first customer. Rubrics are TOML so a QA lead can edit one
//! without a build, and they carry a version so a stored score can always be
//! traced back to the criteria that produced it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    /// Stable machine key. Renaming this breaks historical comparisons, so it
    /// is deliberately separate from the human label.
    pub key: String,
    pub label: String,
    /// What the model is asked to look for.
    pub description: String,
    /// Relative weight. Need not sum to anything in particular — weights are
    /// normalised at scoring time.
    #[serde(default = "default_weight")]
    pub weight: f32,
    /// When true, the model may mark this not-applicable rather than scoring
    /// it. Useful for criteria that only apply to some call types.
    #[serde(default)]
    pub allow_not_applicable: bool,
}

fn default_weight() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub criteria: Vec<Criterion>,
}

impl Rubric {
    /// Parse from TOML.
    pub fn from_toml(s: &str) -> ca_core::error::Result<Self> {
        let r: Rubric = toml::from_str(s)
            .map_err(|e| ca_core::error::CaError::Config(format!("rubric: {e}")))?;
        r.validate()?;
        Ok(r)
    }

    pub fn load(path: &std::path::Path) -> ca_core::error::Result<Self> {
        let s = std::fs::read_to_string(path).map_err(|e| {
            ca_core::error::CaError::Config(format!("read {}: {e}", path.display()))
        })?;
        Self::from_toml(&s)
    }

    fn validate(&self) -> ca_core::error::Result<()> {
        use ca_core::error::CaError;
        if self.criteria.is_empty() {
            return Err(CaError::Config("rubric has no criteria".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.criteria {
            if !seen.insert(&c.key) {
                return Err(CaError::Config(format!(
                    "duplicate criterion key `{}`",
                    c.key
                )));
            }
            if c.weight < 0.0 {
                return Err(CaError::Config(format!(
                    "criterion `{}` has negative weight",
                    c.key
                )));
            }
        }
        if self.criteria.iter().all(|c| c.weight == 0.0) {
            return Err(CaError::Config("all criteria have zero weight".into()));
        }
        Ok(())
    }
}

impl Default for Rubric {
    /// The five criteria in the brief, as a usable starting point.
    fn default() -> Self {
        Self::from_toml(DEFAULT_RUBRIC).expect("built-in rubric must parse")
    }
}

pub const DEFAULT_RUBRIC: &str = r#"
id = "default"
version = "1"
name = "General contact centre QA"
description = "Baseline scorecard covering greeting through resolution."

[[criteria]]
key = "greeting"
label = "Greeting"
weight = 1.0
description = """
Did the agent open professionally: identify themselves and/or the company, and
offer help? Score low if the call opens abruptly or the agent never identifies
themselves."""

[[criteria]]
key = "active_listening"
label = "Active Listening"
weight = 1.5
description = """
Did the agent demonstrate they heard the customer — acknowledging, paraphrasing,
or referring back to details the customer gave? Score low for repeated requests
for information the customer already provided, or for talking over them."""

[[criteria]]
key = "empathy"
label = "Empathy"
weight = 1.5
description = """
Did the agent acknowledge the customer's situation or frustration in a way that
fits the call? Score low for a purely transactional tone when the customer is
clearly upset. Do not reward empty phrases that ignore what was actually said."""

[[criteria]]
key = "resolution"
label = "Resolution"
weight = 2.0
description = """
Was the customer's actual issue addressed, or a concrete next step agreed with
an owner and a timeframe? Score low if the call ends ambiguously or the customer
is passed on without explanation."""

[[criteria]]
key = "professionalism"
label = "Professionalism"
weight = 1.0
description = """
Was the agent's language appropriate throughout — courteous, clear, free of
inappropriate remarks? Score low for rudeness, audible impatience, or unexplained
jargon."""
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_rubric_parses_and_has_the_expected_criteria() {
        let r = Rubric::default();
        let keys: Vec<&str> = r.criteria.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "greeting",
                "active_listening",
                "empathy",
                "resolution",
                "professionalism"
            ]
        );
    }

    #[test]
    fn resolution_is_weighted_highest() {
        // Whether the customer's problem got solved should matter more than
        // whether the greeting was word-perfect.
        let r = Rubric::default();
        let max = r
            .criteria
            .iter()
            .max_by(|a, b| a.weight.partial_cmp(&b.weight).unwrap())
            .unwrap();
        assert_eq!(max.key, "resolution");
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let toml = r#"
id="x"
version="1"
name="x"
[[criteria]]
key="a"
label="A"
description="d"
[[criteria]]
key="a"
label="B"
description="d"
"#;
        let err = Rubric::from_toml(toml).err().unwrap();
        assert!(format!("{err}").contains("duplicate"), "got {err}");
    }

    #[test]
    fn empty_rubric_is_rejected() {
        let toml = r#"
id="x"
version="1"
name="x"
criteria=[]
"#;
        assert!(Rubric::from_toml(toml).is_err());
    }

    #[test]
    fn all_zero_weights_is_rejected() {
        // Would make every score a division by zero.
        let toml = r#"
id="x"
version="1"
name="x"
[[criteria]]
key="a"
label="A"
description="d"
weight=0.0
"#;
        assert!(Rubric::from_toml(toml).is_err());
    }

    #[test]
    fn weight_defaults_to_one() {
        let toml = r#"
id="x"
version="1"
name="x"
[[criteria]]
key="a"
label="A"
description="d"
"#;
        let r = Rubric::from_toml(toml).unwrap();
        assert_eq!(r.criteria[0].weight, 1.0);
    }
}
