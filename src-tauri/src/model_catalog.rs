use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const CHAT: &[&str] = &["chat", "streaming"];
const CHAT_TOOLS: &[&str] = &["chat", "streaming", "tools", "structured_output"];
const CHAT_VISION: &[&str] = &["chat", "streaming", "tools", "vision", "structured_output"];
const CHAT_REASONING: &[&str] = &[
    "chat",
    "streaming",
    "tools",
    "reasoning",
    "structured_output",
];
const CHAT_VISION_REASONING: &[&str] = &[
    "chat",
    "streaming",
    "tools",
    "vision",
    "reasoning",
    "structured_output",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub capabilities: Vec<String>,
    pub context_window: u64,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    pub task_quality: BTreeMap<String, f64>,
    pub source: MetadataSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSource {
    ProviderApi,
    Catalog,
    Fallback,
}

struct KnownModel {
    keys: &'static [&'static str],
    capabilities: &'static [&'static str],
    context_window: u64,
    input_price_per_million: Option<f64>,
    output_price_per_million: Option<f64>,
    quality_general: f64,
    quality_coding: f64,
    quality_reasoning: f64,
    quality_vision: f64,
}

const KNOWN: &[KnownModel] = &[
    KnownModel {
        keys: &["gpt-4o-mini"],
        capabilities: CHAT_VISION,
        context_window: 128_000,
        input_price_per_million: Some(0.15),
        output_price_per_million: Some(0.60),
        quality_general: 72.0,
        quality_coding: 70.0,
        quality_reasoning: 62.0,
        quality_vision: 70.0,
    },
    KnownModel {
        keys: &["gpt-4o"],
        capabilities: CHAT_VISION,
        context_window: 128_000,
        input_price_per_million: Some(2.50),
        output_price_per_million: Some(10.00),
        quality_general: 88.0,
        quality_coding: 84.0,
        quality_reasoning: 78.0,
        quality_vision: 86.0,
    },
    KnownModel {
        keys: &["gpt-4.1-nano"],
        capabilities: CHAT_VISION,
        context_window: 1_048_576,
        input_price_per_million: Some(0.10),
        output_price_per_million: Some(0.40),
        quality_general: 68.0,
        quality_coding: 70.0,
        quality_reasoning: 60.0,
        quality_vision: 64.0,
    },
    KnownModel {
        keys: &["gpt-4.1-mini"],
        capabilities: CHAT_VISION,
        context_window: 1_048_576,
        input_price_per_million: Some(0.40),
        output_price_per_million: Some(1.60),
        quality_general: 78.0,
        quality_coding: 80.0,
        quality_reasoning: 70.0,
        quality_vision: 74.0,
    },
    KnownModel {
        keys: &["gpt-4.1"],
        capabilities: CHAT_VISION,
        context_window: 1_048_576,
        input_price_per_million: Some(2.00),
        output_price_per_million: Some(8.00),
        quality_general: 90.0,
        quality_coding: 90.0,
        quality_reasoning: 82.0,
        quality_vision: 84.0,
    },
    KnownModel {
        keys: &["gpt-5-mini", "gpt-5.6-luna"],
        capabilities: CHAT_VISION_REASONING,
        context_window: 256_000,
        input_price_per_million: Some(0.25),
        output_price_per_million: Some(2.00),
        quality_general: 82.0,
        quality_coding: 84.0,
        quality_reasoning: 86.0,
        quality_vision: 78.0,
    },
    KnownModel {
        keys: &["gpt-5", "gpt-5.6-sol", "gpt-5.6-terra"],
        capabilities: CHAT_VISION_REASONING,
        context_window: 256_000,
        input_price_per_million: Some(1.25),
        output_price_per_million: Some(10.00),
        quality_general: 94.0,
        quality_coding: 93.0,
        quality_reasoning: 94.0,
        quality_vision: 88.0,
    },
    KnownModel {
        keys: &["o4-mini", "o3-mini"],
        capabilities: CHAT_REASONING,
        context_window: 200_000,
        input_price_per_million: Some(1.10),
        output_price_per_million: Some(4.40),
        quality_general: 84.0,
        quality_coding: 88.0,
        quality_reasoning: 92.0,
        quality_vision: 50.0,
    },
    KnownModel {
        keys: &["o3", "o1"],
        capabilities: CHAT_REASONING,
        context_window: 200_000,
        input_price_per_million: Some(2.00),
        output_price_per_million: Some(8.00),
        quality_general: 90.0,
        quality_coding: 91.0,
        quality_reasoning: 96.0,
        quality_vision: 50.0,
    },
    KnownModel {
        keys: &["claude-opus-4", "claude-4-opus", "claude-3-opus"],
        capabilities: CHAT_VISION_REASONING,
        context_window: 200_000,
        input_price_per_million: Some(15.00),
        output_price_per_million: Some(75.00),
        quality_general: 96.0,
        quality_coding: 94.0,
        quality_reasoning: 95.0,
        quality_vision: 90.0,
    },
    KnownModel {
        keys: &[
            "claude-sonnet-4",
            "claude-4-sonnet",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet",
        ],
        capabilities: CHAT_VISION_REASONING,
        context_window: 200_000,
        input_price_per_million: Some(3.00),
        output_price_per_million: Some(15.00),
        quality_general: 91.0,
        quality_coding: 92.0,
        quality_reasoning: 90.0,
        quality_vision: 88.0,
    },
    KnownModel {
        keys: &[
            "claude-haiku-4",
            "claude-4-haiku",
            "claude-3-5-haiku",
            "claude-3-haiku",
        ],
        capabilities: CHAT_VISION,
        context_window: 200_000,
        input_price_per_million: Some(0.80),
        output_price_per_million: Some(4.00),
        quality_general: 76.0,
        quality_coding: 74.0,
        quality_reasoning: 68.0,
        quality_vision: 72.0,
    },
    KnownModel {
        keys: &["gemini-2.5-pro", "gemini-1.5-pro"],
        capabilities: CHAT_VISION_REASONING,
        context_window: 1_048_576,
        input_price_per_million: Some(1.25),
        output_price_per_million: Some(10.00),
        quality_general: 90.0,
        quality_coding: 86.0,
        quality_reasoning: 88.0,
        quality_vision: 92.0,
    },
    KnownModel {
        keys: &["gemini-2.5-flash", "gemini-2.0-flash", "gemini-1.5-flash"],
        capabilities: CHAT_VISION,
        context_window: 1_048_576,
        input_price_per_million: Some(0.15),
        output_price_per_million: Some(0.60),
        quality_general: 80.0,
        quality_coding: 78.0,
        quality_reasoning: 74.0,
        quality_vision: 86.0,
    },
    KnownModel {
        keys: &["llama-3.3-70b", "llama-3.1-70b"],
        capabilities: CHAT_TOOLS,
        context_window: 128_000,
        input_price_per_million: Some(0.59),
        output_price_per_million: Some(0.79),
        quality_general: 78.0,
        quality_coding: 80.0,
        quality_reasoning: 70.0,
        quality_vision: 40.0,
    },
    KnownModel {
        keys: &["mistral-large"],
        capabilities: CHAT_TOOLS,
        context_window: 128_000,
        input_price_per_million: Some(2.00),
        output_price_per_million: Some(6.00),
        quality_general: 84.0,
        quality_coding: 82.0,
        quality_reasoning: 76.0,
        quality_vision: 40.0,
    },
    KnownModel {
        keys: &["mistral-small", "ministral"],
        capabilities: CHAT_TOOLS,
        context_window: 128_000,
        input_price_per_million: Some(0.10),
        output_price_per_million: Some(0.30),
        quality_general: 70.0,
        quality_coding: 68.0,
        quality_reasoning: 60.0,
        quality_vision: 40.0,
    },
];

pub fn canonicalize_model_id(id: &str) -> String {
    let trimmed = id.trim().trim_start_matches("models/").to_ascii_lowercase();
    if let Some((_, rest)) = trimmed.split_once('/') {
        if !rest.is_empty() {
            return rest.to_owned();
        }
    }
    trimmed
}

pub fn resolve_model_metadata(model_id: &str, api: Option<&Value>) -> ModelMetadata {
    let catalog = lookup_known(model_id);
    let discovered = api.map(parse_discovered);
    merge_metadata(catalog, discovered)
}

pub fn placeholder_capabilities() -> Vec<String> {
    CHAT.iter().map(|item| (*item).to_owned()).collect()
}

pub fn capabilities_are_placeholder(capabilities: &[String]) -> bool {
    let mut items = capabilities.to_vec();
    items.sort();
    items == ["chat".to_string(), "streaming".to_string()]
        || items.is_empty()
        || items == ["chat".to_string()]
}

fn lookup_known(model_id: &str) -> Option<&'static KnownModel> {
    let canonical = canonicalize_model_id(model_id);
    let mut ranked = KNOWN.iter().collect::<Vec<_>>();
    ranked.sort_by_key(|model| {
        std::cmp::Reverse(
            model
                .keys
                .iter()
                .map(|key| key.len())
                .max()
                .unwrap_or_default(),
        )
    });
    ranked.into_iter().find(|model| {
        model.keys.iter().any(|key| {
            canonical == *key
                || canonical.starts_with(&format!("{key}-"))
                || canonical.starts_with(&format!("{key}."))
        })
    })
}

fn known_to_metadata(model: &KnownModel) -> ModelMetadata {
    let mut task_quality = BTreeMap::new();
    task_quality.insert("general".into(), model.quality_general);
    task_quality.insert("coding".into(), model.quality_coding);
    task_quality.insert("reasoning".into(), model.quality_reasoning);
    if model.capabilities.contains(&"vision") {
        task_quality.insert("vision".into(), model.quality_vision);
    }
    ModelMetadata {
        capabilities: model
            .capabilities
            .iter()
            .map(|item| (*item).to_owned())
            .collect(),
        context_window: model.context_window,
        input_price_per_million: model.input_price_per_million,
        output_price_per_million: model.output_price_per_million,
        task_quality,
        source: MetadataSource::Catalog,
    }
}

fn fallback_metadata() -> ModelMetadata {
    let mut task_quality = BTreeMap::new();
    task_quality.insert("general".into(), 50.0);
    ModelMetadata {
        capabilities: placeholder_capabilities(),
        context_window: 8_192,
        input_price_per_million: None,
        output_price_per_million: None,
        task_quality,
        source: MetadataSource::Fallback,
    }
}

#[derive(Default)]
struct Discovered {
    capabilities: Vec<String>,
    context_window: Option<u64>,
    input_price_per_million: Option<f64>,
    output_price_per_million: Option<f64>,
}

fn parse_discovered(value: &Value) -> Discovered {
    let mut discovered = Discovered::default();
    discovered.context_window = first_u64(
        value,
        &[
            "context_length",
            "context_window",
            "max_context_length",
            "inputTokenLimit",
            "input_token_limit",
        ],
    );
    let pricing = value.get("pricing").or_else(|| value.get("price"));
    discovered.input_price_per_million = pricing_field(pricing, &["prompt", "input", "input_cost"])
        .or_else(|| first_f64(value, &["input_price_per_million", "input_price"]));
    discovered.output_price_per_million =
        pricing_field(pricing, &["completion", "output", "output_cost"])
            .or_else(|| first_f64(value, &["output_price_per_million", "output_price"]));
    push_unique(&mut discovered.capabilities, capabilities_from_api(value));
    discovered
}

fn capabilities_from_api(value: &Value) -> Vec<String> {
    let mut capabilities = placeholder_capabilities();
    let modality = value
        .pointer("/architecture/modality")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let input_modalities = string_list(
        value
            .pointer("/architecture/input_modalities")
            .or_else(|| value.get("input_modalities")),
    );
    if modality.contains("image") || input_modalities.iter().any(|item| item.contains("image")) {
        push_unique(&mut capabilities, ["vision".into()]);
    }
    if modality.contains("audio") || input_modalities.iter().any(|item| item.contains("audio")) {
        push_unique(&mut capabilities, ["audio_input".into()]);
    }
    if modality.contains("video") || input_modalities.iter().any(|item| item.contains("video")) {
        push_unique(&mut capabilities, ["video_input".into()]);
    }
    let parameters = string_list(
        value
            .get("supported_parameters")
            .or_else(|| value.get("supported_features"))
            .or_else(|| value.get("capabilities")),
    );
    for parameter in parameters {
        match parameter.as_str() {
            "tools" | "tool_use" | "function_calling" => {
                push_unique(&mut capabilities, ["tools".into()])
            }
            "response_format" | "structured_output" | "json_schema" => {
                push_unique(&mut capabilities, ["structured_output".into()])
            }
            "reasoning" | "thinking" => push_unique(&mut capabilities, ["reasoning".into()]),
            "vision" | "image" => push_unique(&mut capabilities, ["vision".into()]),
            "audio" | "audio_input" => push_unique(&mut capabilities, ["audio_input".into()]),
            "streaming" | "chat" => {}
            other if other == "embeddings" || other == "embedcontent" => {
                push_unique(&mut capabilities, ["embeddings".into()])
            }
            _ => {}
        }
    }
    let methods = string_list(value.get("supportedGenerationMethods"));
    if methods.iter().any(|item| item == "embedcontent") {
        push_unique(&mut capabilities, ["embeddings".into()]);
    }
    capabilities
}

fn merge_metadata(catalog: Option<&KnownModel>, discovered: Option<Discovered>) -> ModelMetadata {
    let mut meta = catalog
        .map(known_to_metadata)
        .unwrap_or_else(fallback_metadata);
    let Some(discovered) = discovered else {
        return meta;
    };
    if discovered.context_window.is_some()
        || discovered.input_price_per_million.is_some()
        || discovered.output_price_per_million.is_some()
        || discovered.capabilities.iter().any(|item| {
            !placeholder_capabilities().contains(item) && !meta.capabilities.contains(item)
        })
    {
        meta.source = MetadataSource::ProviderApi;
    }
    if let Some(context) = discovered.context_window {
        meta.context_window = context;
    }
    if discovered.input_price_per_million.is_some() {
        meta.input_price_per_million = discovered.input_price_per_million;
    }
    if discovered.output_price_per_million.is_some() {
        meta.output_price_per_million = discovered.output_price_per_million;
    }
    for capability in discovered.capabilities {
        push_unique(&mut meta.capabilities, [capability]);
    }
    meta
}

fn pricing_field(pricing: Option<&Value>, keys: &[&str]) -> Option<f64> {
    let pricing = pricing?;
    for key in keys {
        if let Some(value) = number_like(pricing.get(*key)?) {
            return Some(normalize_price(value));
        }
    }
    None
}

fn normalize_price(value: f64) -> f64 {
    if value > 0.0 && value < 0.05 {
        value * 1_000_000.0
    } else {
        value
    }
}

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_f64().map(|number| number as u64))
                .or_else(|| item.as_str().and_then(|text| text.parse().ok()))
        })
    })
}

fn first_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(number_like))
}

fn number_like(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .or_else(|| value.as_str().and_then(parse_decimal))
}

fn parse_decimal(text: &str) -> Option<f64> {
    let trimmed = text.trim().replace([' ', '\u{00a0}'], "");
    if trimmed.is_empty() {
        return None;
    }
    let last_comma = trimmed.rfind(',');
    let last_dot = trimmed.rfind('.');
    let normalized = match (last_comma, last_dot) {
        (Some(comma), Some(dot)) if comma > dot => trimmed.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => trimmed.replace(',', ""),
        (Some(comma), None) => {
            let (int, frac) = trimmed.split_at(comma);
            format!("{}{}", int.replace(',', ""), frac.replace(',', "."))
        }
        _ => trimmed,
    };
    normalized.parse().ok().filter(|value: &f64| value.is_finite())
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_ascii_lowercase))
            .collect(),
        Some(Value::String(item)) => vec![item.to_ascii_lowercase()],
        _ => Vec::new(),
    }
}

fn push_unique(target: &mut Vec<String>, items: impl IntoIterator<Item = String>) {
    for item in items {
        if !target.contains(&item) {
            target.push(item);
        }
    }
}

pub fn extract_model_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(|id| id.trim_start_matches("models/").to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_gpt4o_has_vision_tools_and_list_prices() {
        let meta = resolve_model_metadata("gpt-4o", None);
        assert!(meta.capabilities.contains(&"vision".to_string()));
        assert!(meta.capabilities.contains(&"tools".to_string()));
        assert_eq!(meta.input_price_per_million, Some(2.50));
        assert_eq!(meta.output_price_per_million, Some(10.00));
        assert_eq!(meta.source, MetadataSource::Catalog);
        assert!(meta.task_quality.get("coding").copied().unwrap() > 50.0);
    }

    #[test]
    fn openrouter_prefix_and_dated_suffix_use_the_same_defaults() {
        let direct = resolve_model_metadata("gpt-4o-mini", None);
        let prefixed = resolve_model_metadata("openai/gpt-4o-mini", None);
        let dated = resolve_model_metadata("gpt-4o-mini-2024-07-18", None);
        assert_eq!(
            direct.input_price_per_million,
            prefixed.input_price_per_million
        );
        assert_eq!(
            direct.input_price_per_million,
            dated.input_price_per_million
        );
        assert_eq!(direct.input_price_per_million, Some(0.15));
    }

    #[test]
    fn gpt4o_mini_does_not_inherit_full_gpt4o_price() {
        assert_ne!(
            resolve_model_metadata("gpt-4o-mini", None).input_price_per_million,
            resolve_model_metadata("gpt-4o", None).input_price_per_million
        );
    }

    #[test]
    fn unknown_model_keeps_conservative_chat_defaults() {
        let meta = resolve_model_metadata("my-finetune-v3", None);
        assert_eq!(meta.capabilities, vec!["chat", "streaming"]);
        assert_eq!(meta.input_price_per_million, None);
        assert_eq!(meta.source, MetadataSource::Fallback);
    }

    #[test]
    fn openrouter_api_pricing_overrides_catalog_and_adds_audio() {
        let api = json!({
            "id": "openai/gpt-4o",
            "context_length": 200000,
            "pricing": { "prompt": "0.000001", "completion": "0.000004" },
            "architecture": { "modality": "text+image+audio->text" },
            "supported_parameters": ["tools", "response_format"]
        });
        let meta = resolve_model_metadata("openai/gpt-4o", Some(&api));
        assert_eq!(meta.input_price_per_million, Some(1.0));
        assert_eq!(meta.output_price_per_million, Some(4.0));
        assert_eq!(meta.context_window, 200_000);
        assert!(meta.capabilities.contains(&"audio_input".to_string()));
        assert_eq!(meta.source, MetadataSource::ProviderApi);
    }

    #[test]
    fn api_without_pricing_keeps_known_list_prices() {
        let api = json!({ "id": "gpt-4o", "owned_by": "openai" });
        let meta = resolve_model_metadata("gpt-4o", Some(&api));
        assert_eq!(meta.input_price_per_million, Some(2.50));
        assert!(meta.capabilities.contains(&"vision".to_string()));
    }

    #[test]
    fn placeholder_capabilities_are_replaced_for_known_models() {
        let meta = resolve_model_metadata("gpt-4o", None);
        assert!(capabilities_are_placeholder(&[
            "chat".into(),
            "streaming".into()
        ]));
        assert!(!capabilities_are_placeholder(&meta.capabilities));
        assert_eq!(meta.input_price_per_million, Some(2.50));
        assert_eq!(meta.context_window, 128_000);
    }

    #[test]
    fn comma_decimal_prices_are_accepted() {
        let api = json!({
            "id": "custom-model",
            "pricing": { "prompt": "0,15", "completion": "1,25" }
        });
        let meta = resolve_model_metadata("custom-model", Some(&api));
        assert_eq!(meta.input_price_per_million, Some(0.15));
        assert_eq!(meta.output_price_per_million, Some(1.25));
    }
}
