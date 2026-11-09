use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::config::AgentConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImagePart {
    data_url: String,
}

impl ImagePart {
    pub(crate) fn new(data_url: String) -> Self {
        Self { data_url }
    }

    pub(crate) fn data_url(&self) -> &str {
        &self.data_url
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(skip)]
    pub(crate) images: Vec<ImagePart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUse {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub input_known: bool,
    pub output_known: bool,
}

impl TokenUse {
    pub fn known(self) -> bool {
        self.input_known && self.output_known
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub usage: TokenUse,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("missing API key in environment variable {env}")]
    MissingApiKey { env: String },
    #[error("http: {0}")]
    Http(reqwest::Error),
    #[error("LLM API status {status}: {body}")]
    Api { status: u16, body: String },
    #[error("LLM request timed out")]
    Timeout,
    #[error("failed to decode LLM response: {0}")]
    Decode(String),
    #[error("truncated or incomplete LLM response: {0}")]
    Truncated(String),
    #[error("model context limit is not verified")]
    ContextClosed,
    #[error("request exceeds the context budget")]
    OverBudget,
    #[error("image tokens are not metered")]
    ImageUnmetered,
    #[error("image request exceeds transport limits")]
    PayloadLimited,
}

pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    temperature: f64,
    timeout: Duration,
    limit: Option<u64>,
    image_cap: Option<u64>,
    max_image_bytes: u64,
}

impl LlmClient {
    /// Reads the key from the env var named by `cfg.api_key_env`. Never panics.
    pub fn from_config(cfg: &AgentConfig, http: reqwest::Client) -> Result<Self, LlmError> {
        let raw = std::env::var(&cfg.api_key_env).ok();
        let api_key = require_key(&cfg.api_key_env, raw.as_deref())?;
        Ok(Self::new(cfg, http, api_key))
    }

    pub async fn complete(&self, messages: &[ChatMessage]) -> Result<Completion, LlmError> {
        self.complete_sent(messages).await.0
    }

    pub(crate) async fn complete_sent(
        &self,
        messages: &[ChatMessage],
    ) -> (Result<Completion, LlmError>, SentCall) {
        if let Err(err) = self.request_bound(messages, &[], &[]) {
            return (Err(err), SentCall::none());
        }
        let request = responses_request(
            &self.model,
            self.temperature,
            messages,
            &[],
            &[],
            crate::context_budget::OUTPUT_RESERVE,
            false,
        );
        match self.send_json("responses", &request).await {
            Err(err) => (Err(err), SentCall::unknown()),
            Ok((status, body)) => {
                let usage = token_use(&body);
                let sent = SentCall::once(usage);
                if !status.is_success() {
                    return (
                        Err(LlmError::Api {
                            status: status.as_u16(),
                            body: error_message_or_snippet(&body),
                        }),
                        sent,
                    );
                }
                match parse_responses(&body) {
                    Ok(text) => (Ok(Completion { text, usage }), sent),
                    Err(err) => (Err(err), sent),
                }
            }
        }
    }

    pub(crate) async fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        prior: &[PriorCall],
        tools: &[FunctionTool<'_>],
    ) -> Result<(ModelOutput, TokenUse), LlmError> {
        self.request_bound(messages, prior, tools)?;
        let request = responses_request(
            &self.model,
            self.temperature,
            messages,
            prior,
            tools,
            crate::context_budget::OUTPUT_RESERVE,
            false,
        );
        let body = self.post_ok(&request).await?;
        Ok((parse_model_output(&body)?, token_use(&body)))
    }

    pub(crate) fn new(cfg: &AgentConfig, http: reqwest::Client, api_key: String) -> Self {
        Self {
            http,
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            api_key,
            temperature: cfg.temperature,
            timeout: Duration::from_secs(cfg.timeout_secs),
            limit: crate::context_budget::resolve(
                &cfg.base_url,
                &cfg.model,
                cfg.context_budget,
                cfg.model_context_limit,
            ),
            image_cap: crate::context_budget::image_token_cap(
                &cfg.base_url,
                &cfg.model,
                cfg.image_token_upper_bound,
            ),
            max_image_bytes: cfg.max_image_bytes,
        }
    }

    pub(crate) fn max_image_bytes(&self) -> u64 {
        self.max_image_bytes
    }

    pub(crate) fn request_bytes(
        &self,
        messages: &[ChatMessage],
        prior: &[PriorCall],
        tools: &[FunctionTool<'_>],
    ) -> Result<u64, LlmError> {
        if self.limit.is_none() {
            return Err(LlmError::ContextClosed);
        }
        let urls = image_urls(messages, prior);
        let each = if urls.is_empty() {
            0
        } else {
            self.image_cap.ok_or(LlmError::ImageUnmetered)?
        };
        let mut url_bytes = 0u64;
        let mut decoded = 0u64;
        for url in &urls {
            let len = u64::try_from(url.len()).unwrap_or(u64::MAX);
            url_bytes = url_bytes.saturating_add(len);
            let one = decoded_image_bytes(url);
            if one > crate::context_budget::MAX_ONE_IMAGE_BYTES {
                return Err(LlmError::PayloadLimited);
            }
            decoded = decoded.saturating_add(one);
        }
        if crate::context_budget::image_transport_blocked(urls.len(), url_bytes, decoded) {
            return Err(LlmError::PayloadLimited);
        }
        let reserve = crate::context_budget::OUTPUT_RESERVE;
        let request = responses_request(
            &self.model,
            self.temperature,
            messages,
            prior,
            tools,
            reserve,
            true,
        );
        let bytes = serde_json::to_vec(&request).map_err(|_| LlmError::OverBudget)?;
        let projected = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let wire = physical_wire_len(projected, &urls)?;
        if wire > crate::context_budget::MAX_REQUEST_BODY_BYTES {
            return Err(LlmError::PayloadLimited);
        }
        let visual = u64::try_from(urls.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(each);
        Ok(projected.saturating_add(visual).saturating_add(reserve))
    }

    pub(crate) fn request_bound(
        &self,
        messages: &[ChatMessage],
        prior: &[PriorCall],
        tools: &[FunctionTool<'_>],
    ) -> Result<u64, LlmError> {
        let total = self.request_bytes(messages, prior, tools)?;
        let limit = self.limit.ok_or(LlmError::ContextClosed)?;
        let reserve = crate::context_budget::OUTPUT_RESERVE;
        if reserve > limit || total > limit {
            Err(LlmError::OverBudget)
        } else {
            Ok(total)
        }
    }

    async fn send_json(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<(reqwest::StatusCode, String), LlmError> {
        let response = self
            .http
            .post(endpoint(&self.base_url, path))
            .timeout(self.timeout)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(map_reqwest)?;
        let status = response.status();
        let text = response.text().await.map_err(map_reqwest)?;
        Ok((status, text))
    }

    async fn post_ok(&self, request: &ResponsesRequest<'_>) -> Result<String, LlmError> {
        let (status, body) = self.send_json("responses", request).await?;
        if !status.is_success() {
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: error_message_or_snippet(&body),
            });
        }
        Ok(body)
    }
}

fn require_key(env_name: &str, raw: Option<&str>) -> Result<String, LlmError> {
    match raw.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => Ok(key.to_string()),
        None => Err(LlmError::MissingApiKey {
            env: env_name.to_string(),
        }),
    }
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim().trim_end_matches('/'), path)
}

/// One executed function call, echoed on the next `/responses` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PriorCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub output: String,
    pub images: Vec<ImagePart>,
    pub image_note: String,
}

impl PriorCall {
    pub(crate) fn bare(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments.into(),
            output: output.into(),
            images: Vec::new(),
            image_note: String::new(),
        }
    }

    pub(crate) fn from_call(call: &FunctionCall, output: impl Into<String>) -> Self {
        Self::bare(&call.call_id, &call.name, &call.arguments, output)
    }

    pub(crate) fn with_images(
        call: &FunctionCall,
        output: impl Into<String>,
        images: Vec<ImagePart>,
        image_note: String,
    ) -> Self {
        Self {
            images,
            image_note,
            ..Self::from_call(call, output)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FunctionTool<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: &'a Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelOutput {
    Text(String),
    Calls(Vec<FunctionCall>),
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<InputItem<'a>>,
    temperature: f64,
    max_output_tokens: u64,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireFunction<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum InputItem<'a> {
    Message {
        role: &'a str,
        content: &'a str,
    },
    UserParts {
        role: &'a str,
        content: Vec<ContentPart<'a>>,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: &'a str,
        output: &'a str,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum ContentPart<'a> {
    Text {
        #[serde(rename = "type")]
        kind: &'static str,
        text: &'a str,
    },
    Image {
        #[serde(rename = "type")]
        kind: &'static str,
        image_url: &'a str,
    },
}

#[derive(Serialize)]
struct WireFunction<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

fn responses_request<'a>(
    model: &'a str,
    temperature: f64,
    messages: &'a [ChatMessage],
    prior: &'a [PriorCall],
    tools: &'a [FunctionTool<'a>],
    max_output_tokens: u64,
    project_images: bool,
) -> ResponsesRequest<'a> {
    let mut instructions = String::new();
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {
                if message.content.is_empty() {
                    continue;
                }
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&message.content);
            }
            Role::User if !message.images.is_empty() => input.push(InputItem::UserParts {
                role: "user",
                content: user_parts(message, project_images),
            }),
            Role::User => input.push(InputItem::Message {
                role: "user",
                content: &message.content,
            }),
            Role::Assistant => input.push(InputItem::Message {
                role: "assistant",
                content: &message.content,
            }),
        }
    }
    for call in prior {
        input.push(InputItem::FunctionCall {
            kind: "function_call",
            call_id: &call.call_id,
            name: &call.name,
            arguments: &call.arguments,
        });
        input.push(InputItem::FunctionCallOutput {
            kind: "function_call_output",
            call_id: &call.call_id,
            output: &call.output,
        });
        if !call.images.is_empty() {
            input.push(InputItem::UserParts {
                role: "user",
                content: held_parts(&call.image_note, &call.images, project_images),
            });
        }
    }
    let tools = tools
        .iter()
        .map(|tool| WireFunction {
            kind: "function",
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
        })
        .collect();
    ResponsesRequest {
        model,
        instructions: if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        },
        input,
        temperature,
        max_output_tokens,
        stream: false,
        tools,
    }
}

fn held_parts<'a>(
    note: &'a str,
    images: &'a [ImagePart],
    project_images: bool,
) -> Vec<ContentPart<'a>> {
    let mut parts = Vec::with_capacity(images.len() + 1);
    parts.push(ContentPart::Text {
        kind: "input_text",
        text: note,
    });
    for image in images {
        parts.push(ContentPart::Image {
            kind: "input_image",
            image_url: if project_images { "" } else { image.data_url() },
        });
    }
    parts
}

fn user_parts(message: &ChatMessage, project_images: bool) -> Vec<ContentPart<'_>> {
    let mut parts = Vec::with_capacity(message.images.len() + 1);
    parts.push(ContentPart::Text {
        kind: "input_text",
        text: &message.content,
    });
    for image in &message.images {
        parts.push(ContentPart::Image {
            kind: "input_image",
            image_url: if project_images { "" } else { image.data_url() },
        });
    }
    parts
}

pub(crate) fn physical_wire_len(projected_len: u64, urls: &[&str]) -> Result<u64, LlmError> {
    let mut extra = 0u64;
    for url in urls {
        extra = extra.saturating_add(json_string_extra(url)?);
    }
    Ok(projected_len.saturating_add(extra))
}

fn json_string_extra(url: &str) -> Result<u64, LlmError> {
    let encoded = serde_json::to_string(url).map_err(|_| LlmError::OverBudget)?;
    Ok(u64::try_from(encoded.len())
        .unwrap_or(u64::MAX)
        .saturating_sub(2))
}

fn image_urls<'a>(messages: &'a [ChatMessage], prior: &'a [PriorCall]) -> Vec<&'a str> {
    let mut urls: Vec<&str> = messages
        .iter()
        .flat_map(|message| message.images.iter().map(|image| image.data_url()))
        .collect();
    urls.extend(
        prior
            .iter()
            .flat_map(|call| call.images.iter().map(|image| image.data_url())),
    );
    urls
}

fn decoded_image_bytes(url: &str) -> u64 {
    let raw = url.rsplit_once(',').map(|(_, data)| data).unwrap_or(url);
    let len = raw.chars().filter(|ch| !ch.is_whitespace()).count();
    u64::try_from(len.saturating_mul(3) / 4).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SentCall {
    pub calls: i64,
    pub usage: TokenUse,
}

impl SentCall {
    fn none() -> Self {
        Self {
            calls: 0,
            usage: TokenUse::default(),
        }
    }

    fn unknown() -> Self {
        Self {
            calls: 1,
            usage: TokenUse::default(),
        }
    }

    fn once(usage: TokenUse) -> Self {
        Self { calls: 1, usage }
    }
}

fn map_reqwest(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Http(err)
    }
}

fn snippet(body: &str) -> String {
    const MAX: usize = 400;
    let body = body.trim();
    if body.chars().count() <= MAX {
        return body.to_string();
    }
    let mut out: String = body.chars().take(MAX).collect();
    out.push_str("...");
    out
}

fn error_message_or_snippet(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return snippet(body);
    };
    let message = value
        .get("error")
        .and_then(|err| err.get("message"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    match message {
        Some(text) => snippet(text),
        None => snippet(body),
    }
}

fn token_use(body: &str) -> TokenUse {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return TokenUse::default();
    };
    let Some(usage) = value.get("usage").filter(|item| item.is_object()) else {
        return TokenUse::default();
    };
    let input = measured_token(usage.get("input_tokens"));
    let output = measured_token(usage.get("output_tokens"));
    TokenUse {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        input_known: input.is_some(),
        output_known: output.is_some(),
    }
}

fn measured_token(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_i64()?;
    if number < 0 {
        None
    } else {
        Some(number)
    }
}

fn parse_responses(body: &str) -> Result<String, LlmError> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| LlmError::Decode(err.to_string()))?;
    text_from_response(&value)
}

fn parse_model_output(body: &str) -> Result<ModelOutput, LlmError> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| LlmError::Decode(err.to_string()))?;
    let calls = function_calls(&value)?;
    if !calls.is_empty() {
        return Ok(ModelOutput::Calls(calls));
    }
    text_from_response(&value).map(ModelOutput::Text)
}

fn text_from_response(value: &Value) -> Result<String, LlmError> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Ok(text.to_string());
        }
    }
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Err(LlmError::Truncated("missing output".into()));
    };
    let mut text = String::new();
    for item in output {
        if !is_message_item(item) {
            continue;
        }
        text.push_str(&message_text(item));
    }
    if text.is_empty() {
        Err(LlmError::Truncated("empty output".into()))
    } else {
        Ok(text)
    }
}

fn function_calls(value: &Value) -> Result<Vec<FunctionCall>, LlmError> {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        let (Some(name), Some(call_id)) = (name, call_id) else {
            return Err(LlmError::Decode(
                "function_call missing call_id or name".into(),
            ));
        };
        let arguments = match item.get("arguments") {
            Some(Value::String(text)) if text.trim().is_empty() => "{}".to_string(),
            Some(Value::String(text)) => text.clone(),
            Some(_) => {
                return Err(LlmError::Decode(
                    "function_call arguments must be a string".into(),
                ));
            }
            None => "{}".to_string(),
        };
        calls.push(FunctionCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments,
        });
    }
    Ok(calls)
}

fn is_message_item(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => true,
        Some(_) => false,
        None => item.get("role").and_then(Value::as_str) == Some("assistant"),
    }
}

fn message_text(item: &Value) -> String {
    let mut out = String::new();
    match item.get("content") {
        Some(Value::String(text)) => out.push_str(text),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = output_text_part(part) {
                    out.push_str(text);
                }
            }
        }
        _ => {}
    }
    out
}

fn output_text_part(part: &Value) -> Option<&str> {
    match part {
        Value::String(text) => Some(text),
        other => {
            if other.get("type").and_then(Value::as_str) != Some("output_text") {
                return None;
            }
            other.get("text").and_then(Value::as_str)
        }
    }
}

#[cfg(test)]
pub(crate) fn rendered_request(
    client: &LlmClient,
    messages: &[ChatMessage],
    prior: &[PriorCall],
    tools: &[FunctionTool<'_>],
) -> Value {
    serde_json::to_value(responses_request(
        &client.model,
        client.temperature,
        messages,
        prior,
        tools,
        crate::context_budget::OUTPUT_RESERVE,
        false,
    ))
    .expect("request json")
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
