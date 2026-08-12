//! Speech-to-text.
//!
//! Engines sit behind [`ca_core::Transcriber`]. The local whisper.cpp CLI is
//! the default because it needs no credentials and nothing leaves the machine;
//! cloud engines are added behind the same trait.
//!
//! Note on NVIDIA: their hosted ASR is gRPC-only via NVCF (function-id plus
//! Riva protobufs), not the OpenAI-compatible REST surface their LLMs use, so
//! it is not a drop-in for the `ca-llm` transport.

pub mod wav;
pub mod whisper_cli;

pub use whisper_cli::{WhisperCliConfig, WhisperCliTranscriber};
