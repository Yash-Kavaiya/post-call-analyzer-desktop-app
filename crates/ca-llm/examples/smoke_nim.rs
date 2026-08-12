//! Live check that the provider layer reaches a real endpoint.
//! Run with: cargo run -p ca-llm --example smoke_nim
use ca_core::provider::{ChatRequest, Message};
use ca_llm::{build_provider, ProviderKind, ProviderSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("NVIDIA_API_KEY")?;
    let spec = ProviderSpec::new(ProviderKind::NvidiaNim, "meta/llama-3.1-8b-instruct");
    let provider = build_provider(&spec, Some(&key))?;

    let req = ChatRequest::new(vec![
        Message::system("You return only JSON. No prose, no markdown fence."),
        Message::user("Return exactly: {\"greeting_present\": true, \"score\": 0.8}"),
    ])
    .with_schema(serde_json::json!({"type": "object"}));

    let (value, resp): (serde_json::Value, _) = ca_llm::complete_json(provider.as_ref(), req).await?;
    println!("provider   : {}", provider.name());
    println!("model      : {}", resp.model);
    println!("latency_ms : {}", resp.latency_ms);
    println!("tokens     : in={} out={}", resp.input_tokens, resp.output_tokens);
    println!("parsed     : {}", serde_json::to_string(&value)?);
    Ok(())
}
