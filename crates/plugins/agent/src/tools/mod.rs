//! Responses function tools call `ctx.call(service, method, args)`.
//!
//! A tool name is `service.method` (split on the first `.`). Responses function
//! names match `^[a-zA-Z0-9_-]+$`, so the request sends `.` as `_`.

use async_trait::async_trait;
use fujiang_core::BotContext;
use serde_json::Value;
use tracing::warn;

use crate::client::{ChatMessage, FunctionTool, LlmClient, LlmError, ModelOutput, PriorCall};

pub(crate) const TOOL_CAP_REPLY: &str = "要查的步骤太多了，我先停在这里。";
pub(crate) const TOOL_FAIL_REPLY: &str = "我没查成，过一会儿再问我一次。";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    name: String,
    wire_name: String,
    service: String,
    method: String,
    description: String,
    parameters: Value,
}

impl ToolSpec {
    /// `name` is `service.method`. The first `.` splits the ServiceHub call.
    pub fn parse(name: &str, description: &str, parameters: Value) -> Option<Self> {
        let name = name.trim();
        let (service, method) = split_tool_name(name)?;
        let wire_name = name.replace('.', "_");
        if !wire_name_ok(&wire_name) {
            return None;
        }
        let parameters = if parameters.is_object() {
            parameters
        } else {
            default_parameters()
        };
        Some(Self {
            name: name.to_string(),
            wire_name,
            service: service.to_string(),
            method: method.to_string(),
            description: description.trim().to_string(),
            parameters,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn wire_name(&self) -> &str {
        &self.wire_name
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolReply {
    Text(String),
    Degraded(&'static str),
}

#[async_trait]
pub(crate) trait ToolHub: Send + Sync {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value>;
}

#[async_trait]
impl ToolHub for BotContext {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.call(service, method, args).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuiltinTools {
    pub rank: bool,
    pub contest: bool,
    pub problem: bool,
}

#[cfg(test)]
impl BuiltinTools {
    const ON: Self = Self {
        rank: true,
        contest: true,
        problem: true,
    };
    const OFF: Self = Self {
        rank: false,
        contest: false,
        problem: false,
    };
}

pub(crate) fn assemble_tools(configured: &[ToolSpec], builtins: BuiltinTools) -> Vec<ToolSpec> {
    let mut tools = builtin_specs(builtins);
    for spec in configured {
        if tools.iter().any(|kept| kept.wire_name == spec.wire_name) {
            continue;
        }
        tools.push(spec.clone());
    }
    tools
}

fn builtin_specs(builtins: BuiltinTools) -> Vec<ToolSpec> {
    let mut specs = Vec::new();
    if builtins.rank {
        specs.push(builtin_spec(
            "rank.list",
            "列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。",
            default_parameters(),
        ));
        specs.push(builtin_spec(
            "rank.get",
            "按 Codeforces handle 查一位已登记队员。可选字符串参数 handle。不传则这次查询失败。",
            serde_json::json!({
                "type": "object",
                "properties": { "handle": { "type": "string" } }
            }),
        ));
    }
    if builtins.contest {
        specs.push(builtin_spec(
            "contest.upcoming",
            "列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。",
            serde_json::json!({
                "type": "object",
                "properties": { "oj": { "type": "string" } }
            }),
        ));
        specs.push(builtin_spec(
            "contest.today",
            "列出今天开始的比赛。不需要参数。",
            default_parameters(),
        ));
    }
    if builtins.problem {
        specs.push(builtin_spec(
            "problem.today",
            "返回当前每日一题里已经保存的题目。不需要参数。还没保存时是空数组。",
            default_parameters(),
        ));
    }
    specs
}

fn builtin_spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec::parse(name, description, parameters).expect("builtin tool name")
}

pub(crate) async fn run_tool_loop<H: ToolHub + ?Sized>(
    client: &LlmClient,
    messages: &[ChatMessage],
    configured: &[ToolSpec],
    builtins: BuiltinTools,
    max_rounds: u32,
    hub: &H,
) -> Result<ToolReply, LlmError> {
    let tools = assemble_tools(configured, builtins);
    if tools.is_empty() {
        return Ok(ToolReply::Text(client.complete(messages).await?));
    }
    let defs: Vec<FunctionTool<'_>> = tools
        .iter()
        .map(|spec| FunctionTool {
            name: spec.wire_name(),
            description: spec.description(),
            parameters: spec.parameters(),
        })
        .collect();
    let mut prior = Vec::new();
    let mut used = 0u32;
    loop {
        match client.complete_with_tools(messages, &prior, &defs).await? {
            ModelOutput::Text(text) => return Ok(ToolReply::Text(text)),
            ModelOutput::Calls(calls) => {
                if used >= max_rounds {
                    warn!(max_tool_rounds = max_rounds, "agent tool round cap");
                    return Ok(ToolReply::Degraded(TOOL_CAP_REPLY));
                }
                for call in calls {
                    let Some(spec) = tools.iter().find(|spec| spec.wire_name == call.name) else {
                        warn!(tool = %call.name, "agent tool unknown");
                        return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                    };
                    let args = match parse_arguments(&call.arguments) {
                        Ok(value) => value,
                        Err(err) => {
                            warn!(error = %err, tool = spec.name(), "agent tool arguments");
                            return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                        }
                    };
                    let value = match hub.invoke(spec.service(), spec.method(), args).await {
                        Ok(value) => value,
                        Err(err) => {
                            warn!(
                                error = %err,
                                tool = spec.name(),
                                service = spec.service(),
                                method = spec.method(),
                                "agent tool"
                            );
                            return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                        }
                    };
                    let output = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
                    prior.push(PriorCall {
                        call_id: call.call_id,
                        name: spec.wire_name.clone(),
                        arguments: call.arguments,
                        output,
                    });
                }
                used = used.saturating_add(1);
            }
        }
    }
}

fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    let (service, method) = name.split_once('.')?;
    if service.is_empty() || method.is_empty() {
        None
    } else {
        Some((service, method))
    }
}

fn wire_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn default_parameters() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

fn parse_arguments(raw: &str) -> Result<Value, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(trimmed).map_err(|_| "arguments are not json")
}

#[cfg(test)]
mod tests;
