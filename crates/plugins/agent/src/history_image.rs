use fujiang_core::{BotContext, Media, MessageEvent, Segment, Source};
use serde_json::Value;

use crate::client::{FunctionCall, FunctionTool, ImagePart, LlmClient, PriorCall};
use crate::config::AgentConfig;
use crate::tools::{MemoCallScope, ToolHub};
use crate::vision;

pub(crate) const WIRE: &str = "view_image";
pub(crate) const NAME: &str = "view.image";
pub(crate) const DESC: &str =
    "查看当前会话聊天记录或引用消息里的图片，参数是占位符里的编号，每次最多 3 张";

const NOT_IN_SESSION: &str = "不在当前会话或已过期";
const BAD_ID: &str = "编号格式不对";
const TOO_MANY: &str = "一次最多 3 张";
const TURN_CAP: &str = "本轮看图已达上限";
const GONE: &str = "图片取不到（可能已过期）";
const BUDGET: &str = "超出上下文预算";
const MAX_PER_CALL: usize = 3;
const MAX_PER_TURN: usize = 6;

pub(crate) fn offered(cfg: &AgentConfig) -> bool {
    cfg.vision_enabled
        && crate::context_budget::image_token_cap(
            &cfg.base_url,
            &cfg.model,
            cfg.image_token_upper_bound,
        )
        .is_some()
}

pub(crate) fn parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "ids": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": MAX_PER_CALL
            }
        }
    })
}

pub(crate) fn with_placeholders(
    user_text: &str,
    message_id: i64,
    own: &[Segment],
    quoted: Option<(i64, &[Segment])>,
) -> String {
    let mut out = user_text.to_string();
    push_marks(&mut out, "图片", message_id, own);
    if let Some((id, segs)) = quoted {
        push_marks(&mut out, "引用图片", id, segs);
    }
    out
}

pub(crate) fn quoted_segments(
    current: Source,
    stored: Option<&str>,
    fetched: Option<&MessageEvent>,
) -> Option<Vec<Segment>> {
    if let Some(raw) = stored {
        return serde_json::from_str(raw).ok();
    }
    let ev = fetched?;
    if ev.source == current {
        Some(ev.segments.clone())
    } else {
        None
    }
}

pub(crate) async fn stamp(ctx: &BotContext, msg: &MessageEvent, user_text: String) -> String {
    let quoted = match msg.reply_id() {
        Some(id) => {
            let scope = MemoCallScope::from_source(msg.source, msg.user_id());
            segments_in_session(ctx, &scope, id).await
        }
        None => None,
    };
    let pair = msg
        .reply_id()
        .zip(quoted.as_ref())
        .map(|(id, segs)| (id, segs.as_slice()));
    with_placeholders(&user_text, msg.id, &msg.segments, pair)
}

pub(crate) async fn segments_in_session<H: ToolHub + ?Sized>(
    hub: &H,
    scope: &MemoCallScope,
    message_id: i64,
) -> Option<Vec<Segment>> {
    let source = scope.chat_source();
    let (group_id, peer) = match source {
        Source::Group { id } => (Some(id), None),
        Source::Friend { id } => (None, Some(id)),
    };
    let stored = hub
        .session_message_segments(group_id, peer, message_id)
        .await
        .ok()
        .flatten();
    let fetched = if stored.is_some() {
        None
    } else {
        hub.get_message(message_id).await.ok()
    };
    quoted_segments(source, stored.as_deref(), fetched.as_ref())
}

pub(crate) struct ViewCtx<'a, H: ToolHub + ?Sized> {
    pub hub: &'a H,
    pub scope: Option<&'a MemoCallScope>,
    pub client: &'a LlmClient,
    pub messages: &'a [crate::client::ChatMessage],
    pub prior: &'a [PriorCall],
    pub defs: &'a [FunctionTool<'a>],
    pub seen: &'a mut usize,
}

pub(crate) async fn perform<H: ToolHub + ?Sized>(
    ctx: ViewCtx<'_, H>,
    call: &FunctionCall,
    args: &Value,
) -> PriorCall {
    match gather(&ctx, args).await {
        Stop(text) => PriorCall::from_call(call, text),
        Ready { output, held } => attach(ctx, call, output, held).await,
    }
}

enum Gather {
    Stop(String),
    Ready {
        output: String,
        held: Vec<(String, ImagePart)>,
    },
}

use Gather::{Ready, Stop};

async fn gather<H: ToolHub + ?Sized>(ctx: &ViewCtx<'_, H>, args: &Value) -> Gather {
    let Some(scope) = ctx.scope else {
        return Stop(NOT_IN_SESSION.to_string());
    };
    let Ok(ids) = id_strings(args) else {
        return Stop(BAD_ID.to_string());
    };
    if ids.is_empty() {
        return Stop(BAD_ID.to_string());
    }
    if ids.len() > MAX_PER_CALL {
        return Stop(TOO_MANY.to_string());
    }
    if ctx.seen.saturating_add(ids.len()) > MAX_PER_TURN {
        return Stop(TURN_CAP.to_string());
    }
    let mut lines = Vec::new();
    let mut held = Vec::new();
    for raw in ids {
        match one_image(ctx.hub, scope, ctx.client.max_image_bytes(), &raw).await {
            Ok(pair) => held.push(pair),
            Err(text) => lines.push(text),
        }
    }
    if !held.is_empty() {
        let labels: Vec<&str> = held.iter().map(|(label, _)| label.as_str()).collect();
        lines.push(format!("已返回：{}", labels.join(" ")));
    }
    Ready {
        output: lines.join("\n"),
        held,
    }
}

async fn one_image<H: ToolHub + ?Sized>(
    hub: &H,
    scope: &MemoCallScope,
    max_bytes: u64,
    raw: &str,
) -> Result<(String, ImagePart), String> {
    let (id, k) = parse_id(raw).ok_or_else(|| BAD_ID.to_string())?;
    let segs = segments_in_session(hub, scope, id)
        .await
        .ok_or_else(|| NOT_IN_SESSION.to_string())?;
    let stored = nth_image(&segs, k).ok_or_else(|| missing(k))?;
    let label = format!("{id}:{k}");
    if let Ok(bytes) = hub.fetch_media_bytes(&stored).await {
        return finish(&label, &bytes, max_bytes);
    }
    let fresh = hub.get_message(id).await.map_err(|_| GONE.to_string())?;
    if fresh.source != scope.chat_source() {
        return Err(GONE.to_string());
    }
    let media = nth_image(&fresh.segments, k).ok_or_else(|| GONE.to_string())?;
    let bytes = hub
        .fetch_media_bytes(&media)
        .await
        .map_err(|_| GONE.to_string())?;
    finish(&label, &bytes, max_bytes)
}

fn finish(label: &str, bytes: &[u8], max_bytes: u64) -> Result<(String, ImagePart), String> {
    match vision::image_part(bytes, max_bytes) {
        Ok(part) => Ok((label.to_string(), part)),
        Err(fault) => Err(vision::fault_tip(fault).to_string()),
    }
}

async fn attach<H: ToolHub + ?Sized>(
    ctx: ViewCtx<'_, H>,
    call: &FunctionCall,
    output: String,
    held: Vec<(String, ImagePart)>,
) -> PriorCall {
    if held.is_empty() {
        return PriorCall::from_call(call, output);
    }
    let labels: Vec<&str> = held.iter().map(|(label, _)| label.as_str()).collect();
    let note = format!("view_image 返回的图片：{}", labels.join(" "));
    let images: Vec<ImagePart> = held.into_iter().map(|(_, part)| part).collect();
    let candidate = PriorCall::with_images(call, output, images, note);
    let mut tentative = ctx.prior.to_vec();
    tentative.push(candidate.clone());
    match ctx.client.request_bound(ctx.messages, &tentative, ctx.defs) {
        Ok(_) => {
            *ctx.seen = ctx.seen.saturating_add(candidate.images.len());
            candidate
        }
        Err(_) => PriorCall::from_call(call, BUDGET),
    }
}

fn id_strings(args: &Value) -> Result<Vec<String>, ()> {
    let list = args.get("ids").and_then(Value::as_array).ok_or(())?;
    list.iter()
        .map(|item| item.as_str().map(str::to_string).ok_or(()))
        .collect()
}

fn parse_id(raw: &str) -> Option<(i64, u32)> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix('[')
        .and_then(|rest| {
            let rest = rest
                .strip_prefix("引用图片#")
                .or_else(|| rest.strip_prefix("图片#"))?;
            rest.strip_suffix(']')
        })
        .unwrap_or(trimmed);
    let (id, k) = body.split_once(':')?;
    if !digits(id) || !digits(k) {
        return None;
    }
    let id: i64 = id.parse().ok()?;
    let k: u32 = k.parse().ok()?;
    if id <= 0 || k == 0 {
        return None;
    }
    Some((id, k))
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn nth_image(segs: &[Segment], k: u32) -> Option<Media> {
    segs.iter()
        .filter_map(|seg| match seg {
            Segment::Image { src, .. } => Some(src.clone()),
            _ => None,
        })
        .nth(usize::try_from(k - 1).ok()?)
}

fn missing(k: u32) -> String {
    format!("这条消息没有第 {k} 张图")
}

fn push_marks(out: &mut String, kind: &str, id: i64, segs: &[Segment]) {
    let mut k = 0u32;
    for seg in segs {
        if !matches!(seg, Segment::Image { .. }) {
            continue;
        }
        k += 1;
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('[');
        out.push_str(kind);
        out.push('#');
        out.push_str(&id.to_string());
        out.push(':');
        out.push_str(&k.to_string());
        out.push(']');
    }
}

#[cfg(test)]
#[path = "history_image_tests.rs"]
mod tests;
