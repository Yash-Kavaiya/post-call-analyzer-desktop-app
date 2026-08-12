//! Ingestion sources.
//!
//! Everything the pipeline can consume arrives through [`Source`]: a folder, a
//! single file, or a public bucket on S3, Azure Blob or Google Cloud Storage.
//! The pipeline itself knows about none of them — it is handed a path.

pub mod cache;
pub mod object_store;

pub use cache::is_remote;
pub use object_store::{CloudProvider, CloudSpec, ObjectStoreSource};

use async_trait::async_trait;
use ca_core::error::{CaError, Result};
use ca_core::provider::{Source, SourceItem, SourceKind};
use std::path::{Path, PathBuf};

/// Media containers ffmpeg-free symphonia can decode.
const AUDIO_EXTS: &[&str] = &[
    "m4a", "mp3", "wav", "flac", "ogg", "opus", "mp4", "webm", "aac", "mka", "mkv", "caf", "aiff",
];

/// Already-transcribed inputs, which skip ASR entirely.
const TRANSCRIPT_EXTS: &[&str] = &["json", "srt", "vtt", "txt", "csv"];

/// A stable identity for a local file.
///
/// Identity used to be the path string exactly as it arrived, which made the
/// same recording two different calls depending on who asked: the CLI produced
/// `C:/Users/...` and the file dialog produced `C:\Users\...`. A corpus scanned
/// through both entry points was registered twice, and the second copy had no
/// audio processed against it — a Calls list where half the rows have no
/// waveform.
///
/// `canonicalize` resolves `.`, `..`, symlinks and the on-disk casing, so any
/// spelling of a path collapses to one key. It needs the file to exist; when it
/// does not, the separator is normalised so at least that class of duplicate is
/// still caught.
pub fn canonical_key(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(resolved) => {
            let s = resolved.to_string_lossy().to_string();
            // Windows canonicalisation returns the `\?\` verbatim prefix, which
            // is correct but unusable as a display value and confusing in logs.
            s.strip_prefix(r"\?\").unwrap_or(&s).to_string()
        }
        Err(_) => path.to_string_lossy().replace('/', "\\"),
    }
}

/// Classify by extension. Takes a string rather than a `Path` so an object
/// key, which is not a filesystem path, goes through exactly the same rule.
pub fn classify_key(name: &str, include_transcripts: bool) -> Option<SourceKind> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let ext = ext.split(['?', '#']).next().unwrap_or(&ext);
    if AUDIO_EXTS.contains(&ext) {
        Some(SourceKind::Audio)
    } else if include_transcripts && TRANSCRIPT_EXTS.contains(&ext) {
        Some(SourceKind::Transcript)
    } else {
        None
    }
}

/// Flatten a `reqwest` error to something actionable; its own `Display` hides
/// the cause, so a timeout and a refused TLS handshake read identically.
pub(crate) fn describe(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut parts = Vec::new();
    if e.is_timeout() {
        parts.push("timed out".to_string());
    } else if e.is_connect() {
        parts.push("could not connect".to_string());
    }
    parts.push(e.to_string());
    let mut src = e.source();
    while let Some(s) = src {
        let text = s.to_string();
        if !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
        src = s.source();
    }
    parts.join(": ")
}

pub struct LocalFolderSource {
    root: PathBuf,
    recursive: bool,
    include_transcripts: bool,
}

impl LocalFolderSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            recursive: true,
            include_transcripts: false,
        }
    }

    pub fn recursive(mut self, yes: bool) -> Self {
        self.recursive = yes;
        self
    }

    pub fn include_transcripts(mut self, yes: bool) -> Self {
        self.include_transcripts = yes;
        self
    }

    fn classify(&self, path: &Path) -> Option<SourceKind> {
        classify_key(&path.to_string_lossy(), self.include_transcripts)
    }

    fn walk(&self, dir: &Path, out: &mut Vec<SourceItem>) -> Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            CaError::Config(format!("cannot read {}: {e}", dir.display()))
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if self.recursive {
                    // A permission error deep in a tree should skip that
                    // subtree, not abort a 1,968-file scan.
                    if let Err(e) = self.walk(&path, out) {
                        tracing::warn!(dir = %path.display(), error = %e, "skipping");
                    }
                }
                continue;
            }
            let Some(kind) = self.classify(&path) else {
                continue;
            };
            out.push(SourceItem {
                key: canonical_key(&path),
                display_name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size_bytes: entry.metadata().ok().map(|m| m.len()),
                kind,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Source for LocalFolderSource {
    fn name(&self) -> &str {
        "local"
    }

    async fn list(&self) -> Result<Vec<SourceItem>> {
        if !self.root.exists() {
            return Err(CaError::NotFound(format!(
                "{} does not exist",
                self.root.display()
            )));
        }

        // Pointing at one file is the same operation as pointing at a folder
        // holding only that file, so it is handled here rather than as a
        // separate source with its own subtly different behaviour.
        if self.root.is_file() {
            let kind = self.classify(&self.root).ok_or_else(|| {
                CaError::UnsupportedFormat(format!(
                    "{} is not an audio file this app can read",
                    self.root.display()
                ))
            })?;
            return Ok(vec![SourceItem {
                key: canonical_key(&self.root),
                display_name: self
                    .root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size_bytes: self.root.metadata().ok().map(|m| m.len()),
                kind,
            }]);
        }

        let mut out = Vec::new();
        self.walk(&self.root, &mut out)?;
        // Deterministic order keeps a resumed batch processing in the same
        // sequence as the run it is continuing.
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn fetch(&self, item: &SourceItem) -> Result<PathBuf> {
        // Local files are already materialised.
        let p = PathBuf::from(&item.key);
        if !p.exists() {
            return Err(CaError::NotFound(format!("{} vanished", item.key)));
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ca-ingest-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[tokio::test]
    async fn finds_audio_and_ignores_other_files() {
        let d = scratch("basic");
        touch(&d, "a.m4a");
        touch(&d, "b.mp3");
        touch(&d, "notes.pdf");
        touch(&d, "readme.md");

        let items = LocalFolderSource::new(&d).list().await.unwrap();
        let names: Vec<_> = items.iter().map(|i| i.display_name.as_str()).collect();
        assert_eq!(names, vec!["a.m4a", "b.mp3"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn transcripts_are_opt_in() {
        let d = scratch("transcripts");
        touch(&d, "a.m4a");
        touch(&d, "a.json");

        let without = LocalFolderSource::new(&d).list().await.unwrap();
        assert_eq!(without.len(), 1);

        let with = LocalFolderSource::new(&d)
            .include_transcripts(true)
            .list()
            .await
            .unwrap();
        assert_eq!(with.len(), 2);
        assert!(with.iter().any(|i| i.kind == SourceKind::Transcript));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn recursion_can_be_turned_off() {
        let d = scratch("recurse");
        touch(&d, "top.m4a");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        touch(&sub, "deep.m4a");

        let flat = LocalFolderSource::new(&d)
            .recursive(false)
            .list()
            .await
            .unwrap();
        assert_eq!(flat.len(), 1);

        let deep = LocalFolderSource::new(&d).list().await.unwrap();
        assert_eq!(deep.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn extension_matching_is_case_insensitive() {
        let d = scratch("case");
        touch(&d, "LOUD.M4A");
        let items = LocalFolderSource::new(&d).list().await.unwrap();
        assert_eq!(items.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn listing_is_deterministic() {
        let d = scratch("order");
        for n in ["c.m4a", "a.m4a", "b.m4a"] {
            touch(&d, n);
        }
        let a = LocalFolderSource::new(&d).list().await.unwrap();
        let b = LocalFolderSource::new(&d).list().await.unwrap();
        assert_eq!(
            a.iter().map(|i| &i.key).collect::<Vec<_>>(),
            b.iter().map(|i| &i.key).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn missing_folder_is_an_error() {
        let s = LocalFolderSource::new("definitely/not/here");
        assert!(s.list().await.is_err());
    }
}
