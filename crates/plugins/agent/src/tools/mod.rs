//! Responses function tools call `ctx.call(service, method, args)`.
//!
//! A tool name is `service.method` (split on the first `.`). Responses function
//! names match `^[a-zA-Z0-9_-]+$`, so the request sends `.` as `_`.

use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use fujiang_core::{BotContext, Messenger, Source};
use serde_json::{Map, Value};
use tracing::warn;

use crate::session::session_key;

use crate::client::{
    ChatMessage, FunctionCall, FunctionTool, LlmClient, LlmError, ModelOutput, PriorCall,
};

mod qq_read;
mod skills;

pub(crate) use skills::resident_tools;

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

#[derive(Debug)]
pub(crate) struct ToolTurn {
    pub reply: Result<ToolReply, LlmError>,
    pub calls: i64,
    pub in_tokens: i64,
    pub out_tokens: i64,
}

#[async_trait]
pub(crate) trait ToolHub: Send + Sync {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value>;

    async fn onebot(&self, _action: &str, _params: Value) -> anyhow::Result<Value> {
        anyhow::bail!("onebot call is not available")
    }
}

#[async_trait]
impl ToolHub for BotContext {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.call(service, method, args).await
    }

    async fn onebot(&self, action: &str, params: Value) -> anyhow::Result<Value> {
        let messenger = self.messenger().await;
        Messenger::onebot(messenger.as_ref(), action, params).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuiltinTools {
    pub rank: bool,
    pub contest: bool,
    pub problem: bool,
    pub memo: bool,
    pub memo_write: bool,
    pub fun: bool,
    pub memory: bool,
    pub memory_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoCallScope {
    source_kind: String,
    source_id: i64,
    session: String,
    user_id: i64,
}

impl MemoCallScope {
    pub(crate) fn from_source(source: Source, user_id: i64) -> Self {
        let (kind, id) = match source {
            Source::Group { id } => ("group", id),
            Source::Friend { id } => ("friend", id),
        };
        Self {
            source_kind: kind.to_string(),
            source_id: id,
            session: session_key(source),
            user_id,
        }
    }
}

pub(crate) struct ToolPass<'a> {
    pub max_rounds: u32,
    pub scope: Option<&'a MemoCallScope>,
    pub memory: Option<&'a sqlx::SqlitePool>,
    pub memory_unused_secs: i64,
    pub memory_max_entries: i64,
}

#[cfg(test)]
impl ToolPass<'static> {
    pub(crate) fn rounds(max_rounds: u32) -> Self {
        Self {
            max_rounds,
            scope: None,
            memory: None,
            memory_unused_secs: crate::memory_consolidate::unused_secs(
                crate::memory_consolidate::DEFAULT_UNUSED_DAYS,
            ),
            memory_max_entries: i64::from(crate::memory_consolidate::DEFAULT_MAX_ENTRIES),
        }
    }
}

#[cfg(test)]
impl BuiltinTools {
    const ON: Self = Self {
        rank: true,
        contest: true,
        problem: true,
        memo: true,
        memo_write: false,
        fun: true,
        memory: true,
        memory_write: true,
    };
    const OFF: Self = Self {
        rank: false,
        contest: false,
        problem: false,
        memo: false,
        memo_write: false,
        fun: false,
        memory: false,
        memory_write: false,
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

pub(crate) fn tools_command_text(tools: &[ToolSpec]) -> String {
    match tool_list_block(tools) {
        Some(block) => block,
        None => "当前没有可用工具。".to_string(),
    }
}

pub(crate) fn instructions_with_catalog(prompt: &str, tools: &[ToolSpec], inject: bool) -> String {
    let prompt = prompt.trim();
    if !inject {
        return prompt.to_string();
    }
    match tool_list_block(tools) {
        Some(block) if prompt.is_empty() => block,
        Some(block) => format!("{prompt}\n\n{block}"),
        None => prompt.to_string(),
    }
}

fn tool_list_block(tools: &[ToolSpec]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let lines = tools
        .iter()
        .map(catalog_line)
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("可用工具：\n{lines}"))
}

fn catalog_line(spec: &ToolSpec) -> String {
    let description = spec.description();
    if description.is_empty() {
        spec.name().to_string()
    } else {
        format!("{}：{}", spec.name(), description)
    }
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
    if builtins.memo {
        specs.push(builtin_spec(
            "memo.list",
            "列出当前对话已经记下的备忘。不需要参数。还没有备忘时是空数组。",
            default_parameters(),
        ));
    }
    if builtins.memo && builtins.memo_write {
        specs.push(builtin_spec(
            "memo.add",
            "给当前对话新记一条备忘。字符串参数 body 是正文。正文为空或这条对话已经写满时，这次调用失败。",
            serde_json::json!({
                "type": "object",
                "properties": { "body": { "type": "string" } }
            }),
        ));
    }
    if builtins.fun {
        specs.push(builtin_spec(
            "fun.tags",
            "列出图库的 tag，含条数和别名。旧专辑也在这份清单里。不需要参数。还没有 tag 时是空数组。",
            default_parameters(),
        ));
        specs.push(builtin_spec(
            "fun.stars",
            "列出收藏夹的名称和链接。不需要参数。还没有收藏时是空数组。",
            default_parameters(),
        ));
        specs.push(builtin_spec(
            "fun.learn",
            "列出当前会话的学话。可选字符串 trigger，只返回这个触发词。群里看本群，私聊看全局。还没有时是空数组。",
            serde_json::json!({
                "type": "object",
                "properties": { "trigger": { "type": "string" } }
            }),
        ));
        specs.push(builtin_spec(
            "fun.search",
            "在 tag、收藏和当前会话的学话里按原文查找。字符串参数 q。q 为空时这次调用失败。",
            serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } }
            }),
        ));
    }
    if builtins.memory {
        specs.push(builtin_spec(
            "memory.list",
            "列出分层记忆的键。可选字符串 scope：account、group 或 user。不传时群里看账号、本群和当前发言人，私聊看账号和当前发言人。还没有时是空数组。",
            serde_json::json!({
                "type": "object",
                "properties": { "scope": { "type": "string" } }
            }),
        ));
        specs.push(builtin_spec(
            "memory.get",
            "读取一条分层记忆。字符串 scope 和 key。没有这条时结果是 null。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "key": { "type": "string" }
                }
            }),
        ));
    }
    if builtins.memory && builtins.memory_write {
        specs.push(builtin_spec(
            "memory.upsert",
            "写入或覆盖一条分层记忆。字符串 scope、key、value。value 最多 500 个字。user 只写当前发言人，group 只写当前群。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "key": { "type": "string" },
                    "value": { "type": "string" }
                }
            }),
        ));
        specs.push(builtin_spec(
            "memory.forget",
            "删除分层记忆。字符串 scope 和 key。可选布尔 prefix 为 true 时按前缀删除。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "key": { "type": "string" },
                    "prefix": { "type": "boolean" }
                }
            }),
        ));
        specs.push(builtin_spec(
            "memory.consolidate",
            "整理当前可见的分层记忆。可选字符串 scope：account、group 或 user。不传时群里整理账号、本群和当前发言人，私聊整理账号和当前发言人。丢掉太久没用的，每层条数有上限。返回 removed 和 kept。",
            serde_json::json!({
                "type": "object",
                "properties": { "scope": { "type": "string" } }
            }),
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
    pass: &ToolPass<'_>,
    hub: &H,
) -> ToolTurn {
    tool_loop(client, messages, pass, hub, |opened| {
        skills::round_tools(configured, builtins, opened)
    })
    .await
}

async fn tool_loop<H, F>(
    client: &LlmClient,
    messages: &[ChatMessage],
    pass: &ToolPass<'_>,
    hub: &H,
    mut visible: F,
) -> ToolTurn
where
    H: ToolHub + ?Sized,
    F: FnMut(&BTreeSet<String>) -> Vec<ToolSpec>,
{
    let max_rounds = pass.max_rounds;
    let mut opened = BTreeSet::new();
    let mut members = HashMap::new();
    let mut prior = Vec::new();
    let mut used = 0u32;
    let mut calls = 0i64;
    let mut in_tokens = 0i64;
    let mut out_tokens = 0i64;
    loop {
        let tools = visible(&opened);
        if tools.is_empty() && prior.is_empty() {
            return match client.complete(messages).await {
                Ok(done) => tool_turn(
                    Ok(ToolReply::Text(done.text)),
                    1,
                    done.usage.input_tokens,
                    done.usage.output_tokens,
                ),
                Err(err) => tool_turn(Err(err), 1, 0, 0),
            };
        }
        let defs: Vec<FunctionTool<'_>> = tools
            .iter()
            .map(|spec| FunctionTool {
                name: spec.wire_name(),
                description: spec.description(),
                parameters: spec.parameters(),
            })
            .collect();
        let (output, usage) = match client.complete_with_tools(messages, &prior, &defs).await {
            Ok(round) => round,
            Err(err) => return tool_turn(Err(err), calls + 1, in_tokens, out_tokens),
        };
        calls += 1;
        in_tokens += usage.input_tokens;
        out_tokens += usage.output_tokens;
        match output {
            ModelOutput::Text(text) => {
                return tool_turn(Ok(ToolReply::Text(text)), calls, in_tokens, out_tokens);
            }
            ModelOutput::Calls(found) => {
                if used >= max_rounds {
                    warn!(max_tool_rounds = max_rounds, "agent tool round cap");
                    return tool_turn(
                        Ok(ToolReply::Degraded(TOOL_CAP_REPLY)),
                        calls,
                        in_tokens,
                        out_tokens,
                    );
                }
                let opened_now = opened.clone();
                for call in found {
                    let args = match parse_arguments(&call.arguments) {
                        Ok(value) => value,
                        Err(err) => {
                            warn!(error = %err, tool = %call.name, "agent tool arguments");
                            return tool_turn(
                                Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                calls,
                                in_tokens,
                                out_tokens,
                            );
                        }
                    };
                    if let Some(step) = skills::exec(&call.name, &args, &opened_now, &mut opened) {
                        match step {
                            skills::SkillOut::Ready(Ok(value)) => {
                                prior.push(prior_call(call, value));
                            }
                            skills::SkillOut::Ready(Err(tag)) => {
                                warn!(tool = %call.name, "{tag}");
                                return tool_turn(
                                    Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                    calls,
                                    in_tokens,
                                    out_tokens,
                                );
                            }
                            skills::SkillOut::Qq => {
                                match qq_read::handle(
                                    hub,
                                    pass.scope,
                                    &call.name,
                                    &args,
                                    &mut members,
                                )
                                .await
                                {
                                    Ok(value) => prior.push(prior_call(call, value)),
                                    Err(qq_read::QqFail::Denied) => {
                                        warn!(tool = %call.name, "{}", qq_read::DENIED);
                                        return tool_turn(
                                            Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                            calls,
                                            in_tokens,
                                            out_tokens,
                                        );
                                    }
                                    Err(qq_read::QqFail::Down(err)) => {
                                        warn!(error = %err, tool = %call.name, "agent tool");
                                        return tool_turn(
                                            Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                            calls,
                                            in_tokens,
                                            out_tokens,
                                        );
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    let Some(spec) = tools.iter().find(|spec| spec.wire_name == call.name) else {
                        warn!(tool = %call.name, "agent tool unknown");
                        return tool_turn(
                            Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                            calls,
                            in_tokens,
                            out_tokens,
                        );
                    };
                    let args = bind_memo_scope(spec.service(), pass.scope, args);
                    let args = bind_fun_scope(spec.service(), spec.method(), pass.scope, args);
                    let args = bind_memory_scope(spec.service(), pass.scope, args);
                    let value = if spec.service() == "memory" {
                        match memory_value(pass, spec.method(), &args).await {
                            Ok(value) => value,
                            Err(err) => {
                                warn!(error = %err, tool = spec.name(), "agent tool");
                                return tool_turn(
                                    Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                    calls,
                                    in_tokens,
                                    out_tokens,
                                );
                            }
                        }
                    } else {
                        match hub.invoke(spec.service(), spec.method(), args).await {
                            Ok(value) => value,
                            Err(err) => {
                                warn!(
                                    error = %err,
                                    tool = spec.name(),
                                    service = spec.service(),
                                    method = spec.method(),
                                    "agent tool"
                                );
                                return tool_turn(
                                    Ok(ToolReply::Degraded(TOOL_FAIL_REPLY)),
                                    calls,
                                    in_tokens,
                                    out_tokens,
                                );
                            }
                        }
                    };
                    prior.push(prior_call(call, value));
                }
                used = used.saturating_add(1);
            }
        }
    }
}

fn prior_call(call: FunctionCall, value: serde_json::Value) -> PriorCall {
    PriorCall {
        call_id: call.call_id,
        name: call.name,
        arguments: call.arguments,
        output: serde_json::to_string(&value).unwrap_or_else(|_| "null".into()),
    }
}

async fn memory_value(pass: &ToolPass<'_>, method: &str, args: &Value) -> anyhow::Result<Value> {
    let Some(pool) = pass.memory else {
        anyhow::bail!("memory db is not open");
    };
    if pass.scope.is_none() {
        anyhow::bail!("memory scope is missing");
    }
    crate::layered_memory::call_limited(
        pool,
        method,
        args,
        crate::session::unix_now(),
        pass.memory_unused_secs,
        pass.memory_max_entries,
    )
    .await
}

fn tool_turn(
    reply: Result<ToolReply, LlmError>,
    calls: i64,
    in_tokens: i64,
    out_tokens: i64,
) -> ToolTurn {
    ToolTurn {
        reply,
        calls,
        in_tokens,
        out_tokens,
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

fn bind_fun_scope(
    service: &str,
    method: &str,
    scope: Option<&MemoCallScope>,
    args: Value,
) -> Value {
    if service != "fun" || (method != "learn" && method != "search") {
        return args;
    }
    let Some(scope) = scope else {
        return args;
    };
    let mut obj = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    if scope.source_kind == "group" {
        obj.insert("group_id".to_string(), Value::from(scope.source_id));
    } else {
        obj.remove("group_id");
    }
    Value::Object(obj)
}

fn bind_memory_scope(service: &str, scope: Option<&MemoCallScope>, args: Value) -> Value {
    let Some(scope) = scope.filter(|_| service == "memory") else {
        return args;
    };
    let mut obj = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    obj.insert("user_id".to_string(), Value::from(scope.user_id));
    obj.insert("source".to_string(), Value::String(memory_source(scope)));
    if scope.source_kind == "group" {
        obj.insert("group_id".to_string(), Value::from(scope.source_id));
    } else {
        obj.remove("group_id");
    }
    obj.remove("scope_id");
    Value::Object(obj)
}

fn memory_source(scope: &MemoCallScope) -> String {
    if scope.source_kind == "group" {
        format!("group:{}", scope.source_id)
    } else {
        "private".to_string()
    }
}

fn bind_memo_scope(service: &str, scope: Option<&MemoCallScope>, args: Value) -> Value {
    let Some(scope) = scope.filter(|_| service == "memo") else {
        return args;
    };
    let mut obj = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    obj.insert(
        "source_kind".to_string(),
        Value::String(scope.source_kind.clone()),
    );
    obj.insert("source_id".to_string(), Value::from(scope.source_id));
    obj.insert("session".to_string(), Value::String(scope.session.clone()));
    obj.insert("user_id".to_string(), Value::from(scope.user_id));
    Value::Object(obj)
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
