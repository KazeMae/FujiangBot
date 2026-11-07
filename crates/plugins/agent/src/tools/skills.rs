use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::contest_pack;
use super::fun_pack;
use super::qq_read;
use super::schedule_pack;
use super::{assemble_tools, default_parameters, BuiltinTools, ToolSpec};

pub(crate) const UNKNOWN_SKILL: &str = "agent skill unknown";
pub(crate) const CLOSED_SKILL: &str = "agent skill closed";

const LIST_DESC: &str =
    "列出可以打开的技能包。每项有 name 和 description。不返回包里的工具参数。不需要参数。";
const OPEN_DESC: &str = "打开一个技能包。字符串参数 name。同一次发言的后续模型回合才带上这个包的工具。未知的 name 这次调用失败。再次打开同一个包仍然成功。";
const PING_DESC: &str =
    "占位技能。打开之后多一个无参数工具，结果固定为 pong。不访问 QQ，也不写数据库。";
const PONG_DESC: &str = "返回固定结果 pong。不需要参数。不访问 QQ，也不写数据库。";

struct Pack {
    name: &'static str,
    description: &'static str,
    tools: &'static [PackTool],
}

struct PackTool {
    name: &'static str,
    wire: &'static str,
    description: &'static str,
}

const PACKS: &[Pack] = &[Pack {
    name: "ping",
    description: PING_DESC,
    tools: &[PackTool {
        name: "ping.pong",
        wire: "ping_pong",
        description: PONG_DESC,
    }],
}];

pub(crate) fn resident_tools(configured: &[ToolSpec], builtins: BuiltinTools) -> Vec<ToolSpec> {
    round_tools(configured, builtins, &BTreeSet::new())
}

pub(crate) fn round_tools(
    configured: &[ToolSpec],
    builtins: BuiltinTools,
    opened: &BTreeSet<String>,
) -> Vec<ToolSpec> {
    round_tools_gated(
        configured,
        builtins,
        opened,
        crate::workspace_backend::production_available(),
    )
}

pub(crate) fn round_tools_gated(
    configured: &[ToolSpec],
    builtins: BuiltinTools,
    opened: &BTreeSet<String>,
    accepted: bool,
) -> Vec<ToolSpec> {
    let mut tools = assemble_tools(configured, builtins);
    tools.retain(|spec| !reserved_wire(spec.wire_name()));
    for spec in meta_specs() {
        push_new(&mut tools, spec);
    }
    for pack in PACKS {
        if !opened.contains(pack.name) {
            continue;
        }
        for tool in pack.tools {
            let spec = ToolSpec::parse(tool.name, tool.description, default_parameters())
                .expect(tool.name);
            push_new(&mut tools, spec);
        }
    }
    if opened.contains(qq_read::PACK) {
        for tool in qq_read::read_tools() {
            let spec = ToolSpec::parse(tool.name, tool.description, qq_read::parameters(tool.wire))
                .expect(tool.name);
            push_new(&mut tools, spec);
        }
    }
    if opened.contains(schedule_pack::PACK) {
        for tool in schedule_pack::tools() {
            let spec = ToolSpec::parse(
                tool.name,
                tool.description,
                schedule_pack::parameters(tool.wire),
            )
            .expect(tool.name);
            push_new(&mut tools, spec);
        }
    }
    if builtins.fun && opened.contains(fun_pack::PACK) {
        for tool in fun_pack::tools() {
            let spec =
                ToolSpec::parse(tool.name, tool.description, fun_pack::parameters(tool.wire))
                    .expect(tool.name);
            push_new(&mut tools, spec);
        }
    }
    if builtins.contest && opened.contains(contest_pack::PACK) {
        for tool in contest_pack::tools() {
            let spec = ToolSpec::parse(
                tool.name,
                tool.description,
                contest_pack::parameters(tool.wire),
            )
            .expect(tool.name);
            push_new(&mut tools, spec);
        }
    }
    finish_workspace(tools, accepted)
}

fn finish_workspace(mut tools: Vec<ToolSpec>, accepted: bool) -> Vec<ToolSpec> {
    tools.retain(|spec| !super::workspace_pack::reserved(spec));
    if accepted {
        for spec in super::workspace_pack::specs() {
            push_new(&mut tools, spec);
        }
    }
    tools
}

#[derive(Debug)]
pub(crate) enum SkillOut {
    Ready(Result<Value, &'static str>),
    Qq,
    Schedule,
}

pub(crate) fn exec(
    wire: &str,
    args: &Value,
    builtins: BuiltinTools,
    opened_now: &BTreeSet<String>,
    opened: &mut BTreeSet<String>,
) -> Option<SkillOut> {
    if wire == "skills_list" {
        return Some(SkillOut::Ready(Ok(list_value(builtins))));
    }
    if wire == "skills_open" {
        return Some(SkillOut::Ready(open_value(args, builtins, opened)));
    }
    let pack = pack_of(wire)?;
    if !opened_now.contains(pack) {
        return Some(SkillOut::Ready(Err(CLOSED_SKILL)));
    }
    if (pack == fun_pack::PACK && !builtins.fun)
        || (pack == contest_pack::PACK && !builtins.contest)
    {
        return Some(SkillOut::Ready(Err(CLOSED_SKILL)));
    }
    if pack == fun_pack::PACK || pack == contest_pack::PACK {
        return None;
    }
    if pack == qq_read::PACK {
        return Some(SkillOut::Qq);
    }
    if pack == schedule_pack::PACK {
        return Some(SkillOut::Schedule);
    }
    Some(SkillOut::Ready(invoke(wire)))
}

fn list_value(builtins: BuiltinTools) -> Value {
    let mut rows: Vec<Value> = PACKS
        .iter()
        .map(|pack| json!({"name": pack.name, "description": pack.description}))
        .collect();
    rows.push(json!({
        "name": qq_read::PACK,
        "description": qq_read::PACK_DESC
    }));
    rows.push(json!({
        "name": schedule_pack::PACK,
        "description": schedule_pack::PACK_DESC
    }));
    if builtins.fun {
        rows.push(json!({
            "name": fun_pack::PACK,
            "description": fun_pack::PACK_DESC
        }));
    }
    if builtins.contest {
        rows.push(json!({
            "name": contest_pack::PACK,
            "description": contest_pack::PACK_DESC
        }));
    }
    Value::Array(rows)
}

fn open_value(
    args: &Value,
    builtins: BuiltinTools,
    opened: &mut BTreeSet<String>,
) -> Result<Value, &'static str> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let known = PACKS.iter().any(|pack| pack.name == name)
        || name == qq_read::PACK
        || name == schedule_pack::PACK
        || (builtins.fun && name == fun_pack::PACK)
        || (builtins.contest && name == contest_pack::PACK);
    if !known {
        return Err(UNKNOWN_SKILL);
    }
    opened.insert(name.to_string());
    Ok(json!({"name": name, "opened": true}))
}

fn invoke(wire: &str) -> Result<Value, &'static str> {
    match wire {
        "ping_pong" => Ok(json!({"pong": true})),
        _ => Err(CLOSED_SKILL),
    }
}

fn meta_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::parse("skills.list", LIST_DESC, default_parameters()).expect("skills.list"),
        ToolSpec::parse(
            "skills.open",
            OPEN_DESC,
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }),
        )
        .expect("skills.open"),
    ]
}

fn reserved_wire(wire: &str) -> bool {
    wire == "skills_list" || wire == "skills_open" || pack_of(wire).is_some()
}

fn pack_of(wire: &str) -> Option<&'static str> {
    if qq_read::is_read_tool(wire) {
        return Some(qq_read::PACK);
    }
    if schedule_pack::is_job_tool(wire) {
        return Some(schedule_pack::PACK);
    }
    if fun_pack::is_fun_tool(wire) {
        return Some(fun_pack::PACK);
    }
    if contest_pack::is_contest_tool(wire) {
        return Some(contest_pack::PACK);
    }
    PACKS.iter().find_map(|pack| {
        pack.tools
            .iter()
            .find(|tool| tool.wire == wire)
            .map(|_| pack.name)
    })
}

fn push_new(tools: &mut Vec<ToolSpec>, spec: ToolSpec) {
    if tools
        .iter()
        .any(|kept| kept.wire_name() == spec.wire_name())
    {
        return;
    }
    tools.push(spec);
}
