use std::collections::BTreeSet;

use serde_json::{json, Value};

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
    tools
}

pub(crate) fn exec(
    wire: &str,
    args: &Value,
    opened_now: &BTreeSet<String>,
    opened: &mut BTreeSet<String>,
) -> Option<Result<Value, &'static str>> {
    if wire == "skills_list" {
        return Some(Ok(list_value()));
    }
    if wire == "skills_open" {
        return Some(open_value(args, opened));
    }
    let pack = pack_of(wire)?;
    if !opened_now.contains(pack) {
        return Some(Err(CLOSED_SKILL));
    }
    Some(invoke(wire))
}

fn list_value() -> Value {
    Value::Array(
        PACKS
            .iter()
            .map(|pack| json!({"name": pack.name, "description": pack.description}))
            .collect(),
    )
}

fn open_value(args: &Value, opened: &mut BTreeSet<String>) -> Result<Value, &'static str> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !PACKS.iter().any(|pack| pack.name == name) {
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
