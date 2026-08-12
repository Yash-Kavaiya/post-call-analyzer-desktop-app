//! Public object storage: S3, Azure Blob, and Google Cloud Storage.
//!
//! All three are read over plain HTTPS against their public listing APIs
//! rather than through vendor SDKs. That is a deliberate trade:
//!
//! - **Three SDKs is three dependency trees.** `aws-sdk-s3` alone pulls in more
//!   code than this entire workspace, and the app would carry all of it to
//!   support a feature most users reach for once.
//! - **Public buckets need no signing.** The whole value of the SDKs is
//!   credential handling and request signing; for anonymous reads there is
//!   nothing to sign, and the listing APIs are stable, documented XML/JSON.
//!
//! The consequence is stated rather than hidden: this reads *public* buckets
//! only. A private bucket needs signed requests, which needs the SDKs, and that
//! is a separate decision to make when someone actually needs it.
//!
//! S3's endpoint is configurable, so any S3-compatible service — MinIO,
//! Cloudflare R2, Wasabi, Backblaze B2 — works through the same path.

use crate::classify_key;
use async_trait::async_trait;
use ca_core::error::{CaError, Result};
use ca_core::provider::{Source, SourceItem};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Pages to walk before giving up. A bucket with more than this is telling you
/// to use a prefix, and an unbounded loop against a paginated API is how a
/// scan becomes a hang.
const MAX_PAGES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    S3,
    AzureBlob,
    Gcs,
}

impl CloudProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            CloudProvider::S3 => "s3",
            CloudProvider::AzureBlob => "azure",
            CloudProvider::Gcs => "gcs",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            CloudProvider::S3 => "Amazon S3 (or S3-compatible)",
            CloudProvider::AzureBlob => "Azure Blob Storage",
            CloudProvider::Gcs => "Google Cloud Storage",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "s3" | "aws" | "minio" | "r2" => Some(CloudProvider::S3),
            "azure" | "azure_blob" | "azureblob" | "blob" => Some(CloudProvider::AzureBlob),
            "gcs" | "google" | "gcp" => Some(CloudProvider::Gcs),
            _ => None,
        }
    }

    pub fn all() -> &'static [CloudProvider] {
        &[CloudProvider::S3, CloudProvider::AzureBlob, CloudProvider::Gcs]
    }
}

/// What to scan. `bucket` is the container name on Azure and the bucket
/// elsewhere; `account` is Azure's storage account and `region` is S3's.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSpec {
    pub provider: CloudProvider,
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    /// S3 only. Defaults to `us-east-1`.
    #[serde(default)]
    pub region: Option<String>,
    /// Azure only. The storage account name.
    #[serde(default)]
    pub account: Option<String>,
    /// S3-compatible services. When set, replaces the AWS host entirely.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Include already-transcribed files alongside audio.
    #[serde(default)]
    pub include_transcripts: bool,
}

impl CloudSpec {
    /// Reject a configuration that cannot address anything, with a message that
    /// says which field is missing rather than failing later on a 404.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.bucket.trim().is_empty() {
            let what = match self.provider {
                CloudProvider::AzureBlob => "a container name",
                _ => "a bucket name",
            };
            return Err(format!("Enter {what}."));
        }
        if self.provider == CloudProvider::AzureBlob
            && self.account.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("Azure needs the storage account name.".into());
        }
        if let Some(ep) = &self.endpoint {
            if !ep.trim().is_empty() && !ep.starts_with("http") {
                return Err("The endpoint must start with http:// or https://".into());
            }
        }
        Ok(())
    }

    /// The host that objects live on. Also shown in the UI, so what is about to
    /// be contacted is visible before it is contacted.
    pub fn base_url(&self) -> String {
        match self.provider {
            CloudProvider::S3 => match self.endpoint.as_deref().map(str::trim) {
                Some(ep) if !ep.is_empty() => {
                    // Path style, which is what S3-compatible services use.
                    format!("{}/{}", ep.trim_end_matches('/'), self.bucket)
                }
                _ => format!(
                    "https://{}.s3.{}.amazonaws.com",
                    self.bucket,
                    self.region.as_deref().unwrap_or("us-east-1")
                ),
            },
            CloudProvider::AzureBlob => format!(
                "https://{}.blob.core.windows.net/{}",
                self.account.as_deref().unwrap_or(""),
                self.bucket
            ),
            CloudProvider::Gcs => format!("https://storage.googleapis.com/{}", self.bucket),
        }
    }

    /// Public URL of one object.
    pub fn object_url(&self, key: &str) -> String {
        format!("{}/{}", self.base_url(), encode_path(key))
    }

    fn list_url(&self, token: Option<&str>) -> String {
        let prefix = encode_query(&self.prefix);
        match self.provider {
            CloudProvider::S3 => {
                let mut u = format!("{}/?list-type=2&max-keys=1000", self.base_url());
                if !self.prefix.is_empty() {
                    u.push_str(&format!("&prefix={prefix}"));
                }
                if let Some(t) = token {
                    u.push_str(&format!("&continuation-token={}", encode_query(t)));
                }
                u
            }
            CloudProvider::AzureBlob => {
                let mut u = format!("{}?restype=container&comp=list", self.base_url());
                if !self.prefix.is_empty() {
                    u.push_str(&format!("&prefix={prefix}"));
                }
                if let Some(t) = token {
                    u.push_str(&format!("&marker={}", encode_query(t)));
                }
                u
            }
            CloudProvider::Gcs => {
                // The JSON API, because the XML one needs a signed request for
                // listing even when objects are public.
                let mut u = format!(
                    "https://storage.googleapis.com/storage/v1/b/{}/o?maxResults=1000",
                    encode_path(&self.bucket)
                );
                if !self.prefix.is_empty() {
                    u.push_str(&format!("&prefix={prefix}"));
                }
                if let Some(t) = token {
                    u.push_str(&format!("&pageToken={}", encode_query(t)));
                }
                u
            }
        }
    }
}

/// Percent-encode everything a path segment may not contain. `/` is kept: it
/// separates key segments and re-encoding it would break every nested object.
fn encode_path(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn encode_query(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub struct ObjectStoreSource {
    spec: CloudSpec,
    http: reqwest::Client,
    cache: PathBuf,
    name: String,
}

impl ObjectStoreSource {
    pub fn new(spec: CloudSpec, cache: impl Into<PathBuf>) -> Result<Self> {
        spec.validate().map_err(CaError::Config)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| CaError::Config(format!("http client: {e}")))?;
        // The source name is part of a call's identity, so it must distinguish
        // two buckets — otherwise re-scanning bucket B would silently adopt
        // bucket A's already-processed calls.
        let name = format!("{}:{}", spec.provider.as_str(), spec.bucket);
        Ok(Self { spec, http, cache: cache.into(), name })
    }

    pub fn spec(&self) -> &CloudSpec {
        &self.spec
    }

    async fn get(&self, url: &str) -> Result<String> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| CaError::provider("object-store", crate::describe(&e)))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // 403 on a public-bucket read almost always means "not public",
            // which is a different fix from "wrong name".
            let hint = if status.as_u16() == 403 {
                " — the bucket may not allow anonymous listing"
            } else if status.as_u16() == 404 {
                " — check the name, region and prefix"
            } else {
                ""
            };
            return Err(CaError::ProviderResponse {
                provider: "object-store".into(),
                status: status.as_u16(),
                message: format!("{}{hint}", body.chars().take(300).collect::<String>()),
            });
        }

        resp.text()
            .await
            .map_err(|e| CaError::provider("object-store", crate::describe(&e)))
    }
}

/// One object, before it is filtered by extension.
struct RawObject {
    key: String,
    size: Option<u64>,
}

/// S3 `ListBucketResult` and Azure `EnumerationResults` are both flat lists of
/// elements; the only difference is which tags carry the key and the size.
fn parse_xml_listing(
    xml: &str,
    key_tag: &str,
    size_tag: &str,
    token_tag: &str,
) -> Result<(Vec<RawObject>, Option<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut objects = Vec::new();
    let mut token = None;
    let mut buf = String::new();
    let mut key: Option<String> = None;
    let mut size: Option<u64> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(_)) => buf.clear(),

            // Text arrives in pieces: an entity reference interrupts it and
            // comes back as its own event. Appending rather than assigning is
            // what stops `a&amp;b/c.m4a` from being read as just `b/c.m4a` —
            // a truncated key that 404s like a missing file.
            Ok(Event::Text(e)) => {
                if let Ok(t) = e.decode() {
                    buf.push_str(&t);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let Ok(name) = e.decode() {
                    buf.push_str(&resolve_entity(&name));
                }
            }

            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let text = std::mem::take(&mut buf);
                if name == key_tag {
                    key = Some(text);
                } else if name == size_tag {
                    size = text.trim().parse().ok();
                } else if name == token_tag {
                    if !text.trim().is_empty() {
                        token = Some(text.trim().to_string());
                    }
                } else if (name == "Contents" || name == "Blob") && key.is_some() {
                    objects.push(RawObject { key: key.take().unwrap(), size: size.take() });
                }
            }

            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(CaError::Config(format!(
                    "could not read the listing returned by the bucket: {e}"
                )))
            }
            _ => {}
        }
    }

    Ok((objects, token))
}

/// The five entities XML predefines, plus numeric references. Object keys
/// legitimately contain `&`, `<` and quotes, so this is not a rare path.
fn resolve_entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            let parsed = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32);
            match parsed {
                Some(c) => c.to_string(),
                // Unknown entity: keep it literal rather than dropping
                // characters out of a key that has to match exactly.
                None => format!("&{name};"),
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GcsListing {
    #[serde(default)]
    items: Vec<GcsObject>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GcsObject {
    name: String,
    #[serde(default)]
    size: Option<String>,
}

#[async_trait]
impl Source for ObjectStoreSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn list(&self) -> Result<Vec<SourceItem>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;

        for page in 0..MAX_PAGES {
            let body = self.get(&self.spec.list_url(token.as_deref())).await?;

            let (raw, next) = match self.spec.provider {
                CloudProvider::S3 => {
                    parse_xml_listing(&body, "Key", "Size", "NextContinuationToken")?
                }
                CloudProvider::AzureBlob => {
                    parse_xml_listing(&body, "Name", "Content-Length", "NextMarker")?
                }
                CloudProvider::Gcs => {
                    let parsed: GcsListing = serde_json::from_str(&body).map_err(|e| {
                        CaError::Config(format!("unexpected listing from Google: {e}"))
                    })?;
                    (
                        parsed
                            .items
                            .into_iter()
                            .map(|o| RawObject {
                                key: o.name,
                                size: o.size.and_then(|s| s.parse().ok()),
                            })
                            .collect(),
                        parsed.next_page_token,
                    )
                }
            };

            for obj in raw {
                // A "directory" placeholder is a zero-byte key ending in `/`.
                if obj.key.ends_with('/') {
                    continue;
                }
                let Some(kind) = classify_key(&obj.key, self.spec.include_transcripts) else {
                    continue;
                };
                out.push(SourceItem {
                    // The full URL is the identity *and* the address, which is
                    // what lets the pipeline fetch it later without holding on
                    // to this source.
                    key: self.spec.object_url(&obj.key),
                    display_name: obj.key.rsplit('/').next().unwrap_or(&obj.key).to_string(),
                    size_bytes: obj.size,
                    kind,
                });
            }

            match next {
                Some(t) => token = Some(t),
                None => break,
            }
            if page + 1 == MAX_PAGES {
                tracing::warn!(
                    pages = MAX_PAGES,
                    found = out.len(),
                    "stopped paging the bucket; narrow the prefix to see the rest"
                );
            }
        }

        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn fetch(&self, item: &SourceItem) -> Result<PathBuf> {
        crate::cache::download(&self.http, &item.key, &self.cache).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3() -> CloudSpec {
        CloudSpec {
            provider: CloudProvider::S3,
            bucket: "calls".into(),
            prefix: "inbound/".into(),
            region: Some("eu-west-2".into()),
            account: None,
            endpoint: None,
            include_transcripts: false,
        }
    }

    #[test]
    fn s3_addresses_a_virtual_hosted_bucket() {
        assert_eq!(s3().base_url(), "https://calls.s3.eu-west-2.amazonaws.com");
        assert!(s3().list_url(None).contains("list-type=2"));
        assert!(s3().list_url(None).contains("prefix=inbound%2F"));
    }

    #[test]
    fn a_custom_endpoint_switches_to_path_style() {
        // MinIO, R2 and friends do not do virtual-hosted buckets.
        let spec = CloudSpec { endpoint: Some("https://minio.local:9000".into()), ..s3() };
        assert_eq!(spec.base_url(), "https://minio.local:9000/calls");
    }

    #[test]
    fn azure_addresses_account_and_container() {
        let spec = CloudSpec {
            provider: CloudProvider::AzureBlob,
            account: Some("acme".into()),
            bucket: "recordings".into(),
            ..s3()
        };
        assert_eq!(spec.base_url(), "https://acme.blob.core.windows.net/recordings");
        assert!(spec.list_url(None).contains("restype=container&comp=list"));
    }

    #[test]
    fn gcs_lists_through_the_json_api_but_fetches_the_public_url() {
        let spec = CloudSpec { provider: CloudProvider::Gcs, ..s3() };
        assert!(spec.list_url(None).contains("storage/v1/b/calls/o"));
        assert_eq!(spec.object_url("a b.m4a"), "https://storage.googleapis.com/calls/a%20b.m4a");
    }

    #[test]
    fn object_keys_keep_their_slashes_but_encode_everything_else() {
        // Re-encoding `/` would flatten every nested key into a 404.
        assert_eq!(encode_path("a/b c+d.m4a"), "a/b%20c%2Bd.m4a");
    }

    #[test]
    fn azure_needs_an_account_and_says_so() {
        let spec = CloudSpec { provider: CloudProvider::AzureBlob, account: None, ..s3() };
        assert!(spec.validate().unwrap_err().contains("storage account"));
    }

    #[test]
    fn an_empty_bucket_name_is_rejected_in_the_right_words() {
        let mut spec = s3();
        spec.bucket = "  ".into();
        assert!(spec.validate().unwrap_err().contains("bucket"));

        let azure = CloudSpec {
            provider: CloudProvider::AzureBlob,
            bucket: "".into(),
            account: Some("a".into()),
            ..s3()
        };
        assert!(azure.validate().unwrap_err().contains("container"));
    }

    #[test]
    fn s3_listings_are_parsed_with_their_continuation_token() {
        let xml = r#"<?xml version="1.0"?>
        <ListBucketResult>
          <Contents><Key>inbound/a.m4a</Key><Size>1234</Size></Contents>
          <Contents><Key>inbound/b.mp3</Key><Size>99</Size></Contents>
          <NextContinuationToken>tok123</NextContinuationToken>
        </ListBucketResult>"#;
        let (objs, token) =
            parse_xml_listing(xml, "Key", "Size", "NextContinuationToken").unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].key, "inbound/a.m4a");
        assert_eq!(objs[0].size, Some(1234));
        assert_eq!(token.as_deref(), Some("tok123"));
    }

    #[test]
    fn azure_listings_are_parsed_from_their_own_tag_names() {
        let xml = r#"<?xml version="1.0"?>
        <EnumerationResults>
          <Blobs>
            <Blob><Name>calls/x.wav</Name><Properties><Content-Length>77</Content-Length></Properties></Blob>
          </Blobs>
          <NextMarker/>
        </EnumerationResults>"#;
        let (objs, token) =
            parse_xml_listing(xml, "Name", "Content-Length", "NextMarker").unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].key, "calls/x.wav");
        assert_eq!(objs[0].size, Some(77));
        assert!(token.is_none(), "an empty NextMarker means the last page");
    }

    #[test]
    fn xml_entities_in_keys_survive_parsing() {
        // A key containing `&` arrives escaped; decoding it wrong produces a
        // URL that 404s in a way that looks like a missing file.
        let xml = r#"<ListBucketResult>
          <Contents><Key>a&amp;b/c.m4a</Key><Size>1</Size></Contents>
        </ListBucketResult>"#;
        let (objs, _) = parse_xml_listing(xml, "Key", "Size", "NextContinuationToken").unwrap();
        assert_eq!(objs[0].key, "a&b/c.m4a");
    }

    #[test]
    fn gcs_json_is_parsed_including_its_string_sizes() {
        // Google returns sizes as strings; parsing them as numbers fails.
        let json = r#"{"items":[{"name":"a.m4a","size":"4096"}],"nextPageToken":"p2"}"#;
        let parsed: GcsListing = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.items[0].name, "a.m4a");
        assert_eq!(parsed.items[0].size.as_deref(), Some("4096"));
        assert_eq!(parsed.next_page_token.as_deref(), Some("p2"));
    }

    #[test]
    fn two_buckets_are_two_sources() {
        // Identity is (source_name, key); sharing a name across buckets would
        // make a scan of one adopt the other's processed calls.
        let a = ObjectStoreSource::new(s3(), ".").unwrap();
        let b = ObjectStoreSource::new(CloudSpec { bucket: "other".into(), ..s3() }, ".").unwrap();
        assert_ne!(a.name(), b.name());
    }
}
