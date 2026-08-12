use thiserror::Error;

/// Every fallible operation in the workspace funnels through this.
///
/// Variants are deliberately coarse: callers act on the *category* of failure
/// (retry the network, skip the file, abort the batch), not on the specific
/// library that produced it.
#[derive(Debug, Error)]
pub enum CaError {
    #[error("audio decode failed for {path}: {source}")]
    AudioDecode {
        path: String,
        #[source]
        source: anyhow_like::BoxError,
    },

    #[error("unsupported media format: {0}")]
    UnsupportedFormat(String),

    /// Transport-level failure talking to a provider. Retryable.
    #[error("provider `{provider}` transport error: {message}")]
    ProviderTransport { provider: String, message: String },

    /// Provider returned a well-formed error response. Usually not retryable.
    #[error("provider `{provider}` returned {status}: {message}")]
    ProviderResponse {
        provider: String,
        status: u16,
        message: String,
    },

    /// Provider hit a rate limit; `retry_after` is seconds when the server told us.
    #[error("provider `{provider}` rate limited (retry after {retry_after:?}s)")]
    RateLimited {
        provider: String,
        retry_after: Option<u64>,
    },

    /// The model produced output that did not match the contract we required.
    #[error("model output invalid: {0}")]
    InvalidModelOutput(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error(transparent)]
    Other(#[from] anyhow_like::BoxError),
}

impl CaError {
    /// Whether a batch runner should retry this call after a backoff.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            CaError::ProviderTransport { .. } | CaError::RateLimited { .. }
        )
    }

    pub fn provider(name: impl Into<String>, message: impl Into<String>) -> Self {
        CaError::ProviderTransport {
            provider: name.into(),
            message: message.into(),
        }
    }
}

/// Tiny shim so `ca-core` stays free of an `anyhow` dependency while still
/// letting downstream crates attach arbitrary source errors.
pub mod anyhow_like {
    pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
}

pub type Result<T> = std::result::Result<T, CaError>;
