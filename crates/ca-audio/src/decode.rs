//! Container/codec decoding to mono f32.
//!
//! Every supported input — m4a/AAC, mp3, wav, flac, ogg/opus, mp4, mkv —
//! lands here and leaves as a single mono f32 stream at its source rate.
//! Downstream stages never branch on format again.

use ca_core::error::{CaError, Result};
use std::path::Path;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct Decoded {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: String,
}

/// Decode `path` and downmix to mono.
pub fn decode_file(path: &Path) -> Result<Decoded> {
    let file = std::fs::File::open(path).map_err(|e| CaError::AudioDecode {
        path: path.display().to_string(),
        source: Box::new(e),
    })?;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // The extension is only a hint; symphonia probes the actual bytes, so a
    // mislabelled file still decodes correctly.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| CaError::UnsupportedFormat(format!("{}: {e}", path.display())))?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| CaError::UnsupportedFormat(format!("{}: no audio track", path.display())))?;

    let track_id = track.id;
    // Resolve the numeric codec id to its short name ("aac", "mp3", ...);
    // the raw CodecType is meaningless in the UI.
    let codec = symphonia::default::get_codecs()
        .get_codec(track.codec_params.codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_else(|| format!("{:?}", track.codec_params.codec));
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| CaError::UnsupportedFormat(format!("{}: {e}", path.display())))?;

    let mut mono = Vec::<f32>::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream.
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => {
                // A truncated or slightly corrupt tail is common in phone
                // recordings; keep what decoded rather than losing the call.
                tracing::debug!(path = %path.display(), error = %e, "stopping at packet error");
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(buf) => {
                if sample_rate == 0 {
                    sample_rate = buf.spec().rate;
                }
                if channels == 0 {
                    channels = buf.spec().channels.count() as u16;
                }
                append_mono(&buf, &mut mono);
            }
            // Decode errors are recoverable per the symphonia contract: skip
            // the packet and keep going.
            Err(symphonia::core::errors::Error::DecodeError(e)) => {
                tracing::trace!(error = %e, "skipping undecodable packet");
                continue;
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "stopping at decode error");
                break;
            }
        }
    }

    if mono.is_empty() {
        return Err(CaError::AudioDecode {
            path: path.display().to_string(),
            source: "decoded zero samples".into(),
        });
    }
    if sample_rate == 0 {
        return Err(CaError::AudioDecode {
            path: path.display().to_string(),
            source: "unknown sample rate".into(),
        });
    }

    Ok(Decoded {
        samples: mono,
        sample_rate,
        channels: channels.max(1),
        codec,
    })
}

/// Downmix any channel layout to mono by averaging.
///
/// Averaging rather than taking channel 0 matters for real call recordings:
/// some are dual-mono and some carry one party per channel, and dropping a
/// channel would silently discard half the conversation.
fn append_mono(buf: &AudioBufferRef<'_>, out: &mut Vec<f32>) {
    macro_rules! mix {
        ($b:expr, $conv:expr) => {{
            let b = $b;
            let ch = b.spec().channels.count();
            let frames = b.frames();
            out.reserve(frames);
            for i in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    acc += $conv(b.chan(c)[i]);
                }
                out.push(acc / ch as f32);
            }
        }};
    }

    match buf {
        AudioBufferRef::F32(b) => mix!(b, |v: f32| v),
        AudioBufferRef::F64(b) => mix!(b, |v: f64| v as f32),
        AudioBufferRef::S32(b) => mix!(b, |v: i32| v as f32 / i32::MAX as f32),
        AudioBufferRef::S16(b) => mix!(b, |v: i16| v as f32 / i16::MAX as f32),
        AudioBufferRef::U8(b) => mix!(b, |v: u8| (v as f32 - 128.0) / 128.0),
        AudioBufferRef::S8(b) => mix!(b, |v: i8| v as f32 / i8::MAX as f32),
        AudioBufferRef::U16(b) => mix!(b, |v: u16| (v as f32 - 32768.0) / 32768.0),
        AudioBufferRef::U24(b) => mix!(b, |v: symphonia::core::sample::u24| {
            (v.inner() as f32 - 8_388_608.0) / 8_388_608.0
        }),
        AudioBufferRef::S24(b) => mix!(b, |v: symphonia::core::sample::i24| {
            v.inner() as f32 / 8_388_608.0
        }),
        AudioBufferRef::U32(b) => mix!(b, |v: u32| {
            (v as f64 - 2_147_483_648.0) as f32 / 2_147_483_648.0
        }),
    }
}
