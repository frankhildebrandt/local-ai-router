use std::collections::HashMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub use crate::providers::WireProtocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicProtocol {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Gemini,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        url: String,
        media_type: Option<String>,
    },
    Reasoning {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalRequest {
    pub system: Vec<ContentBlock>,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<CanonicalTool>,
    pub tool_choice: Option<Value>,
    pub parallel_tool_calls: Option<bool>,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Option<Value>,
    pub reasoning: Option<Value>,
    pub response_format: Option<Value>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

pub fn decode_request(
    protocol: PublicProtocol,
    value: &Value,
    path_model: Option<&str>,
) -> anyhow::Result<CanonicalRequest> {
    if path_model.is_none() && value.get("model").and_then(Value::as_str).is_none() {
        anyhow::bail!("model is required");
    }
    let mut request = CanonicalRequest {
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: value.get("tool_choice").cloned(),
        parallel_tool_calls: value.get("parallel_tool_calls").and_then(Value::as_bool),
        max_tokens: value
            .get("max_tokens")
            .or_else(|| value.get("max_completion_tokens"))
            .or_else(|| value.get("max_output_tokens"))
            .and_then(Value::as_u64),
        temperature: value.get("temperature").and_then(Value::as_f64),
        top_p: value.get("top_p").and_then(Value::as_f64),
        stop: value.get("stop").cloned(),
        reasoning: value
            .get("reasoning")
            .cloned()
            .or_else(|| value.get("reasoning_effort").cloned()),
        response_format: value
            .get("response_format")
            .cloned()
            .or_else(|| value.pointer("/text/format").cloned()),
        stream: value
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    match protocol {
        PublicProtocol::OpenAiChat => {
            decode_openai_messages(value.get("messages"), &mut request)?;
            fill_openai_tools(value, &mut request);
        }
        PublicProtocol::OpenAiResponses => decode_responses_input(value, &mut request)?,
        PublicProtocol::Anthropic => decode_anthropic(value, &mut request)?,
        PublicProtocol::Gemini => decode_gemini(value, &mut request)?,
    }
    Ok(request)
}

fn decode_openai_messages(
    messages: Option<&Value>,
    request: &mut CanonicalRequest,
) -> anyhow::Result<()> {
    for message in messages
        .and_then(Value::as_array)
        .context("messages must be an array")?
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .context("message role is required")?;
        let blocks = openai_content(message.get("content"));
        if matches!(role, "system" | "developer") {
            request.system.extend(blocks);
            continue;
        }
        let mut blocks = blocks;
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let function = &call["function"];
                blocks.push(ContentBlock::ToolUse {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool-call")
                        .into(),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .into(),
                    input: parse_json_or_string(
                        function.get("arguments").cloned().unwrap_or(Value::Null),
                    ),
                });
            }
        }
        if role == "tool" {
            blocks = vec![ContentBlock::ToolResult {
                tool_use_id: message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .into(),
                content: message.get("content").cloned().unwrap_or(Value::Null),
            }];
        }
        request.messages.push(CanonicalMessage {
            role: role.into(),
            content: blocks,
        });
    }
    Ok(())
}

fn fill_openai_tools(value: &Value, request: &mut CanonicalRequest) {
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            let function = tool.get("function").unwrap_or(tool);
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                request.tools.push(CanonicalTool {
                    name: name.into(),
                    description: function
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    input_schema: function
                        .get("parameters")
                        .or_else(|| function.get("input_schema"))
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object"})),
                });
            }
        }
    }
}

fn decode_responses_input(value: &Value, request: &mut CanonicalRequest) -> anyhow::Result<()> {
    if let Some(format) = request.response_format.take() {
        request.response_format = Some(canonical_response_format(format));
    }
    if let Some(instructions) = value.get("instructions").and_then(Value::as_str) {
        request.system.push(ContentBlock::Text {
            text: instructions.into(),
        });
    }
    match value.get("input") {
        Some(Value::String(text)) => request.messages.push(CanonicalMessage {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: text.clone() }],
        }),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => request.messages.push(CanonicalMessage {
                        role: "assistant".into(),
                        content: vec![ContentBlock::ToolUse {
                            id: item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("tool-call")
                                .into(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .into(),
                            input: parse_json_or_string(
                                item.get("arguments").cloned().unwrap_or(Value::Null),
                            ),
                        }],
                    }),
                    Some("function_call_output") => request.messages.push(CanonicalMessage {
                        role: "user".into(),
                        content: vec![ContentBlock::ToolResult {
                            tool_use_id: item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("tool-call")
                                .into(),
                            content: item.get("output").cloned().unwrap_or(Value::Null),
                        }],
                    }),
                    _ => decode_openai_messages(Some(&json!([item])), request)?,
                }
            }
        }
        Some(_) => anyhow::bail!("input must be a string or array"),
        None => anyhow::bail!("input is required"),
    }
    fill_openai_tools(value, request);
    Ok(())
}

fn decode_anthropic(value: &Value, request: &mut CanonicalRequest) -> anyhow::Result<()> {
    request.parallel_tool_calls = value
        .pointer("/tool_choice/disable_parallel_tool_use")
        .and_then(Value::as_bool)
        .map(|disabled| !disabled);
    request.stop = value.get("stop_sequences").cloned();
    request.reasoning = value.get("thinking").cloned();
    if let Some(system) = value.get("system") {
        request.system.extend(anthropic_content(Some(system)));
    }
    for message in value
        .get("messages")
        .and_then(Value::as_array)
        .context("messages must be an array")?
    {
        request.messages.push(CanonicalMessage {
            role: message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .into(),
            content: anthropic_content(message.get("content")),
        });
    }
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            request.tools.push(CanonicalTool {
                name: tool
                    .get("name")
                    .and_then(Value::as_str)
                    .context("tool name is required")?
                    .into(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                input_schema: tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"})),
            });
        }
    }
    Ok(())
}

fn decode_gemini(value: &Value, request: &mut CanonicalRequest) -> anyhow::Result<()> {
    request.tool_choice = value.pointer("/toolConfig/functionCallingConfig").cloned();
    if let Some(parts) = value.pointer("/systemInstruction/parts") {
        request.system.extend(gemini_parts(Some(parts)));
    }
    for content in value
        .get("contents")
        .and_then(Value::as_array)
        .context("contents must be an array")?
    {
        let role = match content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
        {
            "model" => "assistant",
            other => other,
        };
        request.messages.push(CanonicalMessage {
            role: role.into(),
            content: gemini_parts(content.get("parts")),
        });
    }
    if let Some(declarations) = value
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
    {
        for tool in declarations {
            request.tools.push(CanonicalTool {
                name: tool
                    .get("name")
                    .and_then(Value::as_str)
                    .context("tool name is required")?
                    .into(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                input_schema: tool
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"})),
            });
        }
    }
    request.max_tokens = request.max_tokens.or_else(|| {
        value
            .pointer("/generationConfig/maxOutputTokens")
            .and_then(Value::as_u64)
    });
    request.temperature = request.temperature.or_else(|| {
        value
            .pointer("/generationConfig/temperature")
            .and_then(Value::as_f64)
    });
    request.top_p = request.top_p.or_else(|| {
        value
            .pointer("/generationConfig/topP")
            .and_then(Value::as_f64)
    });
    if request.stop.is_none() {
        request.stop = value.pointer("/generationConfig/stopSequences").cloned();
    }
    if request.response_format.is_none() {
        request.response_format = value
            .pointer("/generationConfig/responseSchema")
            .cloned()
            .map(|schema| json!({"type":"json_schema","json_schema":{"schema":schema}}));
    }
    Ok(())
}

pub fn encode_request(
    protocol: WireProtocol,
    request: &CanonicalRequest,
    model: &str,
) -> anyhow::Result<Value> {
    if protocol == WireProtocol::AnthropicMessages && request.response_format.is_some() {
        anyhow::bail!("structured output cannot be translated safely to Anthropic Messages");
    }
    if let Some(reasoning) = &request.reasoning {
        match protocol {
            WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses
                if reasoning_effort(reasoning).is_none() =>
            {
                anyhow::bail!("reasoning configuration cannot be translated safely to OpenAI")
            }
            WireProtocol::AnthropicMessages
                if reasoning.get("type").and_then(Value::as_str) != Some("enabled")
                    || reasoning
                        .get("budget_tokens")
                        .and_then(Value::as_u64)
                        .is_none() =>
            {
                anyhow::bail!(
                    "reasoning configuration cannot be translated safely to Anthropic Messages"
                )
            }
            _ => {}
        }
    }
    if protocol == WireProtocol::GeminiGenerateContent && request.reasoning.is_some() {
        anyhow::bail!(
            "reasoning configuration cannot be translated safely to Gemini GenerateContent"
        );
    }
    if protocol == WireProtocol::GeminiGenerateContent && request.parallel_tool_calls == Some(false)
    {
        anyhow::bail!("parallel tool calls cannot be disabled safely for Gemini GenerateContent");
    }
    if matches!(
        protocol,
        WireProtocol::AnthropicMessages | WireProtocol::GeminiGenerateContent
    ) && request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .any(|block| matches!(block, ContentBlock::Image { url, .. } if data_url(url).is_none()))
    {
        anyhow::bail!("remote image URLs cannot be translated safely to this protocol");
    }
    Ok(match protocol {
        WireProtocol::OpenAiChat => encode_openai_chat_request(request, model),
        WireProtocol::OpenAiResponses => encode_openai_responses_request(request, model),
        WireProtocol::AnthropicMessages => encode_anthropic_request(request, model),
        WireProtocol::GeminiGenerateContent => encode_gemini_request(request),
    })
}

fn encode_openai_chat_request(request: &CanonicalRequest, model: &str) -> Value {
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        messages.push(json!({"role":"system","content": blocks_text(&request.system)}));
    }
    messages.extend(request.messages.iter().map(openai_message));
    let mut root = json!({"model":model,"messages":messages,"stream":request.stream});
    if let Some(v) = request.max_tokens {
        root["max_tokens"] = v.into();
    }
    if let Some(v) = request.temperature {
        root["temperature"] = json!(v);
    }
    if let Some(v) = request.top_p {
        root["top_p"] = json!(v);
    }
    if let Some(v) = &request.stop {
        root["stop"] = v.clone();
    }
    if let Some(v) = request.reasoning.as_ref().and_then(reasoning_effort) {
        root["reasoning_effort"] = Value::String(v.into());
    }
    if !request.tools.is_empty() {
        root["tools"] = Value::Array(request.tools.iter().map(|tool| json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":tool.input_schema}})).collect());
    }
    if let Some(v) = &request.tool_choice {
        root["tool_choice"] = openai_tool_choice(v);
    }
    if let Some(v) = request.parallel_tool_calls {
        root["parallel_tool_calls"] = Value::Bool(v);
    }
    if let Some(v) = &request.response_format {
        root["response_format"] = v.clone();
    }
    root
}

fn encode_openai_responses_request(request: &CanonicalRequest, model: &str) -> Value {
    let mut root = json!({"model":model,"input":request.messages.iter().flat_map(openai_responses_message).collect::<Vec<_>>(),"stream":request.stream,"store":false});
    if !request.system.is_empty() {
        root["instructions"] = Value::String(blocks_text(&request.system));
    }
    if let Some(v) = request.max_tokens {
        root["max_output_tokens"] = v.into();
    }
    if let Some(v) = request.temperature {
        root["temperature"] = json!(v);
    }
    if let Some(v) = request.top_p {
        root["top_p"] = json!(v);
    }
    if let Some(v) = request.reasoning.as_ref().and_then(reasoning_effort) {
        root["reasoning"] = json!({"effort":v});
    }
    if !request.tools.is_empty() {
        root["tools"] = Value::Array(request.tools.iter().map(|tool| json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.input_schema})).collect());
    }
    if let Some(v) = &request.tool_choice {
        root["tool_choice"] = openai_tool_choice(v);
    }
    if let Some(v) = request.parallel_tool_calls {
        root["parallel_tool_calls"] = Value::Bool(v);
    }
    if let Some(v) = &request.response_format {
        root["text"] = json!({"format":responses_response_format(v)});
    }
    root
}

fn encode_anthropic_request(request: &CanonicalRequest, model: &str) -> Value {
    let mut root = json!({"model":model,"max_tokens":request.max_tokens.unwrap_or(4096),"messages":request.messages.iter().map(anthropic_message).collect::<Vec<_>>(),"stream":request.stream});
    if !request.system.is_empty() {
        root["system"] = Value::String(blocks_text(&request.system));
    }
    if !request.tools.is_empty() {
        root["tools"] = Value::Array(request.tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"input_schema":tool.input_schema})).collect());
    }
    if let Some(v) = request.temperature {
        root["temperature"] = json!(v);
    }
    if let Some(v) = request.top_p {
        root["top_p"] = json!(v);
    }
    if let Some(v) = &request.stop {
        root["stop_sequences"] = if v.is_string() { json!([v]) } else { v.clone() };
    }
    if let Some(v) = &request.reasoning {
        root["thinking"] = v.clone();
    }
    if request.tool_choice.is_some() || request.parallel_tool_calls.is_some() {
        let mut choice = request
            .tool_choice
            .as_ref()
            .map(anthropic_tool_choice)
            .unwrap_or_else(|| json!({"type":"auto"}));
        if let Some(parallel) = request.parallel_tool_calls {
            choice["disable_parallel_tool_use"] = Value::Bool(!parallel);
        }
        root["tool_choice"] = choice;
    }
    root
}

fn encode_gemini_request(request: &CanonicalRequest) -> Value {
    let mut root =
        json!({"contents":request.messages.iter().map(gemini_message).collect::<Vec<_>>()});
    if !request.system.is_empty() {
        root["systemInstruction"] = json!({"parts":[{"text":blocks_text(&request.system)}]});
    }
    if !request.tools.is_empty() {
        root["tools"] = json!([{"functionDeclarations":request.tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"parameters":tool.input_schema})).collect::<Vec<_>>()}]);
    }
    let mut generation = Map::new();
    if let Some(v) = request.max_tokens {
        generation.insert("maxOutputTokens".into(), v.into());
    }
    if let Some(v) = request.temperature {
        generation.insert("temperature".into(), json!(v));
    }
    if let Some(v) = request.top_p {
        generation.insert("topP".into(), json!(v));
    }
    if let Some(v) = &request.stop {
        generation.insert(
            "stopSequences".into(),
            if v.is_string() { json!([v]) } else { v.clone() },
        );
    }
    if let Some(v) = &request.response_format {
        generation.insert("responseMimeType".into(), "application/json".into());
        if let Some(schema) = v.pointer("/json_schema/schema").or_else(|| v.get("schema")) {
            generation.insert("responseSchema".into(), schema.clone());
        }
    }
    if !generation.is_empty() {
        root["generationConfig"] = Value::Object(generation);
    }
    if let Some(v) = &request.tool_choice {
        root["toolConfig"] = gemini_tool_choice(v);
    }
    root
}

fn anthropic_tool_choice(value: &Value) -> Value {
    if let Some(mode) = value.get("mode").and_then(Value::as_str) {
        return match mode {
            "NONE" => json!({"type":"none"}),
            "ANY" => json!({"type":"any"}),
            _ => json!({"type":"auto"}),
        };
    }
    match value.as_str() {
        Some("auto") => json!({"type":"auto"}),
        Some("none") => json!({"type":"none"}),
        Some("required") => json!({"type":"any"}),
        _ => value
            .get("function")
            .and_then(|v| v.get("name"))
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .map(|name| json!({"type":"tool","name":name}))
            .unwrap_or_else(|| value.clone()),
    }
}

fn canonical_response_format(value: Value) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("json_schema")
        && value.get("json_schema").is_none()
    {
        return json!({"type":"json_schema","json_schema":{"name":value.get("name").cloned().unwrap_or_else(||Value::String("response".into())),"schema":value.get("schema").cloned().unwrap_or_else(||json!({"type":"object"})),"strict":value.get("strict").cloned().unwrap_or(Value::Bool(false))}});
    }
    value
}

fn responses_response_format(value: &Value) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("json_schema") {
        if let Some(schema) = value.get("json_schema") {
            return json!({"type":"json_schema","name":schema.get("name").cloned().unwrap_or_else(||Value::String("response".into())),"schema":schema.get("schema").cloned().unwrap_or_else(||json!({"type":"object"})),"strict":schema.get("strict").cloned().unwrap_or(Value::Bool(false))});
        }
    }
    value.clone()
}

fn reasoning_effort(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("effort").and_then(Value::as_str))
}

fn openai_tool_choice(value: &Value) -> Value {
    if let Some(mode) = value.get("mode").and_then(Value::as_str) {
        return Value::String(
            match mode {
                "NONE" => "none",
                "ANY" => "required",
                _ => "auto",
            }
            .into(),
        );
    }
    match value.get("type").and_then(Value::as_str) {
        Some("auto" | "none") => Value::String(value["type"].as_str().unwrap().into()),
        Some("any") => Value::String("required".into()),
        Some("tool") => {
            json!({"type":"function","function":{"name":value.get("name").and_then(Value::as_str).unwrap_or("tool")}})
        }
        _ => value.clone(),
    }
}

fn gemini_tool_choice(value: &Value) -> Value {
    let named = value
        .pointer("/function/name")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str);
    let mode = match value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str))
    {
        Some("none") => "NONE",
        Some("required" | "any" | "tool") => "ANY",
        _ => "AUTO",
    };
    let mut config = json!({"mode":mode});
    if let Some(name) = named {
        config["allowedFunctionNames"] = json!([name]);
    }
    json!({"functionCallingConfig":config})
}

pub fn validate_cross_protocol(protocol: PublicProtocol, value: &Value) -> anyhow::Result<()> {
    let allowed: &[&str] = match protocol {
        PublicProtocol::OpenAiChat => &[
            "model",
            "messages",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "max_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "stop",
            "response_format",
            "reasoning_effort",
            "stream",
        ],
        PublicProtocol::OpenAiResponses => &[
            "model",
            "input",
            "instructions",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "max_output_tokens",
            "temperature",
            "top_p",
            "reasoning",
            "text",
            "stream",
            "store",
        ],
        PublicProtocol::Anthropic => &[
            "model",
            "messages",
            "system",
            "tools",
            "tool_choice",
            "max_tokens",
            "temperature",
            "top_p",
            "stop_sequences",
            "thinking",
            "stream",
        ],
        PublicProtocol::Gemini => &[
            "contents",
            "systemInstruction",
            "tools",
            "toolConfig",
            "generationConfig",
            "stream",
        ],
    };
    let object = value.as_object().context("request must be a JSON object")?;
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        anyhow::bail!("field `{field}` cannot be translated safely");
    }
    validate_nested_content(protocol, value)?;
    Ok(())
}

fn validate_nested_content(protocol: PublicProtocol, value: &Value) -> anyhow::Result<()> {
    let messages = match protocol {
        PublicProtocol::OpenAiChat => value.get("messages"),
        PublicProtocol::OpenAiResponses => value.get("input"),
        PublicProtocol::Anthropic => value.get("messages"),
        PublicProtocol::Gemini => value.get("contents"),
    };
    for message in messages.and_then(Value::as_array).into_iter().flatten() {
        let blocks = if protocol == PublicProtocol::Gemini {
            message.get("parts")
        } else {
            message.get("content")
        };
        for block in blocks.and_then(Value::as_array).into_iter().flatten() {
            match protocol {
                PublicProtocol::OpenAiChat | PublicProtocol::OpenAiResponses
                    if !matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("text" | "input_text" | "output_text" | "image_url" | "input_image")
                    ) =>
                {
                    anyhow::bail!("unsupported OpenAI content block cannot be translated safely")
                }
                PublicProtocol::Anthropic => {
                    let kind = block.get("type").and_then(Value::as_str);
                    if !matches!(
                        kind,
                        Some("text" | "thinking" | "tool_use" | "tool_result" | "image")
                    ) {
                        anyhow::bail!(
                            "unsupported Anthropic content block cannot be translated safely"
                        );
                    }
                    if kind == Some("image")
                        && block.pointer("/source/type").and_then(Value::as_str) != Some("base64")
                    {
                        anyhow::bail!("non-base64 Anthropic images cannot be translated safely");
                    }
                }
                PublicProtocol::Gemini
                    if !(block.get("text").is_some()
                        || block.get("functionCall").is_some()
                        || block.get("functionResponse").is_some()
                        || block.get("inlineData").is_some()) =>
                {
                    anyhow::bail!("unsupported Gemini part cannot be translated safely")
                }
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn decode_response(protocol: WireProtocol, value: &Value) -> anyhow::Result<CanonicalResponse> {
    match protocol {
        WireProtocol::OpenAiChat => decode_openai_chat_response(value),
        WireProtocol::OpenAiResponses => decode_openai_responses_response(value),
        WireProtocol::AnthropicMessages => decode_anthropic_response(value),
        WireProtocol::GeminiGenerateContent => decode_gemini_response(value),
    }
}

fn decode_openai_chat_response(value: &Value) -> anyhow::Result<CanonicalResponse> {
    let choice = value
        .pointer("/choices/0")
        .context("provider response has no choice")?;
    let message = choice
        .get("message")
        .context("provider response has no message")?;
    let mut content = openai_content(message.get("content"));
    if let Some(reasoning) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
    {
        content.push(ContentBlock::Reasoning {
            text: reasoning.into(),
        });
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            content.push(ContentBlock::ToolUse {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .into(),
                name: call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .into(),
                input: parse_json_or_string(
                    call.pointer("/function/arguments")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
            });
        }
    }
    Ok(CanonicalResponse {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("response")
            .into(),
        content,
        stop_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: value
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64),
        output_tokens: value
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64),
    })
}

fn decode_openai_responses_response(value: &Value) -> anyhow::Result<CanonicalResponse> {
    let mut content = Vec::new();
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .context("provider response has no output")?
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for block in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        content.push(ContentBlock::Text { text: text.into() });
                    }
                }
            }
            Some("function_call") => content.push(ContentBlock::ToolUse {
                id: item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .into(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .into(),
                input: parse_json_or_string(item.get("arguments").cloned().unwrap_or(Value::Null)),
            }),
            Some("reasoning") => {
                if let Some(text) = item.pointer("/summary/0/text").and_then(Value::as_str) {
                    content.push(ContentBlock::Reasoning { text: text.into() });
                }
            }
            _ => {}
        }
    }
    Ok(CanonicalResponse {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("response")
            .into(),
        content,
        stop_reason: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: value.pointer("/usage/input_tokens").and_then(Value::as_u64),
        output_tokens: value
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64),
    })
}

fn decode_anthropic_response(value: &Value) -> anyhow::Result<CanonicalResponse> {
    Ok(CanonicalResponse {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("message")
            .into(),
        content: anthropic_content(value.get("content")),
        stop_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: value.pointer("/usage/input_tokens").and_then(Value::as_u64),
        output_tokens: value
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64),
    })
}

fn decode_gemini_response(value: &Value) -> anyhow::Result<CanonicalResponse> {
    Ok(CanonicalResponse {
        id: value
            .get("responseId")
            .and_then(Value::as_str)
            .unwrap_or("gemini-response")
            .into(),
        content: gemini_parts(value.pointer("/candidates/0/content/parts")),
        stop_reason: value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: value
            .pointer("/usageMetadata/promptTokenCount")
            .and_then(Value::as_u64),
        output_tokens: value
            .pointer("/usageMetadata/candidatesTokenCount")
            .and_then(Value::as_u64),
    })
}

pub fn encode_response(
    protocol: PublicProtocol,
    response: &CanonicalResponse,
    model: &str,
) -> Value {
    match protocol {
        PublicProtocol::OpenAiChat => {
            json!({"id":response.id,"object":"chat.completion","model":model,"choices":[{"index":0,"message":openai_assistant(response),"finish_reason":openai_stop(response.stop_reason.as_deref())}],"usage":{"prompt_tokens":response.input_tokens,"completion_tokens":response.output_tokens,"total_tokens":response.input_tokens.unwrap_or(0)+response.output_tokens.unwrap_or(0)}})
        }
        PublicProtocol::OpenAiResponses => {
            json!({"id":response.id,"object":"response","status":"completed","model":model,"output":responses_output(response),"usage":{"input_tokens":response.input_tokens,"output_tokens":response.output_tokens,"total_tokens":response.input_tokens.unwrap_or(0)+response.output_tokens.unwrap_or(0)}})
        }
        PublicProtocol::Anthropic => {
            json!({"id":response.id,"type":"message","role":"assistant","model":model,"content":response.content.iter().filter_map(anthropic_block).collect::<Vec<_>>(),"stop_reason":anthropic_stop(response.stop_reason.as_deref()),"usage":{"input_tokens":response.input_tokens,"output_tokens":response.output_tokens}})
        }
        PublicProtocol::Gemini => {
            json!({"responseId":response.id,"candidates":[{"content":{"role":"model","parts":response.content.iter().filter_map(gemini_block).collect::<Vec<_>>()},"finishReason":gemini_stop(response.stop_reason.as_deref())}],"usageMetadata":{"promptTokenCount":response.input_tokens,"candidatesTokenCount":response.output_tokens,"totalTokenCount":response.input_tokens.unwrap_or(0)+response.output_tokens.unwrap_or(0)}})
        }
    }
}

fn responses_output(response: &CanonicalResponse) -> Vec<Value> {
    let text = response
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text { text } = block {
                Some(json!({"type":"output_text","text":text,"annotations":[]}))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({"id":format!("msg_{}",response.id),"type":"message","role":"assistant","content":text}));
    }
    for block in &response.content {
        match block { ContentBlock::ToolUse{id,name,input}=>output.push(json!({"id":id,"type":"function_call","call_id":id,"name":name,"arguments":input.to_string(),"status":"completed"})),ContentBlock::Reasoning{text}=>output.push(json!({"id":format!("reasoning_{}",response.id),"type":"reasoning","summary":[{"type":"summary_text","text":text}]})),_=>{} }
    }
    output
}

fn openai_content(value: Option<&Value>) -> Vec<ContentBlock> {
    match value {
        Some(Value::String(text)) => vec![ContentBlock::Text { text: text.clone() }],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text" | "input_text" | "output_text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| ContentBlock::Text { text: text.into() }),
                Some("image_url" | "input_image") => block
                    .pointer("/image_url/url")
                    .or_else(|| block.get("image_url"))
                    .and_then(Value::as_str)
                    .map(|url| ContentBlock::Image {
                        url: url.into(),
                        media_type: None,
                    }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn anthropic_content(value: Option<&Value>) -> Vec<ContentBlock> {
    match value {
        Some(Value::String(text)) => vec![ContentBlock::Text { text: text.clone() }],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| ContentBlock::Text { text: text.into() }),
                Some("thinking") => block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .map(|text| ContentBlock::Reasoning { text: text.into() }),
                Some("tool_use") => Some(ContentBlock::ToolUse {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool-call")
                        .into(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .into(),
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                }),
                Some("tool_result") => Some(ContentBlock::ToolResult {
                    tool_use_id: block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool-call")
                        .into(),
                    content: block.get("content").cloned().unwrap_or(Value::Null),
                }),
                Some("image") => {
                    block
                        .pointer("/source/data")
                        .and_then(Value::as_str)
                        .map(|data| ContentBlock::Image {
                            url: format!(
                                "data:{};base64,{}",
                                block
                                    .pointer("/source/media_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("image/jpeg"),
                                data
                            ),
                            media_type: block
                                .pointer("/source/media_type")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn gemini_parts(value: Option<&Value>) -> Vec<ContentBlock> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                return Some(
                    if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        ContentBlock::Reasoning { text: text.into() }
                    } else {
                        ContentBlock::Text { text: text.into() }
                    },
                );
            }
            if let Some(call) = part.get("functionCall") {
                return Some(ContentBlock::ToolUse {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool-call")
                        .into(),
                    name: call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .into(),
                    input: call.get("args").cloned().unwrap_or(Value::Null),
                });
            }
            if let Some(response) = part.get("functionResponse") {
                return Some(ContentBlock::ToolResult {
                    tool_use_id: response
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool-call")
                        .into(),
                    content: response.get("response").cloned().unwrap_or(Value::Null),
                });
            }
            part.get("inlineData").and_then(|data| {
                data.get("data")
                    .and_then(Value::as_str)
                    .map(|raw| ContentBlock::Image {
                        url: format!(
                            "data:{};base64,{raw}",
                            data.get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or("image/jpeg")
                        ),
                        media_type: data
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
            })
        })
        .collect()
}

fn openai_message(message: &CanonicalMessage) -> Value {
    let visible = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(json!({"type":"text","text":text})),
            ContentBlock::Image { url, .. } => {
                Some(json!({"type":"image_url","image_url":{"url":url}}))
            }
            ContentBlock::Reasoning { text } => Some(json!({"type":"text","text":text})),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut root = json!({"role":message.role,"content":if visible.len()==1 && visible[0]["type"]=="text" { visible[0]["text"].clone() } else { Value::Array(visible) }});
    let calls = message.content.iter().filter_map(|block| if let ContentBlock::ToolUse{id,name,input}=block {Some(json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}}))} else {None}).collect::<Vec<_>>();
    if !calls.is_empty() {
        root["tool_calls"] = Value::Array(calls);
    }
    if let Some(ContentBlock::ToolResult {
        tool_use_id,
        content,
    }) = message
        .content
        .iter()
        .find(|block| matches!(block, ContentBlock::ToolResult { .. }))
    {
        root["role"] = "tool".into();
        root["tool_call_id"] = tool_use_id.clone().into();
        root["content"] = if content.is_string() {
            content.clone()
        } else {
            Value::String(content.to_string())
        };
    }
    root
}

fn openai_responses_message(message: &CanonicalMessage) -> Vec<Value> {
    let mut items = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::ToolResult { tool_use_id, content } => items.push(json!({"type":"function_call_output","call_id":tool_use_id,"output":if content.is_string(){content.clone()}else{Value::String(content.to_string())}})),
            ContentBlock::ToolUse { id, name, input } => items.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":input.to_string()})),
            _ => {}
        }
    }
    let content = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                Some(json!({"type":"input_text","text":text}))
            }
            ContentBlock::Image { url, .. } => Some(json!({"type":"input_image","image_url":url})),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !content.is_empty() {
        items.insert(0, json!({"role":message.role,"content":content}));
    }
    items
}

fn anthropic_message(message: &CanonicalMessage) -> Value {
    json!({"role":if message.role=="assistant"{"assistant"}else{"user"},"content":message.content.iter().filter_map(anthropic_block).collect::<Vec<_>>()})
}
fn gemini_message(message: &CanonicalMessage) -> Value {
    json!({"role":if message.role=="assistant"{"model"}else{"user"},"parts":message.content.iter().filter_map(gemini_block).collect::<Vec<_>>()})
}
fn anthropic_block(block: &ContentBlock) -> Option<Value> {
    match block { ContentBlock::Text{text}=>Some(json!({"type":"text","text":text})), ContentBlock::Reasoning{text}=>Some(json!({"type":"thinking","thinking":text})), ContentBlock::ToolUse{id,name,input}=>Some(json!({"type":"tool_use","id":id,"name":name,"input":input})), ContentBlock::ToolResult{tool_use_id,content}=>Some(json!({"type":"tool_result","tool_use_id":tool_use_id,"content":content})), ContentBlock::Image{url,..}=>data_url(url).map(|(media,data)|json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}})) }
}
fn gemini_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({"text":text})),
        ContentBlock::Reasoning { text } => Some(json!({"text":text,"thought":true})),
        ContentBlock::ToolUse { id, name, input } => {
            Some(json!({"functionCall":{"id":id,"name":name,"args":input}}))
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
        } => Some(json!({"functionResponse":{"id":tool_use_id,"response":content}})),
        ContentBlock::Image { url, .. } => {
            data_url(url).map(|(media, data)| json!({"inlineData":{"mimeType":media,"data":data}}))
        }
    }
}
fn openai_assistant(response: &CanonicalResponse) -> Value {
    let text = response
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");
    let reasoning = response
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Reasoning { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");
    let mut value = json!({"role":"assistant","content":text});
    if !reasoning.is_empty() {
        value["reasoning_content"] = Value::String(reasoning);
    }
    let calls=response.content.iter().filter_map(|b|if let ContentBlock::ToolUse{id,name,input}=b{Some(json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}}))}else{None}).collect::<Vec<_>>();
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    value
}
fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
fn parse_json_or_string(value: Value) -> Value {
    value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(value)
}
fn data_url(value: &str) -> Option<(&str, &str)> {
    value.strip_prefix("data:")?.split_once(";base64,")
}
fn openai_stop(stop: Option<&str>) -> &str {
    match stop {
        Some("tool_use" | "tool_calls") => "tool_calls",
        Some("max_tokens" | "length") => "length",
        _ => "stop",
    }
}
fn anthropic_stop(stop: Option<&str>) -> &str {
    match stop {
        Some("tool_calls" | "tool_use") => "tool_use",
        Some("length" | "max_tokens") => "max_tokens",
        _ => "end_turn",
    }
}
fn gemini_stop(stop: Option<&str>) -> &str {
    match stop {
        Some("length" | "max_tokens") => "MAX_TOKENS",
        _ => "STOP",
    }
}

#[derive(Default)]
struct StreamDelta {
    text: Option<String>,
    reasoning: Option<String>,
    tool_id: Option<String>,
    tool_name: Option<String>,
    tool_arguments: Option<String>,
    tool_index: usize,
    done: bool,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

pub struct StreamTranslator {
    upstream: WireProtocol,
    public: PublicProtocol,
    model: String,
    buffer: Vec<u8>,
    started: bool,
    tool_started: bool,
    tool_blocks: HashMap<usize, usize>,
    next_block: usize,
    text_block: Option<usize>,
    reasoning_block: Option<usize>,
    responses_text_started: bool,
    responses_reasoning_started: bool,
    response_text: String,
    response_reasoning: String,
    response_tools: HashMap<usize, (String, String, String)>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    completed: bool,
    id: String,
}

impl StreamTranslator {
    pub fn new(upstream: WireProtocol, public: PublicProtocol, model: &str) -> Self {
        Self {
            upstream,
            public,
            model: model.into(),
            buffer: Vec::new(),
            started: false,
            tool_started: false,
            tool_blocks: HashMap::new(),
            next_block: 0,
            text_block: None,
            reasoning_block: None,
            responses_text_started: false,
            responses_reasoning_started: false,
            response_text: String::new(),
            response_reasoning: String::new(),
            response_tools: HashMap::new(),
            input_tokens: None,
            output_tokens: None,
            completed: false,
            id: format!("lar-{}", uuid::Uuid::new_v4()),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(end) = find_event_end(&self.buffer) {
            let event = self.buffer.drain(..end).collect::<Vec<_>>();
            let separator = if self.buffer.starts_with(b"\r\n\r\n") {
                4
            } else {
                2
            };
            self.buffer.drain(..separator.min(self.buffer.len()));
            let payload = String::from_utf8_lossy(&event)
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim))
                .collect::<Vec<_>>()
                .join("\n");
            if payload.is_empty() {
                continue;
            }
            let deltas = if payload == "[DONE]" {
                vec![StreamDelta {
                    done: true,
                    ..Default::default()
                }]
            } else {
                serde_json::from_str::<Value>(&payload)
                    .ok()
                    .map(|value| decode_stream_deltas(self.upstream, &value))
                    .unwrap_or_default()
            };
            for delta in deltas {
                output.extend_from_slice(&self.encode_delta(delta));
            }
        }
        output
    }

    fn encode_delta(&mut self, mut delta: StreamDelta) -> Vec<u8> {
        if delta.input_tokens.is_some() {
            self.input_tokens = delta.input_tokens;
        }
        if delta.output_tokens.is_some() {
            self.output_tokens = delta.output_tokens;
        }
        if delta.done {
            delta.input_tokens = self.input_tokens;
            delta.output_tokens = self.output_tokens;
        }
        if delta.done && self.completed {
            return Vec::new();
        }
        if delta.done {
            self.completed = true;
        }
        let mut events = String::new();
        match self.public {
            PublicProtocol::OpenAiChat => {
                if !self.started {
                    events.push_str(&format!("data: {}\n\n", json!({"id":self.id,"object":"chat.completion.chunk","model":self.model,"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]})));
                    self.started = true;
                }
                let mut content = Map::new();
                if let Some(text) = delta.text {
                    self.response_text.push_str(&text);
                    content.insert("content".into(), text.into());
                }
                if let Some(reasoning) = delta.reasoning {
                    self.response_reasoning.push_str(&reasoning);
                    content.insert("reasoning_content".into(), reasoning.into());
                }
                if delta.tool_name.is_some() || delta.tool_arguments.is_some() {
                    self.tool_started = true;
                    content.insert("tool_calls".into(),json!([{"index":delta.tool_index,"id":delta.tool_id,"type":"function","function":{"name":delta.tool_name,"arguments":delta.tool_arguments.unwrap_or_default()}}]));
                }
                if !content.is_empty() {
                    events.push_str(&format!("data: {}\n\n",json!({"id":self.id,"object":"chat.completion.chunk","model":self.model,"choices":[{"index":0,"delta":content,"finish_reason":null}]})));
                }
                if delta.done {
                    events.push_str(&format!("data: {}\n\n",json!({"id":self.id,"object":"chat.completion.chunk","model":self.model,"choices":[{"index":0,"delta":{},"finish_reason":if self.tool_started{"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":delta.input_tokens,"completion_tokens":delta.output_tokens}})));
                    events.push_str("data: [DONE]\n\n");
                }
            }
            PublicProtocol::OpenAiResponses => {
                if !self.started {
                    events.push_str(&format!("event: response.created\ndata: {}\n\n",json!({"type":"response.created","response":{"id":self.id,"object":"response","status":"in_progress","model":self.model,"output":[]}})));
                    self.started = true;
                }
                if let Some(text) = delta.text {
                    self.response_text.push_str(&text);
                    if !self.responses_text_started {
                        events.push_str(&format!("event: response.output_item.added\ndata: {}\n\n",json!({"type":"response.output_item.added","output_index":0,"item":{"id":"message","type":"message","role":"assistant","status":"in_progress","content":[]}})));
                        events.push_str(&format!("event: response.content_part.added\ndata: {}\n\n",json!({"type":"response.content_part.added","item_id":"message","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})));
                        self.responses_text_started = true;
                    }
                    events.push_str(&format!("event: response.output_text.delta\ndata: {}\n\n",json!({"type":"response.output_text.delta","item_id":"message","output_index":0,"content_index":0,"delta":text})));
                }
                if let Some(reasoning) = delta.reasoning {
                    self.response_reasoning.push_str(&reasoning);
                    if !self.responses_reasoning_started {
                        events.push_str(&format!("event: response.output_item.added\ndata: {}\n\n",json!({"type":"response.output_item.added","output_index":1,"item":{"id":"reasoning","type":"reasoning","status":"in_progress","summary":[]}})));
                        self.responses_reasoning_started = true;
                    }
                    events.push_str(&format!("event: response.reasoning_summary_text.delta\ndata: {}\n\n",json!({"type":"response.reasoning_summary_text.delta","item_id":"reasoning","output_index":1,"summary_index":0,"delta":reasoning})));
                }
                if let Some(name) = delta.tool_name.clone() {
                    let output_index = delta.tool_index + 2;
                    let id = delta
                        .tool_id
                        .clone()
                        .unwrap_or_else(|| format!("tool-{}", delta.tool_index));
                    self.response_tools
                        .entry(output_index)
                        .or_insert_with(|| (id.clone(), name.clone(), String::new()));
                    events.push_str(&format!("event: response.output_item.added\ndata: {}\n\n",json!({"type":"response.output_item.added","output_index":output_index,"item":{"id":id,"type":"function_call","call_id":id,"name":name,"arguments":"","status":"in_progress"}})));
                }
                if let Some(arguments) = delta.tool_arguments {
                    let output_index = delta.tool_index + 2;
                    let entry = self.response_tools.entry(output_index).or_insert_with(|| {
                        (
                            delta
                                .tool_id
                                .clone()
                                .unwrap_or_else(|| format!("tool-{}", delta.tool_index)),
                            delta.tool_name.clone().unwrap_or_else(|| "tool".into()),
                            String::new(),
                        )
                    });
                    entry.2.push_str(&arguments);
                    events.push_str(&format!("event: response.function_call_arguments.delta\ndata: {}\n\n",json!({"type":"response.function_call_arguments.delta","item_id":entry.0,"output_index":output_index,"delta":arguments})));
                }
                if delta.done {
                    if self.responses_text_started {
                        events.push_str(&format!("event: response.output_text.done\ndata: {}\n\n",json!({"type":"response.output_text.done","item_id":"message","output_index":0,"content_index":0,"text":self.response_text})));
                        events.push_str(&format!("event: response.content_part.done\ndata: {}\n\n",json!({"type":"response.content_part.done","item_id":"message","output_index":0,"content_index":0,"part":{"type":"output_text","text":self.response_text,"annotations":[]}})));
                        events.push_str(&format!("event: response.output_item.done\ndata: {}\n\n",json!({"type":"response.output_item.done","output_index":0,"item":{"id":"message","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":self.response_text,"annotations":[]}]}})));
                    }
                    if self.responses_reasoning_started {
                        events.push_str(&format!("event: response.output_item.done\ndata: {}\n\n",json!({"type":"response.output_item.done","output_index":1,"item":{"id":"reasoning","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":self.response_reasoning}]}})));
                    }
                    for (index, (id, name, arguments)) in &self.response_tools {
                        events.push_str(&format!("event: response.function_call_arguments.done\ndata: {}\n\n",json!({"type":"response.function_call_arguments.done","item_id":id,"output_index":index,"arguments":arguments})));
                        events.push_str(&format!("event: response.output_item.done\ndata: {}\n\n",json!({"type":"response.output_item.done","output_index":index,"item":{"id":id,"type":"function_call","call_id":id,"name":name,"arguments":arguments,"status":"completed"}})));
                    }
                    let mut output = Vec::new();
                    if self.responses_text_started {
                        output.push(json!({"id":"message","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":self.response_text,"annotations":[]}]}));
                    }
                    if self.responses_reasoning_started {
                        output.push(json!({"id":"reasoning","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":self.response_reasoning}]}));
                    }
                    for (id, name, arguments) in self.response_tools.values() {
                        output.push(json!({"id":id,"type":"function_call","call_id":id,"name":name,"arguments":arguments,"status":"completed"}));
                    }
                    events.push_str(&format!("event: response.completed\ndata: {}\n\n",json!({"type":"response.completed","response":{"id":self.id,"object":"response","status":"completed","model":self.model,"output":output,"usage":{"input_tokens":delta.input_tokens,"output_tokens":delta.output_tokens}}})));
                }
            }
            PublicProtocol::Anthropic => {
                if !self.started {
                    events.push_str(&format!("event: message_start\ndata: {}\n\n",json!({"type":"message_start","message":{"id":self.id,"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"usage":{"input_tokens":delta.input_tokens.unwrap_or(0),"output_tokens":0}}})));
                    self.started = true;
                }
                if let Some(text) = delta.text {
                    let index=*self.text_block.get_or_insert_with(||{let index=self.next_block;self.next_block+=1;events.push_str(&format!("event: content_block_start\ndata: {}\n\n",json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}})));index});
                    events.push_str(&format!("event: content_block_delta\ndata: {}\n\n",json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}})));
                }
                if let Some(reasoning) = delta.reasoning {
                    let index=*self.reasoning_block.get_or_insert_with(||{let index=self.next_block;self.next_block+=1;events.push_str(&format!("event: content_block_start\ndata: {}\n\n",json!({"type":"content_block_start","index":index,"content_block":{"type":"thinking","thinking":"","signature":""}})));index});
                    events.push_str(&format!("event: content_block_delta\ndata: {}\n\n",json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":reasoning}})));
                }
                if delta.tool_name.is_some() || delta.tool_arguments.is_some() {
                    self.tool_started = true;
                    let index = *self.tool_blocks.entry(delta.tool_index).or_insert_with(|| {
                        let index = self.next_block;
                        self.next_block += 1;
                        index
                    });
                    if let Some(name) = delta.tool_name {
                        events.push_str(&format!("event: content_block_start\ndata: {}\n\n",json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":delta.tool_id.unwrap_or_else(||"tool-call".into()),"name":name,"input":{}}})));
                    }
                    if let Some(arguments) = delta.tool_arguments {
                        events.push_str(&format!("event: content_block_delta\ndata: {}\n\n",json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":arguments}})));
                    }
                }
                if delta.done {
                    for index in 0..self.next_block {
                        events.push_str(&format!(
                            "event: content_block_stop\ndata: {}\n\n",
                            json!({"type":"content_block_stop","index":index})
                        ));
                    }
                    events.push_str(&format!("event: message_delta\ndata: {}\n\n",json!({"type":"message_delta","delta":{"stop_reason":if self.tool_started{"tool_use"}else{"end_turn"},"stop_sequence":null},"usage":{"output_tokens":delta.output_tokens.unwrap_or(0)}})));
                    events.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
                }
            }
            PublicProtocol::Gemini => {
                if let Some(text) = delta.text {
                    events.push_str(&format!("data: {}\n\n",json!({"responseId":self.id,"candidates":[{"content":{"role":"model","parts":[{"text":text}]}}]})));
                }
                if let Some(reasoning) = delta.reasoning {
                    events.push_str(&format!("data: {}\n\n",json!({"responseId":self.id,"candidates":[{"content":{"role":"model","parts":[{"text":reasoning,"thought":true}]}}]})));
                }
                if delta.tool_name.is_some() {
                    events.push_str(&format!("data: {}\n\n",json!({"responseId":self.id,"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"id":delta.tool_id,"name":delta.tool_name,"args":parse_json_or_string(Value::String(delta.tool_arguments.unwrap_or_else(||"{}".into())))}}]}}]})));
                }
                if delta.done {
                    events.push_str(&format!("data: {}\n\n",json!({"responseId":self.id,"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":delta.input_tokens,"candidatesTokenCount":delta.output_tokens}})));
                }
            }
        }
        events.into_bytes()
    }
}

fn find_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| buffer.windows(4).position(|window| window == b"\r\n\r\n"))
}

fn decode_stream_deltas(protocol: WireProtocol, value: &Value) -> Vec<StreamDelta> {
    match protocol {
        WireProtocol::OpenAiChat => {
            let delta = &value["choices"][0]["delta"];
            let mut result = vec![StreamDelta {
                text: delta
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                reasoning: delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                done: false,
                input_tokens: value
                    .pointer("/usage/prompt_tokens")
                    .and_then(Value::as_u64),
                output_tokens: value
                    .pointer("/usage/completion_tokens")
                    .and_then(Value::as_u64),
                ..Default::default()
            }];
            for tool in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                result.push(StreamDelta {
                    tool_index: tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    tool_id: tool.get("id").and_then(Value::as_str).map(str::to_owned),
                    tool_name: tool
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    tool_arguments: tool
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    ..Default::default()
                });
            }
            result
        }
        WireProtocol::OpenAiResponses => {
            let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
            let item = value.get("item");
            vec![StreamDelta {
                text: (kind == "response.output_text.delta")
                    .then(|| {
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten(),
                reasoning: (kind.contains("reasoning") && kind.ends_with("delta"))
                    .then(|| {
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten(),
                tool_id: value
                    .get("item_id")
                    .or_else(|| item.and_then(|v| v.get("call_id")))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_name: item
                    .and_then(|v| v.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_arguments: (kind == "response.function_call_arguments.delta")
                    .then(|| {
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten(),
                tool_index: value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                done: kind == "response.completed",
                input_tokens: value
                    .pointer("/response/usage/input_tokens")
                    .and_then(Value::as_u64),
                output_tokens: value
                    .pointer("/response/usage/output_tokens")
                    .and_then(Value::as_u64),
            }]
        }
        WireProtocol::AnthropicMessages => {
            let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
            vec![StreamDelta {
                text: value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                reasoning: value
                    .pointer("/delta/thinking")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_id: value
                    .pointer("/content_block/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_name: value
                    .pointer("/content_block/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_arguments: value
                    .pointer("/delta/partial_json")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_index: value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                done: kind == "message_stop",
                input_tokens: value
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64),
                output_tokens: value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64),
            }]
        }
        WireProtocol::GeminiGenerateContent => {
            let part = value.pointer("/candidates/0/content/parts/0");
            vec![StreamDelta {
                text: part
                    .filter(|v| !v.get("thought").and_then(Value::as_bool).unwrap_or(false))
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                reasoning: part
                    .filter(|v| v.get("thought").and_then(Value::as_bool).unwrap_or(false))
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_id: part
                    .and_then(|v| v.pointer("/functionCall/id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_name: part
                    .and_then(|v| v.pointer("/functionCall/name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tool_arguments: part
                    .and_then(|v| v.pointer("/functionCall/args"))
                    .map(Value::to_string),
                done: value.pointer("/candidates/0/finishReason").is_some(),
                input_tokens: value
                    .pointer("/usageMetadata/promptTokenCount")
                    .and_then(Value::as_u64),
                output_tokens: value
                    .pointer("/usageMetadata/candidatesTokenCount")
                    .and_then(Value::as_u64),
                ..Default::default()
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_request_translates_to_openai_chat_with_tools() {
        let request = json!({
            "model": "assistant", "system": "Be concise", "max_tokens": 200,
            "messages": [{"role":"user","content":[{"type":"text","text":"weather"}]}],
            "tools": [{"name":"forecast","description":"Weather","input_schema":{"type":"object"}}]
        });
        let canonical = decode_request(PublicProtocol::Anthropic, &request, None).unwrap();
        let upstream =
            encode_request(WireProtocol::OpenAiChat, &canonical, "provider-model").unwrap();

        assert_eq!(upstream["model"], "provider-model");
        assert_eq!(upstream["messages"][0]["role"], "system");
        assert_eq!(upstream["tools"][0]["function"]["name"], "forecast");
        assert_eq!(upstream["max_tokens"], 200);
    }

    #[test]
    fn openai_response_translates_to_anthropic_and_gemini() {
        let upstream = json!({
            "id":"chat-1","model":"m","choices":[{"message":{"role":"assistant","content":"hello","tool_calls":[{"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}}]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":3,"completion_tokens":4}
        });
        let response = decode_response(WireProtocol::OpenAiChat, &upstream).unwrap();
        let anthropic = encode_response(PublicProtocol::Anthropic, &response, "assistant");
        let gemini = encode_response(PublicProtocol::Gemini, &response, "assistant");

        assert_eq!(anthropic["content"][0]["text"], "hello");
        assert_eq!(anthropic["content"][1]["type"], "tool_use");
        assert_eq!(
            gemini["candidates"][0]["content"]["parts"][1]["functionCall"]["name"],
            "lookup"
        );
        assert_eq!(gemini["usageMetadata"]["promptTokenCount"], 3);
    }

    #[test]
    fn split_openai_sse_translates_to_anthropic_events() {
        let mut translator = StreamTranslator::new(
            WireProtocol::OpenAiChat,
            PublicProtocol::Anthropic,
            "assistant",
        );
        assert!(translator
            .push(br#"data: {"choices":[{"delta":{"content":"hel"#)
            .is_empty());
        let output = translator.push(b"lo\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n");
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("message_start"));
        assert!(text.contains("text_delta"));
        assert!(text.contains("hello"));
        assert!(text.contains("message_stop"));
    }

    #[test]
    fn responses_stream_contains_item_and_content_lifecycle() {
        let mut translator = StreamTranslator::new(
            WireProtocol::OpenAiChat,
            PublicProtocol::OpenAiResponses,
            "assistant",
        );
        let output=translator.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n");
        let text = String::from_utf8(output).unwrap();
        for event in [
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ] {
            assert!(text.contains(event), "missing {event}");
        }
    }

    #[test]
    fn anthropic_reasoning_uses_a_thinking_block() {
        let mut translator = StreamTranslator::new(
            WireProtocol::OpenAiChat,
            PublicProtocol::Anthropic,
            "assistant",
        );
        let output=translator.push(b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n");
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"type\":\"thinking\""));
        assert!(text.contains("thinking_delta"));
        assert!(!text.contains("\"type\":\"text\",\"text\":\"\""));
    }

    #[test]
    fn gemini_vision_tools_and_json_schema_translate_to_openai() {
        let input = json!({
            "contents":[{"role":"user","parts":[{"text":"inspect"},{"inlineData":{"mimeType":"image/png","data":"AAAA"}}]}],
            "tools":[{"functionDeclarations":[{"name":"lookup","parameters":{"type":"object"}}]}],
            "generationConfig":{"maxOutputTokens":42,"topP":0.7,"stopSequences":["done"],"responseMimeType":"application/json","responseSchema":{"type":"object"}}
        });
        let canonical = decode_request(PublicProtocol::Gemini, &input, Some("assistant")).unwrap();
        let openai = encode_request(WireProtocol::OpenAiChat, &canonical, "gpt").unwrap();
        assert_eq!(
            openai["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(openai["tools"][0]["function"]["name"], "lookup");
        assert_eq!(openai["max_tokens"], 42);
        assert_eq!(openai["stop"][0], "done");
        assert_eq!(openai["response_format"]["type"], "json_schema");
    }

    #[test]
    fn cross_protocol_validation_rejects_untranslated_fields() {
        let request = json!({"model":"m","messages":[],"vendor_secret_option":true});
        assert!(
            validate_cross_protocol(PublicProtocol::OpenAiChat, &request)
                .unwrap_err()
                .to_string()
                .contains("vendor_secret_option")
        );
    }

    #[test]
    fn responses_and_chat_json_schema_shapes_are_normalized() {
        let responses = json!({"model":"m","input":"hello","text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"},"strict":true}}});
        let canonical = decode_request(PublicProtocol::OpenAiResponses, &responses, None).unwrap();
        let chat = encode_request(WireProtocol::OpenAiChat, &canonical, "m").unwrap();
        assert_eq!(chat["response_format"]["json_schema"]["name"], "answer");
        let back = encode_request(WireProtocol::OpenAiResponses, &canonical, "m").unwrap();
        assert_eq!(back["text"]["format"]["name"], "answer");
        assert!(back["text"]["format"].get("json_schema").is_none());
    }

    #[test]
    fn nested_unknown_content_is_rejected_for_cross_protocol_requests() {
        let anthropic = json!({"model":"m","messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"https://example.com/a.png"}}]}]});
        assert!(validate_cross_protocol(PublicProtocol::Anthropic, &anthropic).is_err());
        let gemini = json!({"contents":[{"parts":[{"fileData":{"fileUri":"gs://private"}}]}]});
        assert!(validate_cross_protocol(PublicProtocol::Gemini, &gemini).is_err());
    }
}
