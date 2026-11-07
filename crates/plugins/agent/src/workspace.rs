use fujiang_core::Source;
use serde_json::{json, Value};
use sqlx::SqlitePool;

pub const RUNNER_UNAVAILABLE: &str = "没有已验收的独立 rootfs，workspace 执行不可用";
const BASH_WIRE: &str = "workspace_bash";
const CODE_WIRE: &str = "workspace_code";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalQq(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupChat(i64);

impl PersonalQq {
    pub fn get(self) -> i64 {
        self.0
    }
}

impl GroupChat {
    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceOwner {
    Personal(PersonalQq),
    Group(GroupChat),
}

#[derive(Debug, Clone, Copy)]
pub struct HostTurn {
    pub source: Source,
    pub actor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCmd {
    Show,
    Personal,
    Group,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredChoice {
    Personal,
    Group,
}

pub fn parse_cmd<'a>(mut parts: impl Iterator<Item = &'a str>) -> WorkspaceCmd {
    match parts.next() {
        None => WorkspaceCmd::Show,
        Some("personal" | "本人") if parts.next().is_none() => WorkspaceCmd::Personal,
        Some("group" | "群") if parts.next().is_none() => WorkspaceCmd::Group,
        _ => WorkspaceCmd::Bad,
    }
}

pub fn is_exec_wire(wire: &str) -> bool {
    wire == BASH_WIRE || wire == CODE_WIRE
}

pub async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS workspace_choice (
            group_id INTEGER NOT NULL,
            actor_qq INTEGER NOT NULL,
            choice TEXT NOT NULL,
            PRIMARY KEY (group_id, actor_qq)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn command(
    pool: &SqlitePool,
    turn: HostTurn,
    cmd: &WorkspaceCmd,
) -> anyhow::Result<String> {
    match cmd {
        WorkspaceCmd::Bad => Ok(
            "工作区只接受 personal、本人、group、群。不能带别人的号码、昵称、目录或引用。"
                .to_string(),
        ),
        WorkspaceCmd::Show => describe(pool, turn).await,
        WorkspaceCmd::Personal => set_choice(pool, turn, StoredChoice::Personal).await,
        WorkspaceCmd::Group => set_choice(pool, turn, StoredChoice::Group).await,
    }
}

pub async fn handle_tool(
    pool: Option<&SqlitePool>,
    turn: Option<HostTurn>,
    wire: &str,
    args: &Value,
) -> Result<Value, ()> {
    let Some(pool) = pool else {
        return Err(());
    };
    let Some(turn) = turn else {
        return Err(());
    };
    if !is_exec_wire(wire) {
        return Err(());
    }
    let owner = resolve(pool, turn).await.map_err(|_| ())?;
    let payload = accept_script(wire, args)?;
    match crate::workspace_backend::execute(owner, payload) {
        Ok(result) => Ok(json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "timed_out": result.timed_out,
            "truncated": result.truncated,
        })),
        Err(_) => Ok(json!({
            "available": false,
            "bound": owner_label(owner),
            "error": RUNNER_UNAVAILABLE,
        })),
    }
}

pub async fn resolve(pool: &SqlitePool, turn: HostTurn) -> Result<WorkspaceOwner, &'static str> {
    match turn.source {
        Source::Friend { id } => {
            if id != turn.actor || id <= 0 || turn.actor <= 0 {
                Err("私聊的真实宿主和 source 不一致")
            } else {
                Ok(WorkspaceOwner::Personal(PersonalQq(turn.actor)))
            }
        }
        Source::Group { id } => {
            if id <= 0 || turn.actor <= 0 {
                return Err("当前群或发言人无效");
            }
            let choice = read_choice(pool, id, turn.actor).await?;
            if choice == StoredChoice::Personal {
                Ok(WorkspaceOwner::Personal(PersonalQq(turn.actor)))
            } else {
                Ok(WorkspaceOwner::Group(GroupChat(id)))
            }
        }
    }
}

fn accept_script(wire: &str, args: &Value) -> Result<crate::workspace_runner::Payload, ()> {
    let Some(obj) = args.as_object() else {
        return Err(());
    };
    if obj
        .keys()
        .any(|key| !matches!(key.as_str(), "script" | "lang"))
    {
        return Err(());
    }
    let Some(script) = obj.get("script").and_then(Value::as_str) else {
        return Err(());
    };
    if script.is_empty() || script.contains('\0') {
        return Err(());
    }
    let lang = match obj.get("lang") {
        None => None,
        Some(value) => Some(value.as_str().ok_or(())?),
    };
    let script = script.to_string();
    match (wire, lang) {
        (BASH_WIRE, None | Some("bash")) => Ok(crate::workspace_runner::Payload::Bash(script)),
        (BASH_WIRE, Some("sh")) => Ok(crate::workspace_runner::Payload::Sh(script)),
        (CODE_WIRE, Some("python")) => Ok(crate::workspace_runner::Payload::Python(script)),
        _ => Err(()),
    }
}

async fn describe(pool: &SqlitePool, turn: HostTurn) -> anyhow::Result<String> {
    match resolve(pool, turn).await {
        Ok(owner) => Ok(format!(
            "当前工作区是{}。{}",
            owner_label(owner),
            RUNNER_UNAVAILABLE
        )),
        Err(reason) => Ok(reason.to_string()),
    }
}

async fn set_choice(
    pool: &SqlitePool,
    turn: HostTurn,
    choice: StoredChoice,
) -> anyhow::Result<String> {
    let Source::Group { id } = turn.source else {
        return describe(pool, turn).await.map(|text| match turn.source {
            Source::Friend { id } if id == turn.actor && id > 0 => {
                "私聊工作区固定是当前本人，不能改成别人或别的群。".to_string()
            }
            _ => text,
        });
    };
    if id <= 0 || turn.actor <= 0 {
        return Ok("当前群或发言人无效".to_string());
    }
    let stored = match choice {
        StoredChoice::Personal => "personal",
        StoredChoice::Group => "group",
    };
    sqlx::query(
        "INSERT INTO workspace_choice (group_id, actor_qq, choice)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(group_id, actor_qq) DO UPDATE SET choice = excluded.choice",
    )
    .bind(id)
    .bind(turn.actor)
    .bind(stored)
    .execute(pool)
    .await?;
    describe(pool, turn).await
}

async fn read_choice(
    pool: &SqlitePool,
    group_id: i64,
    actor: i64,
) -> Result<StoredChoice, &'static str> {
    let choice: Option<String> = sqlx::query_scalar(
        "SELECT choice FROM workspace_choice WHERE group_id = ?1 AND actor_qq = ?2",
    )
    .bind(group_id)
    .bind(actor)
    .fetch_optional(pool)
    .await
    .map_err(|_| "工作区选择读失败")?;
    Ok(match choice.as_deref() {
        Some("personal") => StoredChoice::Personal,
        _ => StoredChoice::Group,
    })
}

fn owner_label(owner: WorkspaceOwner) -> String {
    match owner {
        WorkspaceOwner::Personal(qq) => format!("本人 {}", qq.get()),
        WorkspaceOwner::Group(group) => format!("当前群 {}", group.get()),
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
