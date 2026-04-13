//! Anthropic Messages API wire implementation using the [async-anthropic](https://crates.io/crates/async-anthropic) SDK.
//!
//! Converts Codex `Prompt` into SDK request types, streams via the SDK, and maps
//! stream events to the shared `ResponseEvent` type so the rest of the stack is unchanged.

use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::error::CodexErr;
use crate::error::Result;
use crate::model_provider_info::ModelProviderInfo;
use async_anthropic::Client;
use async_anthropic::types::ContentBlockDelta;
use async_anthropic::types::CreateMessagesRequestBuilder;
use async_anthropic::types::Message;
use async_anthropic::types::MessageContent;
use async_anthropic::types::MessageContentList;
use async_anthropic::types::MessageRole;
use async_anthropic::types::MessagesStreamEvent;
use async_anthropic::types::Text;
use async_anthropic::types::ToolResult;
use async_anthropic::types::ToolUse;
use codex_api::common::ResponseEvent;
use codex_otel::SessionTelemetry;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::TokenUsage;
use futures::StreamExt;
use secrecy::SecretString;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::trace;
use tracing::warn;

use codex_tools::create_tools_json_for_responses_api;

const DEFAULT_MAX_TOKENS: i32 = 4096;

/// Streams a turn via the Anthropic Messages API using the async-anthropic SDK.
#[allow(clippy::too_many_arguments)]
pub async fn stream_anthropic_messages(
    provider: &ModelProviderInfo,
    base_url: &str,
    auth_token: Option<String>,
    prompt: &Prompt,
    model_info: &ModelInfo,
    _otel_manager: SessionTelemetry,
    _turn_metadata_header: Option<&str>,
) -> Result<ResponseStream> {
    let token = match &auth_token {
        Some(t) => {
            let t = t.trim();
            if t.is_empty() {
                return Err(CodexErr::InvalidRequest(
                    "anthropic provider auth token is empty (check env_key or experimental_bearer_token)"
                        .to_string(),
                ));
            }
            t.to_string()
        }
        None => {
            return Err(CodexErr::InvalidRequest(
                "anthropic provider requires an API key: set env_key in model_providers config and \
                 set that environment variable (e.g. ANTHROPIC_AUTH_TOKEN)"
                    .to_string(),
            ));
        }
    };

    let (messages, tools, tool_name_map) = build_sdk_messages_and_tools(prompt)?;
    let system = if prompt.base_instructions.text.is_empty() {
        None
    } else {
        Some(prompt.base_instructions.text.clone())
    };

    let base_url_trimmed = base_url.trim_end_matches('/').to_string();
    let client = Client::builder()
        .base_url(base_url_trimmed)
        .api_key(SecretString::new(token.into()))
        .build()
        .map_err(|e| CodexErr::InvalidRequest(format!("anthropic client build: {e}")))?;

    let mut b = CreateMessagesRequestBuilder::default();
    b.model(model_info.slug.clone())
        .messages(messages)
        .max_tokens(DEFAULT_MAX_TOKENS)
        .stream(true);
    if let Some(s) = &system {
        b.system(s.clone());
    }
    if !tools.is_empty() {
        b.tools(tools);
    }
    let request = b
        .build()
        .map_err(|e| CodexErr::InvalidRequest(format!("anthropic request build: {e}")))?;

    let stream = client.messages().create_stream(request).await;
    let idle_timeout = provider.stream_idle_timeout();
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

    tokio::spawn(async move {
        process_sdk_stream(stream, tx_event, idle_timeout, tool_name_map).await;
    });

    Ok(ResponseStream { rx_event })
}

fn build_sdk_messages_and_tools(
    prompt: &Prompt,
) -> Result<(
    Vec<Message>,
    Vec<serde_json::Map<String, JsonValue>>,
    HashMap<String, String>,
)> {
    let items = prompt.get_formatted_input();
    let (tool_maps, tool_name_map) = tools_to_sdk_maps(prompt)?;
    let messages = response_items_to_sdk_messages(&items)?;
    Ok((messages, tool_maps, tool_name_map))
}

/// Builds Anthropic Messages API format. Consecutive tool_use blocks are merged into a single
/// assistant message, and consecutive tool_result blocks into a single user message, matching
/// the structure expected by Anthropic and compatible proxies (e.g. Kimi).
fn response_items_to_sdk_messages(items: &[ResponseItem]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    for item in items {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let blocks = content_to_sdk_blocks(content)?;
                if !blocks.is_empty() {
                    let role = match role.trim().to_lowercase().as_str() {
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::User,
                    };
                    out.push(Message {
                        role,
                        content: MessageContentList(blocks),
                    });
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                let input: JsonValue = serde_json::from_str(arguments)
                    .unwrap_or_else(|_| JsonValue::Object(serde_json::Map::new()));
                let block = MessageContent::ToolUse(ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                });
                if let Some(last) = out.last_mut() {
                    if last.role == MessageRole::Assistant {
                        let MessageContentList(blocks) = &mut last.content;
                        if blocks
                            .iter()
                            .all(|b| matches!(b, MessageContent::ToolUse(_)))
                        {
                            blocks.push(block);
                            continue;
                        }
                    }
                }
                out.push(Message {
                    role: MessageRole::Assistant,
                    content: MessageContentList(vec![block]),
                });
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                let content = output.text_content().unwrap_or_default().to_string();
                let block = MessageContent::ToolResult(ToolResult {
                    tool_use_id: call_id.clone(),
                    content: Some(content),
                    is_error: false,
                });
                if let Some(last) = out.last_mut() {
                    if last.role == MessageRole::User {
                        let MessageContentList(blocks) = &mut last.content;
                        if blocks
                            .iter()
                            .all(|b| matches!(b, MessageContent::ToolResult(_)))
                        {
                            blocks.push(block);
                            continue;
                        }
                    }
                }
                out.push(Message {
                    role: MessageRole::User,
                    content: MessageContentList(vec![block]),
                });
            }
            ResponseItem::CustomToolCallOutput { call_id, output, .. } => {
                let block = MessageContent::ToolResult(ToolResult {
                    tool_use_id: call_id.clone(),
                    content: output.text_content().map(|s| s.to_string()),
                    is_error: false,
                });
                if let Some(last) = out.last_mut() {
                    if last.role == MessageRole::User {
                        let MessageContentList(blocks) = &mut last.content;
                        if blocks
                            .iter()
                            .all(|b| matches!(b, MessageContent::ToolResult(_)))
                        {
                            blocks.push(block);
                            continue;
                        }
                    }
                }
                out.push(Message {
                    role: MessageRole::User,
                    content: MessageContentList(vec![block]),
                });
            }
            _ => {
                trace!(item_type = ?item, "skipping unsupported ResponseItem for Anthropic");
            }
        }
    }
    Ok(out)
}

fn content_to_sdk_blocks(content: &[ContentItem]) -> Result<Vec<MessageContent>> {
    let mut blocks = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.is_empty() {
                    blocks.push(MessageContent::Text(Text { text: text.clone() }));
                }
            }
            ContentItem::InputImage { image_url } => {
                blocks.push(MessageContent::Text(Text {
                    text: format!("[image: {image_url}]"),
                }));
            }
        }
    }
    Ok(blocks)
}

fn tools_to_sdk_maps(
    prompt: &Prompt,
) -> Result<(
    Vec<serde_json::Map<String, JsonValue>>,
    HashMap<String, String>,
)> {
    let responses_tools = create_tools_json_for_responses_api(&prompt.tools)?;
    let mut out = Vec::new();
    let mut sanitized_to_original: HashMap<String, String> = HashMap::new();
    let mut name_count: HashMap<String, u32> = HashMap::new();
    for t in responses_tools {
        let obj = t.as_object().ok_or_else(|| {
            CodexErr::InvalidRequest("expected tool to be a JSON object".to_string())
        })?;
        if obj.get("type").and_then(JsonValue::as_str) != Some("function") {
            continue;
        }
        let original_name = obj
            .get("name")
            .or_else(|| obj.get("function").and_then(|f: &JsonValue| f.get("name")))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        if original_name.trim().is_empty() {
            warn!(
                "anthropic wire: skipping tool with empty name (function object had no or empty name)"
            );
            continue;
        }
        let base = sanitize_tool_name_for_anthropic(&original_name);
        let count = name_count.entry(base.clone()).or_insert(0);
        *count += 1;
        let name = if *count > 1 {
            format!("{base}_{count}")
        } else {
            base
        };
        let description = obj
            .get("description")
            .or_else(|| obj.get("function").and_then(|f: &JsonValue| f.get("description")))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let parameters = obj
            .get("parameters")
            .or_else(|| obj.get("function").and_then(|f: &JsonValue| f.get("parameters")))
            .cloned()
            .unwrap_or(JsonValue::Object(serde_json::Map::new()));
        sanitized_to_original.insert(name.clone(), original_name);
        let mut map = serde_json::Map::new();
        map.insert("name".to_string(), JsonValue::String(name));
        map.insert("description".to_string(), JsonValue::String(description));
        map.insert("input_schema".to_string(), parameters);
        out.push(map);
    }
    Ok((out, sanitized_to_original))
}

fn sanitize_tool_name_for_anthropic(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_');
    if s.is_empty() || !s.chars().next().map_or(false, |c| c.is_ascii_alphabetic()) {
        let fallback = format!("tool_{s}");
        warn!(
            original_tool_name = %name,
            sanitized = %fallback,
            "anthropic wire: tool name sanitized to fallback (empty or not starting with letter)"
        );
        fallback
    } else {
        s.to_string()
    }
}

/// Pending tool_use block: (call_id, display_name, accumulated arguments JSON).
type PendingToolUse = (String, String, String);

async fn process_sdk_stream(
    mut stream: impl futures::Stream<
        Item = std::result::Result<MessagesStreamEvent, async_anthropic::errors::AnthropicError>,
    > + Unpin
    + Send
    + 'static,
    tx_event: mpsc::Sender<Result<ResponseEvent>>,
    idle_timeout: Duration,
    tool_name_map: HashMap<String, String>,
) {
    let mut response_id = String::new();
    let mut token_usage: Option<TokenUsage> = None;
    let mut pending_tool_use: Option<PendingToolUse> = None;

    while let Ok(Some(result)) = tokio::time::timeout(idle_timeout, stream.next()).await {
        let event = match result {
            Ok(ev) => ev,
            Err(e) => {
                let _ = tx_event.send(Err(map_anthropic_error(e))).await;
                break;
            }
        };

        match event {
            MessagesStreamEvent::MessageStart { message, usage } => {
                response_id = message.id.clone();
                if let Some(u) = usage {
                    let in_t = u.input_tokens.unwrap_or(0) as i64;
                    let out_t = u.output_tokens.unwrap_or(0) as i64;
                    token_usage = Some(TokenUsage {
                        input_tokens: in_t,
                        cached_input_tokens: 0,
                        output_tokens: out_t,
                        reasoning_output_tokens: 0,
                        total_tokens: in_t + out_t,
                    });
                }
                let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
            }
            MessagesStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                match content_block {
                    MessageContent::Text(Text { text }) => {
                        // Downstream expects OutputItemAdded before any OutputTextDelta so
                        // active_item is set; otherwise we get "OutputTextDelta without active item".
                        let id = format!("{response_id}-{index}");
                        let item = ResponseItem::Message {
                            id: Some(id),
                            role: "assistant".to_string(),
                            content: vec![],
                            end_turn: None,
                            phase: None,
                        };
                        let _ = tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await;
                        // If the SDK sent initial text in the block start, emit it as first delta.
                        if !text.is_empty() {
                            let _ = tx_event
                                .send(Ok(ResponseEvent::OutputTextDelta(text)))
                                .await;
                        }
                    }
                    MessageContent::ToolUse(t) => {
                        let name = tool_name_map
                            .get(&t.name)
                            .cloned()
                            .unwrap_or_else(|| t.name.clone());
                        let args =
                            serde_json::to_string(&t.input).unwrap_or_else(|_| "{}".to_string());
                        pending_tool_use = Some((t.id, name, args));
                    }
                    MessageContent::ToolResult(_) => {}
                }
            }
            MessagesStreamEvent::ContentBlockDelta { index: _, delta } => match delta {
                ContentBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        let _ = tx_event
                            .send(Ok(ResponseEvent::OutputTextDelta(text)))
                            .await;
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some((_, _, ref mut args)) = pending_tool_use {
                        if args == "{}" || args == "null" {
                            *args = partial_json;
                        } else {
                            args.push_str(&partial_json);
                        }
                    }
                }
            },
            MessagesStreamEvent::MessageDelta { usage, .. } => {
                if let Some(u) = usage {
                    let in_t = u.input_tokens.unwrap_or(0) as i64;
                    let out_t = u.output_tokens.unwrap_or(0) as i64;
                    token_usage = Some(TokenUsage {
                        input_tokens: in_t,
                        cached_input_tokens: 0,
                        output_tokens: out_t,
                        reasoning_output_tokens: 0,
                        total_tokens: in_t + out_t,
                    });
                }
            }
            MessagesStreamEvent::ContentBlockStop { .. } => {
                if let Some((call_id, name, args)) = pending_tool_use.take() {
                    let item = ResponseItem::FunctionCall {
                        id: None,
                        name,
                        namespace: None,
                        arguments: args,
                        call_id,
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                }
            }
            MessagesStreamEvent::MessageStop => {
                if let Some((call_id, name, args)) = pending_tool_use.take() {
                    let item = ResponseItem::FunctionCall {
                        id: None,
                        name,
                        namespace: None,
                        arguments: args,
                        call_id,
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                }
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id: std::mem::take(&mut response_id),
                        token_usage,
                    }))
                    .await;
                break;
            }
        }
    }
}

/// Maps SDK errors to Codex errors. For 400/stream failures (e.g. spawn agent with
/// Anthropic-compatible proxies like Kimi), the failure often occurs on the *continuation*
/// request (second request in a turn, with assistant message + tool results). The API response
/// body usually contains the validation reason but the SDK does not expose it; run with
/// `RUST_LOG=codex_core=debug` to see error_type and full error details.
fn map_anthropic_error(e: async_anthropic::errors::AnthropicError) -> CodexErr {
    use async_anthropic::errors::AnthropicError;
    // Log full error for debugging 400/stream failures (e.g. spawn agent with Kimi);
    // the API response body is not exposed by the SDK.
    match &e {
        AnthropicError::StreamError(se) => {
            debug!(
                error_type = %se.error_type,
                message = %se.message,
                "anthropic stream error (full error: {:?})",
                e
            );
        }
        _ => {
            debug!(full_error = ?e, "anthropic API error");
        }
    }
    match e {
        AnthropicError::BadRequest(body) => CodexErr::InvalidRequest(body),
        AnthropicError::Unauthorized => {
            CodexErr::InvalidRequest("anthropic API key invalid or missing".to_string())
        }
        AnthropicError::StreamError(se) => CodexErr::Stream(se.message, None),
        AnthropicError::NetworkError(inner) => CodexErr::Stream(inner.to_string(), None),
        AnthropicError::ApiError(msg) | AnthropicError::Unknown(msg) => CodexErr::Stream(msg, None),
        AnthropicError::DeserializationError(e) => CodexErr::Stream(e.to_string(), None),
        AnthropicError::UnexpectedError => {
            CodexErr::Stream("anthropic unexpected error".to_string(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_anthropic::types::MessageStart as ApiMessageStart;

    #[tokio::test]
    async fn tool_use_stream_emits_output_item_done() {
        let events: Vec<
            std::result::Result<MessagesStreamEvent, async_anthropic::errors::AnthropicError>,
        > = vec![
            Ok(MessagesStreamEvent::MessageStart {
                message: ApiMessageStart {
                    id: "msg-1".to_string(),
                    model: "claude-3".to_string(),
                    role: "assistant".to_string(),
                    content: vec![],
                    stop_reason: None,
                    stop_sequence: None,
                    usage: None,
                },
                usage: None,
            }),
            Ok(MessagesStreamEvent::ContentBlockStart {
                index: 0,
                content_block: MessageContent::ToolUse(ToolUse {
                    id: "call-1".to_string(),
                    name: "my_tool".to_string(),
                    input: serde_json::json!({"x": 1}),
                }),
            }),
            Ok(MessagesStreamEvent::ContentBlockStop { index: 0 }),
            Ok(MessagesStreamEvent::MessageStop),
        ];
        let stream = futures::stream::iter(events);
        let (tx, mut rx) = mpsc::channel(16);
        process_sdk_stream(stream, tx, Duration::from_secs(5), HashMap::new()).await;
        let mut collected = Vec::new();
        while let Some(r) = rx.recv().await {
            collected.push(r);
        }
        let created = collected
            .iter()
            .find(|r| matches!(r, Ok(ResponseEvent::Created)));
        assert!(created.is_some(), "expected Created: {:?}", collected);
        let done = collected.iter().find_map(|r| {
            if let Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            })) = r
            {
                Some((call_id.clone(), name.clone(), arguments.clone()))
            } else {
                None
            }
        });
        let (call_id, name, args) = done
            .unwrap_or_else(|| panic!("expected one OutputItemDone FunctionCall: {collected:?}"));
        assert_eq!(call_id, "call-1");
        assert_eq!(name, "my_tool");
        assert_eq!(args, "{\"x\":1}");
        let completed = collected
            .iter()
            .find(|r| matches!(r, Ok(ResponseEvent::Completed { .. })));
        assert!(completed.is_some(), "expected Completed: {:?}", collected);
    }

    #[tokio::test]
    async fn tool_use_stream_accumulates_input_json_delta() {
        let events: Vec<
            std::result::Result<MessagesStreamEvent, async_anthropic::errors::AnthropicError>,
        > = vec![
            Ok(MessagesStreamEvent::MessageStart {
                message: ApiMessageStart {
                    id: "msg-2".to_string(),
                    model: "claude-3".to_string(),
                    role: "assistant".to_string(),
                    content: vec![],
                    stop_reason: None,
                    stop_sequence: None,
                    usage: None,
                },
                usage: None,
            }),
            Ok(MessagesStreamEvent::ContentBlockStart {
                index: 0,
                content_block: MessageContent::ToolUse(ToolUse {
                    id: "call-2".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::Value::Object(serde_json::Map::new()),
                }),
            }),
            Ok(MessagesStreamEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: r#"{"command":"echo hi"}"#.to_string(),
                },
            }),
            Ok(MessagesStreamEvent::ContentBlockStop { index: 0 }),
            Ok(MessagesStreamEvent::MessageStop),
        ];
        let stream = futures::stream::iter(events);
        let (tx, mut rx) = mpsc::channel(16);
        process_sdk_stream(stream, tx, Duration::from_secs(5), HashMap::new()).await;
        let mut collected = Vec::new();
        while let Some(r) = rx.recv().await {
            collected.push(r);
        }
        let done = collected.iter().find_map(|r| {
            if let Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            })) = r
            {
                Some((call_id.clone(), name.clone(), arguments.clone()))
            } else {
                None
            }
        });
        let (call_id, name, args) = done
            .unwrap_or_else(|| panic!("expected one OutputItemDone FunctionCall: {collected:?}"));
        assert_eq!(call_id, "call-2");
        assert_eq!(name, "shell");
        assert!(
            args.contains("echo hi"),
            "arguments should contain streamed JSON: {args}"
        );
        assert!(
            !args.starts_with("{}\""),
            "arguments must not be empty-obj concatenated with JSON (invalid): {args}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&args).expect("arguments must be valid JSON");
        assert!(
            parsed.get("command").is_some(),
            "arguments should have command key: {args}"
        );
    }

    /// When API sends ContentBlockStart with empty input {{}} then InputJsonDelta with full JSON,
    /// we must not concatenate (producing invalid "{}\"...) but use the delta as the arguments.
    #[tokio::test]
    async fn tool_use_empty_start_then_single_input_json_delta_produces_valid_json() {
        let events: Vec<
            std::result::Result<MessagesStreamEvent, async_anthropic::errors::AnthropicError>,
        > = vec![
            Ok(MessagesStreamEvent::MessageStart {
                message: ApiMessageStart {
                    id: "msg-3".to_string(),
                    model: "claude-3".to_string(),
                    role: "assistant".to_string(),
                    content: vec![],
                    stop_reason: None,
                    stop_sequence: None,
                    usage: None,
                },
                usage: None,
            }),
            Ok(MessagesStreamEvent::ContentBlockStart {
                index: 0,
                content_block: MessageContent::ToolUse(ToolUse {
                    id: "call-3".to_string(),
                    name: "exec_command".to_string(),
                    input: serde_json::Value::Object(serde_json::Map::new()),
                }),
            }),
            Ok(MessagesStreamEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: r#"{"cmd": "ls -al bash"}"#.to_string(),
                },
            }),
            Ok(MessagesStreamEvent::ContentBlockStop { index: 0 }),
            Ok(MessagesStreamEvent::MessageStop),
        ];
        let stream = futures::stream::iter(events);
        let (tx, mut rx) = mpsc::channel(16);
        process_sdk_stream(stream, tx, Duration::from_secs(5), HashMap::new()).await;
        let mut collected = Vec::new();
        while let Some(r) = rx.recv().await {
            collected.push(r);
        }
        let done = collected.iter().find_map(|r| {
            if let Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                arguments, ..
            })) = r
            {
                Some(arguments.clone())
            } else {
                None
            }
        });
        let args = done
            .unwrap_or_else(|| panic!("expected one OutputItemDone FunctionCall: {collected:?}"));
        let parsed: serde_json::Value =
            serde_json::from_str(&args).expect("arguments must be valid JSON");
        assert_eq!(
            parsed.get("cmd").and_then(|v| v.as_str()),
            Some("ls -al bash"),
            "arguments should have cmd field: {args}"
        );
    }
}
