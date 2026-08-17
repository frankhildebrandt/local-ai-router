use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    ApiKey,
    OpenAiSubscription,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    Bearer,
    XApiKey,
    XGoogApiKey,
    OpenAiSubscription,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    #[default]
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerateContent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessTier {
    Paid,
    Subscription,
    FreeTier,
    StarterCredits,
    Experimental,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStrategy {
    OpenAiModels,
    GeminiModels,
    Curated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessType {
    ApiKey,
    Deployment,
    Plan,
    Subscription,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: Option<&'static str>,
    pub editable_base_url: bool,
    pub auth_mode: AuthMode,
    pub auth_scheme: AuthScheme,
    pub default_protocol: WireProtocol,
    pub access_tier: AccessTier,
    pub access_type: AccessType,
    pub discovery_strategy: DiscoveryStrategy,
    pub docs_url: &'static str,
    pub note: Option<&'static str>,
}

macro_rules! preset {
    ($id:literal, $name:literal, $url:expr, $editable:expr, $auth:ident, $scheme:ident, $protocol:ident, $tier:ident, $docs:literal $(, $note:literal)?) => {
        ProviderPreset { id: $id, name: $name, base_url: $url, editable_base_url: $editable,
            auth_mode: AuthMode::$auth, auth_scheme: AuthScheme::$scheme,
            default_protocol: WireProtocol::$protocol, access_tier: AccessTier::$tier,
            access_type: match $id { "poolside" => AccessType::Deployment, "minimax_token_plan" | "zai_coding_plan" => AccessType::Plan, "openai_subscription" => AccessType::Subscription, _ => AccessType::ApiKey },
            discovery_strategy: match $id { "gemini" => DiscoveryStrategy::GeminiModels, "openai_subscription" => DiscoveryStrategy::Curated, _ => DiscoveryStrategy::OpenAiModels },
            docs_url: $docs, note: None$(.or(Some($note)))? }
    };
}

pub fn provider_presets() -> Vec<ProviderPreset> {
    vec![
        preset!(
            "custom_openai",
            "Custom OpenAI-compatible",
            None,
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Paid,
            "https://platform.openai.com/docs/api-reference"
        ),
        preset!(
            "openai",
            "OpenAI API",
            Some("https://api.openai.com/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiResponses,
            Paid,
            "https://developers.openai.com/api/reference/overview"
        ),
        preset!(
            "anthropic",
            "Anthropic",
            Some("https://api.anthropic.com/v1"),
            false,
            ApiKey,
            XApiKey,
            AnthropicMessages,
            Paid,
            "https://docs.anthropic.com/en/api/getting-started"
        ),
        preset!(
            "openai_subscription",
            "OpenAI Subscription",
            Some("https://chatgpt.com/backend-api/codex"),
            false,
            OpenAiSubscription,
            OpenAiSubscription,
            OpenAiResponses,
            Experimental,
            "https://developers.openai.com/codex/auth",
            "Experimental Codex/ChatGPT OAuth integration"
        ),
        preset!(
            "openrouter",
            "OpenRouter",
            Some("https://openrouter.ai/api/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://openrouter.ai/docs"
        ),
        preset!(
            "poolside",
            "Poolside",
            None,
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Subscription,
            "https://docs.poolside.ai/api/overview",
            "Enter your deployment URL ending in /openai/v1"
        ),
        preset!(
            "minimax",
            "MiniMax",
            Some("https://api.minimax.io/v1"),
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Paid,
            "https://platform.minimax.io/docs/api-reference/api-overview"
        ),
        preset!(
            "minimax_token_plan",
            "MiniMax Token Plan",
            Some("https://api.minimax.io/v1"),
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Subscription,
            "https://platform.minimax.io/docs/token-plan/other-tools"
        ),
        preset!(
            "zai",
            "Z.AI",
            Some("https://api.z.ai/api/paas/v4"),
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Paid,
            "https://docs.z.ai/api-reference/introduction"
        ),
        preset!(
            "zai_coding_plan",
            "Z.AI Coding Plan",
            Some("https://api.z.ai/api/coding/paas/v4"),
            true,
            ApiKey,
            Bearer,
            OpenAiChat,
            Subscription,
            "https://docs.z.ai/devpack/tool/others",
            "Intended by Z.AI for supported coding tools"
        ),
        preset!(
            "opencode_zen",
            "OpenCode Zen",
            Some("https://opencode.ai/zen/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://opencode.ai/docs/zen"
        ),
        preset!(
            "gemini",
            "Google Gemini",
            Some("https://generativelanguage.googleapis.com/v1beta"),
            false,
            ApiKey,
            XGoogApiKey,
            GeminiGenerateContent,
            FreeTier,
            "https://ai.google.dev/gemini-api/docs"
        ),
        preset!(
            "groq",
            "Groq",
            Some("https://api.groq.com/openai/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://console.groq.com/docs/openai"
        ),
        preset!(
            "cerebras",
            "Cerebras",
            Some("https://api.cerebras.ai/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://inference-docs.cerebras.ai"
        ),
        preset!(
            "mistral",
            "Mistral",
            Some("https://api.mistral.ai/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://docs.mistral.ai"
        ),
        preset!(
            "huggingface",
            "Hugging Face",
            Some("https://router.huggingface.co/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            StarterCredits,
            "https://huggingface.co/docs/inference-providers"
        ),
        preset!(
            "nvidia_nim",
            "NVIDIA NIM",
            Some("https://integrate.api.nvidia.com/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            FreeTier,
            "https://docs.api.nvidia.com/nim/docs"
        ),
        preset!(
            "sambanova",
            "SambaNova Cloud",
            Some("https://api.sambanova.ai/v1"),
            false,
            ApiKey,
            Bearer,
            OpenAiChat,
            StarterCredits,
            "https://docs.sambanova.ai"
        ),
    ]
}

pub fn provider_preset(id: &str) -> Option<ProviderPreset> {
    provider_presets()
        .into_iter()
        .find(|preset| preset.id == id)
}

pub fn validate_cloud_base_url(value: &str, allow_loopback_http: bool) -> anyhow::Result<String> {
    let url =
        Url::parse(value.trim()).map_err(|_| anyhow::anyhow!("provider base URL is invalid"))?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("provider base URL cannot contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("provider base URL cannot contain a query or fragment");
    }
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if url.scheme() != "https" && !(allow_loopback_http && url.scheme() == "http" && loopback) {
        anyhow::bail!("cloud provider URLs must use HTTPS; HTTP is allowed only for loopback");
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub fn inferred_protocol(preset_id: &str, model: &str) -> WireProtocol {
    match preset_id {
        "openai" | "openai_subscription" => WireProtocol::OpenAiResponses,
        "gemini" => WireProtocol::GeminiGenerateContent,
        "opencode_zen" if model.starts_with("claude-") || model.starts_with("qwen") => {
            WireProtocol::AnthropicMessages
        }
        "opencode_zen" if model.starts_with("gemini-") => WireProtocol::GeminiGenerateContent,
        "opencode_zen" if model.starts_with("gpt-") => WireProtocol::OpenAiResponses,
        _ => provider_preset(preset_id)
            .map(|preset| preset.default_protocol)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_catalog_contains_requested_providers() {
        let ids = provider_presets()
            .into_iter()
            .map(|preset| preset.id)
            .collect::<Vec<_>>();

        for expected in [
            "openai",
            "openai_subscription",
            "openrouter",
            "poolside",
            "minimax",
            "minimax_token_plan",
            "zai",
            "zai_coding_plan",
            "opencode_zen",
            "gemini",
            "groq",
            "cerebras",
            "mistral",
            "huggingface",
            "nvidia_nim",
            "sambanova",
        ] {
            assert!(ids.contains(&expected), "missing preset {expected}");
        }
        assert!(provider_preset("poolside").unwrap().base_url.is_none());
    }

    #[test]
    fn cloud_urls_require_https_except_for_loopback() {
        assert!(validate_cloud_base_url("https://api.example.com/v1", false).is_ok());
        assert!(validate_cloud_base_url("http://127.0.0.1:8000/v1", true).is_ok());
        assert!(validate_cloud_base_url("http://localhost:8000/v1", true).is_ok());
        assert!(validate_cloud_base_url("http://127.0.0.1:8000/v1", false).is_err());
        assert!(validate_cloud_base_url("http://api.example.com/v1", true).is_err());
        assert!(validate_cloud_base_url("https://user:pass@example.com/v1", false).is_err());
        assert!(validate_cloud_base_url("https://example.com/v1?key=secret", false).is_err());
    }
}
