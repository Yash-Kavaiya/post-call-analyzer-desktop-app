//! Analysis agents.
//!
//! Each agent turns a transcript into structured, citable findings. Two rules
//! hold across all of them: citations are validated against real segment ids
//! before anything is stored, and any arithmetic is done here rather than
//! trusted from the model.

pub mod action_items;
pub mod citations;
pub mod coaching;
pub mod compliance;
pub mod copilot;
pub mod customer_experience;
pub mod executive_summary;
pub mod live_monitor;
pub mod qa_scorecard;
pub mod root_cause;
pub mod rubric;
pub mod sentiment;

pub use action_items::ActionItemsAgent;
pub use coaching::CoachingAgent;
pub use compliance::ComplianceAgent;
pub use copilot::{CallCopilot, CopilotAnswer, CopilotTurn};
pub use customer_experience::CustomerExperienceAgent;
pub use executive_summary::ExecutiveSummaryAgent;
pub use live_monitor::LiveMonitorAgent;
pub use qa_scorecard::QaScorecardAgent;
pub use root_cause::RootCauseAgent;
pub use rubric::{Criterion, Rubric, DEFAULT_RUBRIC};
pub use sentiment::SentimentAgent;
