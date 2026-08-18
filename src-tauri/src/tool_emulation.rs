use serde_json::{json, Value};

use crate::{
    domain::TargetKind,
    protocol::{CanonicalRequest, CanonicalResponse, CanonicalTool, ContentBlock},
    storage::ModelTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEmulation {
    None,
    MlxInject,
    GgufSalvage,
}

pub fn force_tool_support(target: &ModelTarget) -> bool {
    target
        .local
        .force_tool_support
        .unwrap_or(target.kind == TargetKind::Mlx)
}

pub fn tool_emulation_for(target: &ModelTarget, has_tools: bool) -> ToolEmulation {
    if !has_tools || !force_tool_support(target) {
        return ToolEmulation::None;
    }
    match target.kind {
        TargetKind::Mlx => ToolEmulation::MlxInject,
        TargetKind::Gguf => ToolEmulation::GgufSalvage,
        TargetKind::Cloud | TargetKind::Alias => ToolEmulation::None,
    }
}

pub fn prepare_mlx_request(request: &mut CanonicalRequest) {
    flatten_tool_history(request);
    if !request.tools.is_empty() {
        request.system.push(ContentBlock::Text {
            text: tool_system_prompt(&request.tools),
        });
    }
    request.tools.clear();
    request.tool_choice = None;
    request.parallel_tool_calls = None;
    request.response_format = None;
    request.stream = false;
}

pub fn strip_unsupported_mlx_fields(payload: &mut Value) {
    if let Some(root) = payload.as_object_mut() {
        root.remove("tools");
        root.remove("tool_choice");
        root.remove("parallel_tool_calls");
        root.remove("response_format");
        root.insert("stream".into(), Value::Bool(false));
    }
}

pub fn salvage_tool_calls(response: &mut CanonicalResponse, tools: &[CanonicalTool]) {
    if tools.is_empty()
        || response
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    {
        return;
    }
    let allowed: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    let mut leftover = String::new();
    let mut calls = Vec::new();
    for block in &response.content {
        match block {
            ContentBlock::Text { text } => leftover.push_str(&extract_calls(text, &allowed, &mut calls)),
            other => match other {
                ContentBlock::Reasoning { text } => leftover.push_str(text),
                _ => {}
            },
        }
    }
    if calls.is_empty() {
        return;
    }
    let mut content = Vec::new();
    let trimmed = leftover.trim();
    if !trimmed.is_empty() {
        content.push(ContentBlock::Text {
            text: trimmed.into(),
        });
    }
    content.extend(calls);
    response.content = content;
    response.stop_reason = Some("tool_calls".into());
}

fn tool_system_prompt(tools: &[CanonicalTool]) -> String {
    let mut prompt = String::from(
        "You can call tools by emitting one or more XML tags and no other text:\n\
         <tool_call>{\"name\":\"TOOL_NAME\",\"arguments\":{}}</tool_call>\n\
         Available tools:\n",
    );
    for tool in tools {
        prompt.push_str("- ");
        prompt.push_str(&tool.name);
        if let Some(description) = &tool.description {
            prompt.push_str(": ");
            prompt.push_str(description);
        }
        prompt.push('\n');
        prompt.push_str("  parameters: ");
        prompt.push_str(&tool.input_schema.to_string());
        prompt.push('\n');
    }
    prompt
}

fn flatten_tool_history(request: &mut CanonicalRequest) {
    for message in &mut request.messages {
        let mut next = Vec::new();
        for block in std::mem::take(&mut message.content) {
            match block {
                ContentBlock::ToolUse { name, input, .. } => next.push(ContentBlock::Text {
                    text: format!(
                        "<tool_call>{}</tool_call>",
                        json!({"name": name, "arguments": input})
                    ),
                }),
                ContentBlock::ToolResult { content, .. } => next.push(ContentBlock::Text {
                    text: format!(
                        "<tool_result>{}</tool_result>",
                        if content.is_string() {
                            content.as_str().unwrap_or_default().to_string()
                        } else {
                            content.to_string()
                        }
                    ),
                }),
                other => next.push(other),
            }
        }
        message.content = next;
    }
}

fn extract_calls(text: &str, allowed: &[&str], calls: &mut Vec<ContentBlock>) -> String {
    let mut leftover = String::new();
    let mut rest = text;
    loop {
        let tagged = next_tag_span(rest);
        let fenced = next_fence_span(rest);
        let (start, end, inner, kind) = match (tagged, fenced) {
            (Some(tag), Some(fence)) if fence.0 < tag.0 => (fence.0, fence.1, fence.2, "fence"),
            (Some(tag), _) => (tag.0, tag.1, tag.2, "tag"),
            (None, Some(fence)) => (fence.0, fence.1, fence.2, "fence"),
            (None, None) => {
                leftover.push_str(rest);
                break;
            }
        };
        leftover.push_str(&rest[..start]);
        if let Some(call) = parse_tool_payload(inner, allowed) {
            calls.push(call);
        } else if kind == "fence" {
            leftover.push_str(&rest[start..end]);
        }
        rest = &rest[end..];
    }
    leftover
}

fn next_tag_span(text: &str) -> Option<(usize, usize, &str)> {
    const TAGS: [(&str, &str); 2] = [
        ("<tool_call>", "</tool_call>"),
        ("<function_call>", "</function_call>"),
    ];
    let mut best: Option<(usize, usize, &str)> = None;
    for (open, close) in TAGS {
        let Some(start) = text.find(open) else {
            continue;
        };
        let inner_start = start + open.len();
        let Some(inner_end) = text[inner_start..].find(close) else {
            continue;
        };
        let end = inner_start + inner_end + close.len();
        let inner = text[inner_start..inner_start + inner_end].trim();
        if best.map(|(best_start, _, _)| start < best_start).unwrap_or(true) {
            best = Some((start, end, inner));
        }
    }
    best
}

fn next_fence_span(text: &str) -> Option<(usize, usize, &str)> {
    let start = text.find("```")?;
    let after = &text[start + 3..];
    let newline = after.find('\n')?;
    let lang = after[..newline].trim();
    if !lang.is_empty() && !lang.eq_ignore_ascii_case("json") {
        return None;
    }
    let body_start = start + 3 + newline + 1;
    let close = text[body_start..].find("```")?;
    let end = body_start + close + 3;
    Some((start, end, text[body_start..body_start + close].trim()))
}

fn parse_tool_payload(payload: &str, allowed: &[&str]) -> Option<ContentBlock> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
    let name = value
        .get("name")
        .or_else(|| value.pointer("/function/name"))
        .and_then(Value::as_str)?;
    if !allowed.iter().any(|item| *item == name) {
        return None;
    }
    let input = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    let input = match input {
        Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        other => other,
    };
    Some(ContentBlock::ToolUse {
        id: format!("call_{}", &uuid::Uuid::new_v4().to_string()[..8]),
        name: name.into(),
        input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> CanonicalTool {
        CanonicalTool {
            name: name.into(),
            description: Some("lookup".into()),
            input_schema: json!({"type":"object"}),
        }
    }

    fn text_response(text: &str) -> CanonicalResponse {
        CanonicalResponse {
            id: "r".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: Some("stop".into()),
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn mlx_defaults_force_on_and_gguf_defaults_off() {
        let mlx = ModelTarget {
            id: "m".into(),
            provider_id: None,
            name: "mlx".into(),
            kind: TargetKind::Mlx,
            provider_model: "m".into(),
            local_path: None,
            runtime_url: None,
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: Default::default(),
        };
        let mut gguf = mlx.clone();
        gguf.kind = TargetKind::Gguf;
        assert_eq!(tool_emulation_for(&mlx, true), ToolEmulation::MlxInject);
        assert_eq!(tool_emulation_for(&gguf, true), ToolEmulation::None);
        gguf.local.force_tool_support = Some(true);
        assert_eq!(tool_emulation_for(&gguf, true), ToolEmulation::GgufSalvage);
        let mut mlx_off = mlx;
        mlx_off.local.force_tool_support = Some(false);
        assert_eq!(tool_emulation_for(&mlx_off, true), ToolEmulation::None);
    }

    #[test]
    fn salvage_parses_hermes_tags_and_named_json_fences() {
        let tools = vec![tool("terminal")];
        let mut tagged = text_response(
            r#"<tool_call>{"name":"terminal","arguments":{"cmd":"ls"}}</tool_call>"#,
        );
        salvage_tool_calls(&mut tagged, &tools);
        assert!(matches!(
            &tagged.content[0],
            ContentBlock::ToolUse { name, .. } if name == "terminal"
        ));
        assert_eq!(tagged.stop_reason.as_deref(), Some("tool_calls"));

        let mut fence = text_response("```json\n{\"name\":\"terminal\",\"arguments\":{\"cmd\":\"pwd\"}}\n```");
        salvage_tool_calls(&mut fence, &tools);
        assert!(matches!(
            &fence.content[0],
            ContentBlock::ToolUse { name, .. } if name == "terminal"
        ));

        let mut ignored = text_response("```json\n{\"name\":\"not_a_tool\",\"arguments\":{}}\n```");
        salvage_tool_calls(&mut ignored, &tools);
        assert!(matches!(ignored.content[0], ContentBlock::Text { .. }));
    }

    #[test]
    fn mlx_prepare_injects_tools_and_flattens_history() {
        let mut request = CanonicalRequest {
            system: Vec::new(),
            messages: vec![crate::protocol::CanonicalMessage {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "terminal".into(),
                    input: json!({"cmd":"ls"}),
                }],
            }],
            tools: vec![tool("terminal")],
            tool_choice: Some(json!("auto")),
            parallel_tool_calls: Some(true),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            reasoning: None,
            response_format: Some(json!({"type":"json_object"})),
            stream: true,
        };
        prepare_mlx_request(&mut request);
        assert!(request.tools.is_empty());
        assert!(request.tool_choice.is_none());
        assert!(!request.stream);
        assert!(matches!(
            &request.system[0],
            ContentBlock::Text { text } if text.contains("terminal")
        ));
        assert!(matches!(
            &request.messages[0].content[0],
            ContentBlock::Text { text } if text.contains("<tool_call>")
        ));
    }
}
