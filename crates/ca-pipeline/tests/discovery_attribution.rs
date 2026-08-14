//! Discovery attributes calls on the way in.
//!
//! A user who scans a folder should see who was on each call immediately. If
//! attribution only ever happened in the backfill, every fresh ingest would
//! show a corpus of anonymous calls until someone thought to run a second
//! command.

use ca_core::identity::{AttributionSource, PersonKind};
use ca_ingest::LocalFolderSource;
use ca_pipeline::Pipeline;
use ca_store::Store;
use std::sync::Arc;

/// Discovery never transcribes, so the transcriber is only there to satisfy
/// the constructor.
fn stub_transcriber() -> Arc<ca_asr::WhisperCliTranscriber> {
    let dir = std::env::temp_dir().join("ca-discovery-attribution-bin");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("not-whisper.exe");
    let model = dir.join("not-a-model.bin");
    for p in [&bin, &model] {
        if !p.exists() {
            std::fs::write(p, b"").unwrap();
        }
    }
    Arc::new(ca_asr::WhisperCliTranscriber::new(ca_asr::WhisperCliConfig::new(bin, model)).unwrap())
}

/// Empty files are enough: `Source::list` walks the filesystem and classifies
/// by extension. Nothing here decodes audio.
fn corpus(dir: &std::path::Path, names: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        std::fs::write(dir.join(n), b"").unwrap();
    }
}

#[tokio::test]
async fn a_scanned_folder_arrives_already_attributed() {
    let dir = std::env::temp_dir().join("ca-discovery-attribution");
    let _ = std::fs::remove_dir_all(&dir);
    corpus(
        &dir,
        &[
            "Call recording +918485823280_251112_142326.m4a",
            "Call recording +918485823280_251114_204226.m4a",
            "Call recording Babulal kavaiya_251101_090000.m4a",
        ],
    );

    let store = Arc::new(Store::in_memory().unwrap());
    let pipeline = Pipeline::new(store.clone(), stub_transcriber());
    let source = LocalFolderSource::new(&dir);

    let (total, added) = pipeline.discover(&source).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(added, 3);

    // Two contacts, one of whom called twice — without a separate backfill.
    let contacts = store.list_people(Some(PersonKind::Contact), None).unwrap();
    assert_eq!(contacts.len(), 2);
    assert_eq!(contacts[0].calls, 2, "busiest contact first");
    assert!(
        contacts[0].last_call.is_some(),
        "the real call time came from the filename, not from ingest time"
    );

    let calls = store.list_calls(0, 10, None).unwrap();
    for c in &calls {
        assert_eq!(
            store
                .call_attribution(&ca_core::ids::CallId::from(c.id.clone()))
                .unwrap(),
            AttributionSource::Rule,
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file the patterns do not recognise must still ingest and stay usable.
#[tokio::test]
async fn an_unrecognised_filename_still_ingests() {
    let dir = std::env::temp_dir().join("ca-discovery-attribution-unmatched");
    let _ = std::fs::remove_dir_all(&dir);
    corpus(&dir, &["holiday video.m4a"]);

    let store = Arc::new(Store::in_memory().unwrap());
    let pipeline = Pipeline::new(store.clone(), stub_transcriber());

    let (total, added) = pipeline
        .discover(&LocalFolderSource::new(&dir))
        .await
        .unwrap();
    assert_eq!((total, added), (1, 1));
    assert_eq!(store.count_calls().unwrap(), 1);
    assert!(
        store.list_people(None, None).unwrap().is_empty(),
        "no pattern matched, so nobody was invented"
    );

    let id = ca_core::ids::CallId::from(store.list_calls(0, 1, None).unwrap()[0].id.clone());
    assert_eq!(
        store.call_attribution(&id).unwrap(),
        AttributionSource::None
    );

    let _ = std::fs::remove_dir_all(&dir);
}
