//! Local cache for remote objects.
//!
//! The decode stage needs a file on disk, so a remote object has to land
//! somewhere before it can be processed. Two properties matter at corpus scale:
//!
//! - **Downloads are resumable across runs.** A batch that is stopped and
//!   restarted must not re-pull gigabytes it already has, so a completed file
//!   is reused as-is.
//! - **A partial download is never mistaken for a complete one.** Bytes go to a
//!   temporary name and are renamed into place only once the transfer finishes;
//!   a process killed mid-download leaves a `.part` file that the next run
//!   overwrites, not a truncated audio file that decodes to silence.

use ca_core::error::{CaError, Result};
use std::path::{Path, PathBuf};

/// Where a URL is cached.
///
/// The name is a hash of the whole URL plus the original extension. A hash
/// because object keys contain characters Windows paths cannot hold and can
/// exceed `MAX_PATH` once nested; the extension because the decoder uses it as
/// a format hint.
pub fn cache_path_for(url: &str, cache_dir: &Path) -> PathBuf {
    let digest = fnv1a64(url.as_bytes());
    let ext = url
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, e)| e.split(['?', '#']).next().unwrap_or(e))
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin")
        .to_ascii_lowercase();

    cache_dir.join(format!("{digest:016x}.{ext}"))
}

/// FNV-1a. Not cryptographic and does not need to be — this names a cache
/// entry, and the URL it came from is the authority.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Fetch `url` into `cache_dir`, reusing an existing complete file.
pub async fn download(client: &reqwest::Client, url: &str, cache_dir: &Path) -> Result<PathBuf> {
    let final_path = cache_path_for(url, cache_dir);

    // A zero-byte file is the signature of an interrupted write, not a valid
    // recording, so it does not count as cached.
    if final_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(final_path);
    }

    std::fs::create_dir_all(cache_dir)
        .map_err(|e| CaError::Config(format!("cannot create the cache directory: {e}")))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CaError::provider("object-store", crate::describe(&e)))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(CaError::ProviderResponse {
            provider: "object-store".into(),
            status: status.as_u16(),
            message: format!("could not download {url}"),
        });
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CaError::provider("object-store", crate::describe(&e)))?;

    if bytes.is_empty() {
        return Err(CaError::Config(format!("{url} is empty")));
    }

    let partial = final_path.with_extension("part");
    std::fs::write(&partial, &bytes)
        .map_err(|e| CaError::Config(format!("cannot write the cache file: {e}")))?;
    std::fs::rename(&partial, &final_path)
        .map_err(|e| CaError::Config(format!("cannot finalise the cache file: {e}")))?;

    Ok(final_path)
}

/// Whether a path is something to download rather than open.
pub fn is_remote(path: &str) -> bool {
    let p = path.trim_start().to_ascii_lowercase();
    p.starts_with("http://") || p.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_are_recognised_and_local_ones_are_not() {
        assert!(is_remote("https://bucket.s3.amazonaws.com/a.m4a"));
        assert!(is_remote("HTTP://x/y.mp3"));
        assert!(!is_remote(r"C:\Users\me\call.m4a"));
        assert!(!is_remote("/home/me/call.m4a"));
    }

    #[test]
    fn the_extension_is_kept_so_the_decoder_still_gets_its_hint() {
        let p = cache_path_for("https://x/y/call.M4A", Path::new("/c"));
        assert_eq!(p.extension().unwrap(), "m4a");
    }

    #[test]
    fn query_strings_do_not_become_part_of_the_extension() {
        let p = cache_path_for("https://x/call.mp3?token=abc", Path::new("/c"));
        assert_eq!(p.extension().unwrap(), "mp3");
    }

    #[test]
    fn an_extensionless_url_still_gets_a_usable_name() {
        let p = cache_path_for("https://x/object-with-no-extension", Path::new("/c"));
        assert_eq!(p.extension().unwrap(), "bin");
    }

    #[test]
    fn different_urls_do_not_collide_and_the_same_url_is_stable() {
        let a = cache_path_for("https://x/a.m4a", Path::new("/c"));
        let b = cache_path_for("https://x/b.m4a", Path::new("/c"));
        assert_ne!(a, b);
        assert_eq!(a, cache_path_for("https://x/a.m4a", Path::new("/c")));
    }

    #[test]
    fn keys_that_would_be_illegal_windows_paths_are_flattened() {
        // Object keys routinely contain characters a path cannot hold; using
        // them verbatim is how a scan fails on the first nested key.
        let p = cache_path_for("https://x/a b/c:d?e*f.m4a", Path::new("/c"));
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(':') && !name.contains('*') && !name.contains(' '));
    }
}
