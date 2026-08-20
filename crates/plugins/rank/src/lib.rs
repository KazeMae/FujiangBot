use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use fujiang_core::{BotContext, Event, Flow, Plugin, PluginScope};
use fujiang_store::CfUser;
use serde::Deserialize;
use tracing::{info, warn};

const HELP: &str = "Codeforces 排行。`.rank` 和 `.rk` 一样。`{year*}` 可写多个年级，不写则看全部。后台会定时刷 rating 和近期比赛排名。\n\
.rank / .rk\n\
.rank -h / --help                 本说明\n\
.rank -a / --add <年级> <姓名> <handle...>\n\
                                  添加一人或多人（handle 是 CF 用户名）\n\
.rank -r / --remove <handle...>   按 handle 删除\n\
.rank -l / --list [年级...]       列出账号\n\
.rank -s / --show [年级...]       当前 rating\n\
.rank -m / --max [年级...]        历史最高分\n\
.rank -vr / --validRating [年级...]  有效分\n\
.rank -c / --count [年级...]      过题数\n\
.rank -lc / --lastCount [年级...] 近 30 天过题\n\
.rank -t / --totalLife <年级> <姓名>\n\
                                  该同学生涯摘要\n\
.rank -cs / --contestStandings <contest_id>\n\
                                  这场比赛里已登记同学的分数变化\n\
.rank -urk / --updateRank         立刻后台刷新比赛排名\n\
.rank -urt / --updateRating       立刻后台刷新 rating\n\
.rank -u / --updateTime           上次刷新时间";

#[derive(Default)]
pub struct RankPlugin;

#[async_trait]
impl Plugin for RankPlugin {
    fn meta(&self) -> fujiang_core::PluginMeta {
        fujiang_core::PluginMeta::new("rank", "Codeforces 排行与生涯", self.commands())
    }

    fn name(&self) -> &'static str {
        "rank"
    }

    fn help(&self) -> &'static str {
        HELP
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".rank", ".rk"]
    }

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        let ctx = ctx.clone();
        let stop = scope.stop_token();
        scope.spawn(async move {
            loop {
                if stop.is_cancelled() {
                    break;
                }
                if let Err(e) = refresh_ratings(&ctx).await {
                    warn!(error = %e, "rating refresh");
                }
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                }
                if stop.is_cancelled() {
                    break;
                }
                if let Err(e) = refresh_ranks(&ctx).await {
                    warn!(error = %e, "rank refresh");
                }
                let minutes = ctx.bot_config().await.rank_update_minutes.max(10);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(minutes * 60)) => {}
                }
            }
        });
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let line = msg.command_line();
        let mut args: Vec<&str> = line.split_whitespace().collect();
        if args.first().copied() != Some(".rank") && args.first().copied() != Some(".rk") {
            return Ok(Flow::Continue);
        }
        args.remove(0);
        if args.is_empty() {
            ctx.reply_text(msg, HELP).await?;
            return Ok(Flow::Stop);
        }
        let rest: Vec<String> = args.iter().skip(1).map(|s| s.to_string()).collect();
        match args[0] {
            "--help" | "-h" => {
                ctx.reply_text(msg, HELP).await?;
            }
            "--add" | "-a" => add_user(ctx, msg, &rest).await?,
            "--remove" | "-r" => remove_user(ctx, msg, &rest).await?,
            "--list" | "-l" => list_users(ctx, msg, &rest).await?,
            "--show" | "-s" => query(ctx, msg, &rest, "last_rating", true).await?,
            "--max" | "-m" | "--maxRating" => query(ctx, msg, &rest, "max_rating", true).await?,
            "--validRating" | "-vr" => query(ctx, msg, &rest, "valid_rating", true).await?,
            "--count" | "-c" => query(ctx, msg, &rest, "solved", false).await?,
            "--lastCount" | "-lc" => query(ctx, msg, &rest, "last_month", false).await?,
            "--totalLife" | "-t" => total_life(ctx, msg, &rest).await?,
            "--contestStandings" | "-cs" => standings(ctx, msg, &rest).await?,
            "--updateRank" | "-urk" => {
                ctx.reply_text(msg, "已放入后台查询").await?;
                let bg = ctx.clone();
                scope.spawn(async move {
                    let _ = refresh_ranks(&bg).await;
                });
            }
            "--updateRating" | "-urt" => {
                ctx.reply_text(msg, "已放入后台查询").await?;
                let bg = ctx.clone();
                scope.spawn(async move {
                    let _ = refresh_ratings(&bg).await;
                });
            }
            "--updateTime" | "-u" => {
                let a = ctx
                    .store
                    .plugin_get("rank", "rank_rating_at")
                    .await?
                    .unwrap_or_else(|| "-".into());
                let b = ctx
                    .store
                    .plugin_get("rank", "rank_standings_at")
                    .await?
                    .unwrap_or_else(|| "-".into());
                ctx.reply_text(
                    msg,
                    format!("上次更新用户 rating：{a}\n上次更新比赛排名：{b}"),
                )
                .await?;
            }
            _ => {
                ctx.reply_text(msg, HELP).await?;
            }
        }
        Ok(Flow::Stop)
    }
}

async fn users_filtered(ctx: &BotContext, years: &[String]) -> anyhow::Result<Vec<CfUser>> {
    if years.is_empty() {
        return ctx.store.cf_users().await;
    }
    let mut out = Vec::new();
    for y in years {
        let year: i64 = y.parse()?;
        out.extend(ctx.store.cf_users_by_year(year).await?);
    }
    Ok(out)
}

async fn add_user(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
) -> anyhow::Result<()> {
    if args.len() < 3 {
        ctx.reply_text(msg, "格式错误😵").await?;
        return Ok(());
    }
    let year: i64 = match args[0].parse() {
        Ok(y) => y,
        Err(_) => {
            ctx.reply_text(msg, "格式错误😵").await?;
            return Ok(());
        }
    };
    let name = &args[1];
    let mut ok = Vec::new();
    let mut exist = Vec::new();
    let mut err = Vec::new();
    for h in &args[2..] {
        if ctx.store.handle_exists(h).await? {
            exist.push(h.clone());
            continue;
        }
        match fetch_info(ctx, h).await {
            Ok((last, max)) => {
                ctx.store
                    .upsert_cf_user(&CfUser {
                        year,
                        name: name.clone(),
                        handle: h.clone(),
                        last_rating: last,
                        max_rating: max,
                        updated_at: chrono::Utc::now().timestamp(),
                        ..CfUser::default()
                    })
                    .await?;
                ok.push(h.clone());
            }
            Err(_) => err.push(h.clone()),
        }
    }
    let mut s = String::new();
    if !ok.is_empty() {
        s.push_str(&format!("用户{ok:?}已添加。\n"));
    }
    if !exist.is_empty() {
        s.push_str(&format!("用户{exist:?}已经存在。\n"));
    }
    if !err.is_empty() {
        s.push_str(&format!("用户{err:?}不存在。\n"));
    }
    ctx.reply_text(msg, s).await?;
    Ok(())
}

async fn remove_user(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
) -> anyhow::Result<()> {
    let mut removed = Vec::new();
    for h in args {
        if ctx.store.delete_cf_user(h).await? {
            removed.push(h.clone());
        }
    }
    ctx.reply_text(msg, format!("用户{removed:?}已删除。"))
        .await?;
    Ok(())
}

async fn list_users(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
) -> anyhow::Result<()> {
    let users = match users_filtered(ctx, args).await {
        Ok(u) => u,
        Err(_) => {
            ctx.reply_text(msg, "格式错误😵").await?;
            return Ok(());
        }
    };
    if users.is_empty() {
        ctx.reply_text(msg, "没有找到信息").await?;
        return Ok(());
    }
    let mut s = String::new();
    for u in users {
        s.push_str(&format!("{}{} {}\n", u.year, u.name, u.handle));
    }
    ctx.reply_text(msg, s.trim_end()).await?;
    Ok(())
}

async fn query(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
    field: &str,
    ismax: bool,
) -> anyhow::Result<()> {
    let users = match users_filtered(ctx, args).await {
        Ok(u) => u,
        Err(_) => {
            ctx.reply_text(msg, "格式错误😵").await?;
            return Ok(());
        }
    };
    if users.is_empty() {
        ctx.reply_text(msg, "没有查询的用户。").await?;
        return Ok(());
    }
    let mut rank: HashMap<String, i64> = HashMap::new();
    for u in users {
        let key = format!("{}{}", u.year, u.name);
        let val = match field {
            "last_rating" => u.last_rating,
            "max_rating" => u.max_rating,
            "valid_rating" => u.valid_rating,
            "solved" => u.solved,
            "last_month" => u.last_month,
            _ => 0,
        };
        rank.entry(key)
            .and_modify(|e| {
                if ismax {
                    *e = (*e).max(val);
                } else {
                    *e += val;
                }
            })
            .or_insert(val);
    }
    let mut items: Vec<_> = rank.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    let mut s = format!("大家要好好努力哦({field})：");
    for (k, v) in items {
        s.push_str(&format!("\n{k}  {v}"));
    }
    ctx.reply_text(msg, s).await?;
    Ok(())
}

async fn standings(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
) -> anyhow::Result<()> {
    let contest_id = if args.is_empty() {
        latest_finished_contest(ctx).await?
    } else {
        Some(args[0].clone())
    };
    let Some(cid) = contest_id else {
        ctx.reply_text(msg, "未找到最近一场比赛").await?;
        return Ok(());
    };
    let rows = ctx.store.standings(&cid).await?;
    if rows.is_empty() {
        ctx.reply_text(msg, format!("比赛{cid}好像没有人打过欸"))
            .await?;
        return Ok(());
    }
    let mut s = format!("比赛{cid}的榜单如下:\n姓名 id 排名 分数变化");
    for r in rows {
        let d = r.new_rating - r.old_rating;
        s.push_str(&format!(
            "\n{} {} {}{d}",
            r.label,
            r.rank,
            if d >= 0 { "+" } else { "" }
        ));
    }
    ctx.reply_text(msg, s).await?;
    Ok(())
}

async fn latest_finished_contest(ctx: &BotContext) -> anyhow::Result<Option<String>> {
    let body: CfResp<Vec<CfContest>> = ctx
        .http
        .get("https://codeforces.com/api/contest.list?gym=false")
        .send()
        .await?
        .json()
        .await?;
    Ok(body
        .result
        .into_iter()
        .find(|c| c.phase == "FINISHED")
        .map(|c| c.id.to_string()))
}

async fn total_life(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    args: &[String],
) -> anyhow::Result<()> {
    if args.len() != 2 {
        ctx.reply_text(msg, "格式错误😵").await?;
        return Ok(());
    }
    let year: i64 = match args[0].parse() {
        Ok(y) => y,
        Err(_) => {
            ctx.reply_text(msg, "格式错误😵").await?;
            return Ok(());
        }
    };
    let users = ctx.store.cf_users_by_name(year, &args[1]).await?;
    if users.is_empty() {
        ctx.reply_text(msg, "没有查询的用户。").await?;
        return Ok(());
    }
    let mut all = Vec::new();
    for u in &users {
        match fetch_status(ctx, &u.handle).await {
            Ok(v) => all.extend(v),
            Err(e) => {
                warn!(error = %e, handle = %u.handle, "status");
                ctx.reply_text(msg, "格式错误😵").await?;
                return Ok(());
            }
        }
    }
    ctx.reply_text(msg, summarize(&args[1], &users, &all))
        .await?;
    Ok(())
}

fn summarize(name: &str, users: &[CfUser], data: &[CfSub]) -> String {
    let mut ac: HashMap<String, i64> = HashMap::new();
    let mut scores = Vec::new();
    let mut submits = Vec::new();
    for sub in data {
        let t = sub.creation_time_seconds + 8 * 3600;
        submits.push(t);
        if sub.verdict.as_deref() != Some("OK") {
            continue;
        }
        let key = match (sub.problem.contest_id, sub.problem.problemset_name.as_ref()) {
            (Some(cid), _) => format!("{cid}{}", sub.problem.index),
            (_, Some(n)) => format!("{n}{}", sub.problem.index),
            _ => continue,
        };
        ac.entry(key).and_modify(|e| *e = (*e).min(t)).or_insert(t);
        if let Some(r) = sub.problem.rating {
            scores.push(r);
        }
    }
    if data.is_empty() || ac.is_empty() {
        return "你还没有提交记录或者AC记录哦。".into();
    }
    scores.sort();
    submits.sort();
    let start = submits[0];
    let end = *submits.last().unwrap();
    let now = chrono::Utc::now().timestamp();
    let mut day_acs: HashMap<String, i64> = HashMap::new();
    let mut month_acs: HashMap<String, i64> = HashMap::new();
    let mut days = Vec::new();
    for t in ac.values() {
        let dt = Utc.timestamp_opt(*t, 0).single().unwrap_or(Utc::now());
        let day = dt.format("%Y-%m-%d").to_string();
        let month = dt.format("%Y-%m").to_string();
        *day_acs.entry(day.clone()).or_insert(0) += 1;
        *month_acs.entry(month).or_insert(0) += 1;
        days.push(day);
    }
    days.sort();
    days.dedup();
    let mut max_gap = 0i64;
    let mut gap_a = days.first().cloned().unwrap_or_default();
    let mut gap_b = gap_a.clone();
    for w in days.windows(2) {
        if let (Ok(a), Ok(b)) = (
            chrono::NaiveDate::parse_from_str(&w[0], "%Y-%m-%d"),
            chrono::NaiveDate::parse_from_str(&w[1], "%Y-%m-%d"),
        ) {
            let d = (b - a).num_days();
            if d > max_gap {
                max_gap = d;
                gap_a = w[0].clone();
                gap_b = w[1].clone();
            }
        }
    }
    let max_day = day_acs.values().copied().max().unwrap_or(0);
    let max_month = month_acs.values().copied().max().unwrap_or(0);
    let max_days: Vec<_> = day_acs
        .iter()
        .filter(|(_, v)| **v == max_day)
        .map(|(k, _)| k.as_str())
        .take(3)
        .collect();
    let max_months: Vec<_> = month_acs
        .iter()
        .filter(|(_, v)| **v == max_month)
        .map(|(k, _)| k.as_str())
        .take(3)
        .collect();

    let handles: Vec<_> = users.iter().map(|u| u.handle.as_str()).collect();
    format!(
        "{name},这是你的CF生涯：\n你一共有{}个CF帐号：{}。\n你一共提交了{}次，AC了{}题。\n其中AC的题的最高分是{}分。\n你第一次提交的时间是{} ,距今已经{}天。\n你最后一次提交的时间是{} ,距今已经{}天。\n你一共在CF上写了{}天题。\n你AC题目最多的日子是：{},每天AC了{max_day}题。\n你AC题目最多的月份是：{},每月AC了{max_month}题。\n你一共有{}天在A题，{}天没A题。\n你最长没A题的一段时间是{gap_a}到{gap_b}，共计{max_gap}天。",
        users.len(),
        handles.join(" , "),
        data.len(),
        ac.len(),
        scores.last().copied().unwrap_or(0),
        fmt(start),
        (now - start) / 86400,
        fmt(end),
        (now - end) / 86400,
        (end - start) / 86400,
        max_days.join(","),
        max_months.join(","),
        day_acs.len(),
        ((end - start) / 86400) - day_acs.len() as i64
    )
}

fn fmt(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

async fn refresh_ratings(ctx: &BotContext) -> anyhow::Result<()> {
    info!("refresh cf ratings");
    let users = ctx.store.cf_users().await?;
    for mut u in users {
        match fetch_full(ctx, &u).await {
            Ok(nu) => {
                ctx.store.upsert_cf_user(&nu).await?;
            }
            Err(e) => warn!(error = %e, handle = %u.handle, "rating"),
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _ = &mut u;
    }
    ctx.store
        .plugin_set(
            "rank",
            "rank_rating_at",
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        )
        .await?;
    Ok(())
}

async fn refresh_ranks(ctx: &BotContext) -> anyhow::Result<()> {
    info!("refresh cf standings");
    let users = ctx.store.cf_users().await?;
    for u in users {
        if let Ok(hist) = fetch_rating_hist(ctx, &u.handle).await {
            let valid = valid_rating(&hist);
            let mut nu = u.clone();
            nu.valid_rating = valid;
            ctx.store.upsert_cf_user(&nu).await?;
            let mut rows = Vec::with_capacity(hist.len());
            for h in hist {
                rows.push(fujiang_store::StandingRow {
                    contest_id: h.contest_id.to_string(),
                    handle: u.handle.clone(),
                    label: format!("{}{} {}", u.year, u.name, u.handle),
                    rank: h.rank,
                    old_rating: h.old_rating,
                    new_rating: h.new_rating,
                });
            }
            ctx.store.upsert_standings(&rows).await?;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ctx.store
        .plugin_set(
            "rank",
            "rank_standings_at",
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        )
        .await?;
    Ok(())
}

fn valid_rating(hist: &[CfRating]) -> i64 {
    if hist.len() < 8 {
        return 0;
    }
    let now = chrono::Utc::now().timestamp();
    let last8 = &hist[hist.len() - 8..];
    let coef = [5.0, 5.0, 10.0, 10.0, 10.0, 10.0, 20.0, 30.0];
    let mut sum = 0.0;
    for (i, h) in last8.iter().enumerate() {
        if now - h.rating_update_time_seconds > 60 * 60 * 24 * 31 * 3 {
            return 0;
        }
        sum += h.new_rating as f64 * coef[i] / 100.0;
    }
    (sum + 0.5) as i64
}

async fn fetch_info(ctx: &BotContext, handle: &str) -> anyhow::Result<(i64, i64)> {
    let url =
        format!("https://codeforces.com/api/user.info?handles={handle}&checkHistoricHandles=true");
    let body: CfResp<Vec<CfInfo>> = ctx.http.get(url).send().await?.json().await?;
    let u = body
        .result
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty"))?;
    Ok((u.rating.unwrap_or(0), u.max_rating.unwrap_or(0)))
}

async fn fetch_full(ctx: &BotContext, u: &CfUser) -> anyhow::Result<CfUser> {
    let (last, max) = fetch_info(ctx, &u.handle).await?;
    let (solved, last_month) = solved_info(ctx, &u.handle).await.unwrap_or((0, 0));
    let mut n = u.clone();
    n.last_rating = last;
    n.max_rating = max;
    n.solved = solved;
    n.last_month = last_month;
    n.updated_at = chrono::Utc::now().timestamp();
    Ok(n)
}

async fn solved_info(ctx: &BotContext, handle: &str) -> anyhow::Result<(i64, i64)> {
    let mut ac = HashSet::new();
    let mut last_month = 0i64;
    let mut page = 1i64;
    let now = chrono::Utc::now().timestamp();
    loop {
        let url = format!(
            "https://codeforces.com/api/user.status?handle={handle}&from={}&count=10000",
            (page - 1) * 10000 + 1
        );
        let body: CfResp<Vec<CfSub>> = ctx.http.get(url).send().await?.json().await?;
        let n = body.result.len();
        for s in body.result {
            if s.verdict.as_deref() == Some("OK") {
                let title = match s.problem.contest_id {
                    Some(cid) => format!("{cid}{}", s.problem.index),
                    None => format!(
                        "{}{}",
                        s.problem.problemset_name.unwrap_or_default(),
                        s.problem.index
                    ),
                };
                ac.insert(title);
                if now - s.creation_time_seconds <= 30 * 24 * 3600 {
                    last_month += 1;
                }
            }
        }
        if n < 10000 {
            break;
        }
        page += 1;
    }
    Ok((ac.len() as i64, last_month))
}

async fn fetch_status(ctx: &BotContext, handle: &str) -> anyhow::Result<Vec<CfSub>> {
    let url = format!("https://codeforces.com/api/user.status?handle={handle}");
    let body: CfResp<Vec<CfSub>> = ctx.http.get(url).send().await?.json().await?;
    Ok(body.result)
}

async fn fetch_rating_hist(ctx: &BotContext, handle: &str) -> anyhow::Result<Vec<CfRating>> {
    let url = format!("https://codeforces.com/api/user.rating?handle={handle}");
    let body: CfResp<Vec<CfRating>> = ctx.http.get(url).send().await?.json().await?;
    Ok(body.result)
}

#[derive(Deserialize)]
struct CfResp<T> {
    #[serde(default)]
    result: T,
}

#[derive(Deserialize)]
struct CfInfo {
    rating: Option<i64>,
    #[serde(rename = "maxRating")]
    max_rating: Option<i64>,
}

#[derive(Deserialize)]
struct CfContest {
    id: i64,
    phase: String,
}

#[derive(Deserialize, Clone)]
struct CfSub {
    #[serde(rename = "creationTimeSeconds")]
    creation_time_seconds: i64,
    verdict: Option<String>,
    problem: CfProb,
}

#[derive(Deserialize, Clone)]
struct CfProb {
    #[serde(rename = "contestId")]
    contest_id: Option<i64>,
    #[serde(default)]
    index: String,
    rating: Option<i64>,
    #[serde(rename = "problemsetName")]
    problemset_name: Option<String>,
}

#[derive(Deserialize)]
struct CfRating {
    #[serde(rename = "contestId")]
    contest_id: i64,
    rank: i64,
    #[serde(rename = "oldRating")]
    old_rating: i64,
    #[serde(rename = "newRating")]
    new_rating: i64,
    #[serde(rename = "ratingUpdateTimeSeconds")]
    rating_update_time_seconds: i64,
}
