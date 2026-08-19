use async_trait::async_trait;
use fujiang_core::{BotContext, Event, Flow, Interest, Plugin, PluginScope};
use fujiang_store::{AttachResult, DetachResult};
use tracing::warn;

const HELP: &str = "学话、收藏夹、图库。改学习/图库默认要在 fun.admins 里（名单空=谁都能改）。\n\
【学话】原话触发。本群有条目只用本群的，没有则用导入/私聊里的全局词库。\n\
.learn add <触发词> <回复>   写入当前群或私聊\n\
.learn list [触发词]         列出本群/私聊里的句子\n\
.learn del <触发词> [n]      删全部；带序号只删第 n 条\n\
【收藏】\n\
.star                        列出名称和链接\n\
.star add <名> <url>         新增（重名拒绝）\n\
.star set <名> <url>         新增或覆盖\n\
.star del <名>\n\
【图库】一张图可挂多个 tag；摘掉最后一个 tag 也不删文件。\n\
.tag list                    所有 tag、张数、别名\n\
.tag add <tag>               只建空 tag\n\
.tag alias <tag> <别名>      来只别名 也算这个 tag\n\
.tag merge <from> <to>       把 from 上的图也挂到 to，from 还在\n\
.tag retire <tag>            去掉这个 tag（图还在）\n\
来只<tag>                    随机一张带该 tag 的图\n\
回复一张图 + .添加<tag>      给这张图挂 tag\n\
回复一张图 + .删除<tag>      只摘这一个 tag\n\
回复一张图 + .标签           列出这张图的全部 tag";

#[derive(Default)]
pub struct FunPlugin;

#[async_trait]
impl Plugin for FunPlugin {
    fn meta(&self) -> fujiang_core::PluginMeta {
        fujiang_core::PluginMeta::new("fun", "学话、收藏夹、图库", self.commands())
    }

    fn name(&self) -> &'static str {
        "fun"
    }

    fn help(&self) -> &'static str {
        HELP
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            ".learn", ".star", ".tag", ".idea", ".album", ".添加", ".删除", ".标签", ".想法",
        ]
    }

    fn command_prefixes(&self) -> &'static [&'static str] {
        &[".添加", ".删除"]
    }

    fn interest(&self) -> Interest {
        Interest::Messages
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        _scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let line = msg.command_line();
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(Flow::Continue);
        }

        if parts[0] == ".learn" {
            handle_learn(ctx, msg, &parts, &line).await?;
            return Ok(Flow::Stop);
        }
        if parts[0] == ".star" {
            handle_star(ctx, msg, &parts).await?;
            return Ok(Flow::Stop);
        }
        if parts[0] == ".tag" || parts[0] == ".idea" || parts[0] == ".album" {
            handle_tag(ctx, msg, &parts).await?;
            return Ok(Flow::Stop);
        }

        if line == ".标签" || line == ".想法" {
            handle_show_tags(ctx, msg).await?;
            return Ok(Flow::Stop);
        }

        if line.starts_with(".添加") || line.starts_with(".删除") {
            handle_image_cmd(ctx, msg, &line).await?;
            return Ok(Flow::Stop);
        }

        if let Some(name) = line.strip_prefix("来只") {
            if !name.is_empty() {
                match ctx.store.random_by_tag(name).await? {
                    Some(path) if path.exists() => {
                        ctx.send_image(msg.source, path.to_string_lossy()).await?;
                    }
                    _ => {}
                }
                return Ok(Flow::Stop);
            }
        }

        if let Some(reply) = learn_reply(ctx, msg.source.group_id(), &line).await? {
            ctx.reply_text(msg, reply).await?;
            return Ok(Flow::Continue);
        }
        Ok(Flow::Continue)
    }
}

async fn learn_reply(
    ctx: &BotContext,
    group_id: Option<i64>,
    line: &str,
) -> anyhow::Result<Option<String>> {
    if let Some(reply) = ctx.store.learn_random(group_id, line).await? {
        return Ok(Some(reply));
    }
    // 旧 bot 按第一个空格分词匹配，例如「活着？ 啊」仍能中。
    let first = line.split_whitespace().next().unwrap_or("");
    if first.is_empty() || first == line {
        return Ok(None);
    }
    ctx.store.learn_random(group_id, first).await
}

async fn can_mutate(ctx: &BotContext, qq: i64) -> bool {
    let cfg = ctx.bot_config().await;
    cfg.fun_allow_mutate && cfg.is_fun_admin(qq)
}

async fn handle_learn(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    parts: &[&str],
    line: &str,
) -> anyhow::Result<()> {
    if parts.len() < 2 {
        ctx.reply_text(msg, HELP).await?;
        return Ok(());
    }
    match parts[1] {
        "add" => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            // .learn add <trigger> <rest...>
            let rest = line.splitn(4, ' ').collect::<Vec<_>>();
            if rest.len() < 4 {
                ctx.reply_text(msg, "格式：.learn add <触发词> <回复>")
                    .await?;
                return Ok(());
            }
            ctx.store
                .learn_add(rest[2], rest[3], msg.user_id(), msg.source.group_id())
                .await?;
            ctx.reply_text(msg, "learned").await?;
        }
        "list" => {
            let trigger = parts.get(2).copied();
            let rows = ctx.store.learn_list(msg.source.group_id(), trigger).await?;
            if rows.is_empty() {
                ctx.reply_text(msg, "空").await?;
                return Ok(());
            }
            let mut s = String::new();
            for (i, r) in rows.iter().enumerate() {
                s.push_str(&format!("{}. [{}] {}\n", i + 1, r.trigger, r.reply));
            }
            ctx.reply_text(msg, s.trim_end()).await?;
        }
        "del" => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if parts.len() < 3 {
                ctx.reply_text(msg, "格式：.learn del <触发词> [n]").await?;
                return Ok(());
            }
            let n = parts.get(3).and_then(|x| x.parse().ok());
            let c = ctx
                .store
                .learn_del(msg.source.group_id(), parts[2], n)
                .await?;
            ctx.reply_text(msg, format!("已删除 {c} 条")).await?;
        }
        _ => {
            ctx.reply_text(msg, HELP).await?;
        }
    }
    Ok(())
}

async fn handle_star(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    parts: &[&str],
) -> anyhow::Result<()> {
    if parts.len() == 1 {
        let rows = ctx.store.star_list().await?;
        let mut s = String::from("收藏夹：.star add|set|del");
        for r in rows {
            s.push_str(&format!("\n{} : {}", r.name, r.url));
        }
        ctx.reply_text(msg, s).await?;
        return Ok(());
    }
    match parts[1] {
        "add" if parts.len() == 4 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if ctx
                .store
                .star_add(parts[2], parts[3], msg.user_id())
                .await?
            {
                ctx.reply_text(msg, "stared").await?;
            } else {
                ctx.reply_text(msg, format!("名称「{}」已存在", parts[2]))
                    .await?;
            }
        }
        "set" if parts.len() == 4 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            ctx.store
                .star_set(parts[2], parts[3], msg.user_id())
                .await?;
            ctx.reply_text(msg, "stared").await?;
        }
        "del" if parts.len() == 3 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if ctx.store.star_del(parts[2]).await? {
                ctx.reply_text(msg, format!("名称「{}」已删除", parts[2]))
                    .await?;
            } else {
                ctx.reply_text(msg, format!("名称「{}」不存在", parts[2]))
                    .await?;
            }
        }
        _ => {
            ctx.reply_text(msg, "格式：.star add|set|del").await?;
        }
    }
    Ok(())
}

async fn handle_tag(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    parts: &[&str],
) -> anyhow::Result<()> {
    if parts.len() < 2 || parts[1] == "list" {
        let list = ctx.store.tag_list().await?;
        if list.is_empty() {
            ctx.reply_text(msg, "还没有 tag").await?;
            return Ok(());
        }
        let mut s = String::from("tag：");
        for item in list {
            s.push_str(&format!(
                "\n{}  {}张  别名:{:?}",
                item.tag.name, item.image_count, item.aliases
            ));
        }
        ctx.reply_text(msg, s).await?;
        return Ok(());
    }
    if !can_mutate(ctx, msg.user_id()).await {
        ctx.reply_text(msg, "没有权限").await?;
        return Ok(());
    }
    match parts[1] {
        "add" if parts.len() == 3 => {
            ctx.store.tag_ensure(parts[2]).await?;
            ctx.reply_text(msg, "ok").await?;
        }
        "alias" if parts.len() == 4 => {
            let t = ctx.store.tag_alias(parts[2], parts[3]).await?;
            ctx.reply_text(msg, t).await?;
        }
        "merge" if parts.len() == 4 => {
            let t = ctx.store.tag_merge(parts[2], parts[3]).await?;
            ctx.reply_text(msg, t).await?;
        }
        "retire" | "del" if parts.len() == 3 => {
            let ok = ctx.store.tag_retire(parts[2]).await?;
            ctx.reply_text(
                msg,
                if ok {
                    "已去掉这个 tag（图片文件还在）"
                } else {
                    "不存在"
                },
            )
            .await?;
        }
        _ => {
            ctx.reply_text(msg, "格式：.tag list|add|alias|merge|retire")
                .await?;
        }
    }
    Ok(())
}

async fn handle_image_cmd(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    line: &str,
) -> anyhow::Result<()> {
    if !can_mutate(ctx, msg.user_id()).await {
        ctx.reply_text(msg, "没有权限").await?;
        return Ok(());
    }
    let add = line.starts_with(".添加");
    let name: String = line.chars().skip(3).collect();
    if name.is_empty() {
        ctx.reply_text(msg, "格式：回复图片后发 .添加名 / .删除名")
            .await?;
        return Ok(());
    }
    let Some(bytes) = reply_image_bytes(ctx, msg).await? else {
        return Ok(());
    };
    if add {
        let img = ctx.store.image_upsert(&bytes, "jpg", msg.user_id()).await?;
        let text = match ctx
            .store
            .image_attach(&img.md5, &name, msg.user_id())
            .await?
        {
            AttachResult::Attached => format!("已挂上 tag「{name}」🤫"),
            AttachResult::Already => format!("这张图已有 tag「{name}」"),
        };
        ctx.reply_text(msg, text).await?;
    } else {
        let md5 = fujiang_store::md5_hex(&bytes);
        let text = match ctx.store.image_detach(&md5, &name).await? {
            DetachResult::Detached => format!("已从这张图去掉 tag「{name}」"),
            DetachResult::NoSuchTag => format!("这张图没有 tag「{name}」"),
            DetachResult::UnknownImage => "图库里没有这张图".into(),
        };
        ctx.reply_text(msg, text).await?;
    }
    Ok(())
}

async fn handle_show_tags(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
) -> anyhow::Result<()> {
    let Some(md5) = reply_image_md5(ctx, msg).await? else {
        return Ok(());
    };
    let tags = ctx.store.image_tags(&md5).await?;
    if tags.is_empty() {
        ctx.reply_text(msg, "这张图还没有 tag").await?;
    } else {
        ctx.reply_text(msg, format!("这张图的 tag：{}", tags.join("、")))
            .await?;
    }
    Ok(())
}

async fn reply_image_md5(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
) -> anyhow::Result<Option<String>> {
    Ok(reply_image_bytes(ctx, msg)
        .await?
        .map(|b| fujiang_store::md5_hex(&b)))
}

async fn reply_image_bytes(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(rid) = msg.reply_id() else {
        ctx.reply_text(msg, "请先回复一张图片").await?;
        return Ok(None);
    };
    let quoted = ctx.messenger().await.get_message(rid).await?;
    let Some(media) = quoted.first_image() else {
        ctx.reply_text(msg, "回复的不是图片").await?;
        return Ok(None);
    };
    match ctx.fetch_media_bytes(media).await {
        Ok(b) => Ok(Some(b)),
        Err(e) => {
            warn!(error = %e, "fetch image");
            ctx.reply_text(msg, "ERROR😪").await?;
            Ok(None)
        }
    }
}
