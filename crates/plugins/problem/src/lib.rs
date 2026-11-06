use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Local, Timelike};
use fujiang_core::{command, BotConfig, BotContext, Event, Flow, Plugin, PluginScope, Service};
use fujiang_store::{DailyProblem, Store};
use rand::seq::SliceRandom;
use rand::{rngs::StdRng, SeedableRng};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

const TAGS: &str = "binary search\nbitmasks\nbrute force\nchinese remainder theorem\ncombinatorics\nconstructive algorithms\ndata structures\ndfs and similar\ndivide and conquer\ndp\ndsu\nexpression parsing\nfft\nflows\ngames\ngeometry\ngraph matchings\ngraphs\ngreedy\nhashing\nimplementation\ninteractive\nmath\nmatrices\nmeet-in-the-middle\nnumber theory\nprobabilities\nschedules\nshortest paths\nsortings\nstring suffix structures\nstrings\nternary search\ntrees\ntwo pointers\n*special problem\nnot-seen";

const BANDS: &[(&str, &str)] = &[
    ("800-1000", "800 1000 !*special new"),
    ("1000-1200", "1000 1200 !*special new"),
    ("1200-1400", "1200 1400 !*special new"),
    ("1400-1600", "1400 1600 !*special new"),
    ("1600-2000", "1600 2000 !*special new"),
];

#[derive(Default)]
pub struct ProblemPlugin;

#[async_trait]
impl Plugin for ProblemPlugin {
    fn meta(&self) -> fujiang_core::PluginMeta {
        fujiang_core::PluginMeta::new("problem", "CF 抽题与每日一题", self.commands())
    }

    fn name(&self) -> &'static str {
        "problem"
    }

    fn help(&self) -> &'static str {
        "从 Codeforces 题库抽题。\n\
.problem <L> <R> [tags...] [-rt]\n\
        L、R 是 rating 区间，必须是 800～3500 的整百，如 1200 1600\n\
        tags 可写多个，空格用下划线代替（binary_search）\n\
        前加 ! 表示不要这个 tag，如 !dp\n\
        new / !new 限制新/旧场次（contest_id ≥1000）\n\
        默认抽一道；末尾加 -rt 会换一种抽法（可重复试）\n\
.cftag  可用 tag 列表\n\
.每日一题  当天五档：800-1000 / 1000-1200 / 1200-1400 / 1400-1600 / 1600-2000\n\
        过了设定的重置小时（默认 4 点）才换新的一天"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".problem", ".cftag", ".每日一题"]
    }

    fn provides(&self) -> &'static [&'static str] {
        &["problem"]
    }

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        ctx.provide(
            scope,
            "problem",
            Arc::new(ProblemService {
                store: ctx.store.clone(),
                config: ctx.config.clone(),
            }),
        )?;
        Ok(())
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
        let prefix = ctx.bot_config().await.command_prefix;
        let line = msg.command_line();
        if command::matches_token(&line, ".cftag", &prefix) {
            ctx.reply_text(msg, TAGS).await?;
            return Ok(Flow::Stop);
        }
        if command::matches_token(&line, ".每日一题", &prefix) {
            let text = daily(ctx).await?;
            ctx.reply_text(msg, text).await?;
            return Ok(Flow::Stop);
        }
        if let Some(rest) = command::strip_token(&line, ".problem", &prefix) {
            if rest.is_empty() {
                ctx.reply_text(msg, command::plugin_help(self, &prefix))
                    .await?;
                return Ok(Flow::Stop);
            }
            let text = pick_problem(ctx, rest, true).await?;
            ctx.reply_text(msg, text).await?;
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }
}

fn daily_use_date(now: chrono::DateTime<Local>, reset_hour: u32) -> String {
    let date = if now.hour() < reset_hour {
        now.date_naive() - chrono::Duration::days(1)
    } else {
        now.date_naive()
    };
    date.format("%Y-%m-%d").to_string()
}

async fn daily(ctx: &BotContext) -> anyhow::Result<String> {
    let now = Local::now();
    let reset = ctx.bot_config().await.daily_reset_hour;
    let use_date = daily_use_date(now, reset);

    let existing = ctx.store.daily_for_date(&use_date).await?;
    if existing.len() >= BANDS.len() {
        return Ok(format_daily(&use_date, &existing));
    }

    let mut rows = Vec::new();
    for (band, spec) in BANDS {
        let url = pick_one(ctx, spec, false)
            .await?
            .unwrap_or_else(|| "没有找到对应的题目哦".into());
        let (contest_id, idx) = parse_url(&url);
        let row = DailyProblem {
            date: use_date.clone(),
            band: (*band).into(),
            contest_id,
            idx,
            url,
        };
        ctx.store.save_daily(&row).await?;
        rows.push(row);
    }
    Ok(format_daily(&use_date, &rows))
}

struct ProblemService {
    store: Store,
    config: Arc<RwLock<BotConfig>>,
}

fn daily_json(row: &DailyProblem) -> Value {
    json!({
        "date": row.date,
        "band": row.band,
        "contest_id": row.contest_id,
        "idx": row.idx,
        "url": row.url,
    })
}

#[async_trait]
impl Service for ProblemService {
    async fn call(&self, method: &str, _args: Value) -> anyhow::Result<Value> {
        match method {
            "today" => {
                let reset = self.config.read().await.daily_reset_hour;
                let date = daily_use_date(Local::now(), reset);
                let rows = self.store.daily_for_date(&date).await?;
                Ok(Value::Array(rows.iter().map(daily_json).collect()))
            }
            other => anyhow::bail!("problem 没有方法 `{other}`（today）"),
        }
    }
}

fn format_daily(date: &str, rows: &[DailyProblem]) -> String {
    let mut s = format!("{date}:");
    for r in rows {
        s.push_str(&format!("\n{}:{}", r.band, r.url));
    }
    s
}

async fn pick_problem(ctx: &BotContext, spec: &str, allow_rt: bool) -> anyhow::Result<String> {
    if spec.is_empty() {
        return Ok("用法：.problem <L> <R> [tags...] [-rt]".into());
    }
    match pick_one(ctx, spec, allow_rt).await? {
        Some(url) => Ok(format!("题目链接：{url}")),
        None => Ok("没有找到对应的题目哦".into()),
    }
}

async fn pick_one(ctx: &BotContext, spec: &str, allow_rt: bool) -> anyhow::Result<Option<String>> {
    let mut args: Vec<String> = spec
        .split_whitespace()
        .map(|s| s.replace('_', " "))
        .collect();
    let mut random_once = true;
    if args.last().map(|s| s.as_str()) == Some("-rt") {
        args.pop();
        random_once = false;
    }
    if !allow_rt {
        random_once = false;
    }
    if args.len() < 2 {
        return Ok(None);
    }
    let l: i64 = args[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("Rating 应该是 800 ~ 3500 的整百数"))?;
    let r: i64 = args[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("Rating 应该是 800 ~ 3500 的整百数"))?;
    if l % 100 != 0 || r % 100 != 0 || !(800..=3500).contains(&l) || !(800..=3500).contains(&r) {
        anyhow::bail!("Rating 应该是 800 ~ 3500 的整百数");
    }
    let tags = &args[2..];
    let mut new_flag = 0i8;
    let mut want: Vec<String> = Vec::new();
    let mut not: Vec<String> = Vec::new();
    for t in tags {
        if t == "new" {
            new_flag = 1;
        } else if t == "!new" {
            new_flag = -1;
        } else if t == "not-seen" {
            continue;
        } else if let Some(rest) = t.strip_prefix('!') {
            not.push(rest.to_string());
        } else {
            want.push(t.clone());
        }
    }

    let mut url = "https://codeforces.com/api/problemset.problems?tags=".to_string();
    for t in &want {
        url.push_str(t);
        url.push(';');
    }
    let body: CfProblems = ctx.http.get(url).send().await?.json().await?;
    let mut hits = Vec::new();
    for p in body.result.problems {
        let Some(rating) = p.rating else { continue };
        let Some(cid) = p.contest_id else { continue };
        if rating < l || rating > r {
            continue;
        }
        if new_flag == 1 && cid < 1000 {
            continue;
        }
        if new_flag == -1 && cid >= 1000 {
            continue;
        }
        if want.iter().any(|t| !p.tags.iter().any(|x| x == t)) {
            continue;
        }
        if not.iter().any(|t| p.tags.iter().any(|x| x == t)) {
            continue;
        }
        hits.push(p);
    }
    if hits.is_empty() {
        return Ok(None);
    }
    let mut rng = if random_once {
        StdRng::from_entropy()
    } else {
        let seed = Local::now().date_naive().format("%Y%m%d").to_string();
        let seed: u64 = seed.parse().unwrap_or(1);
        StdRng::seed_from_u64(seed)
    };
    let p = hits.choose(&mut rng).unwrap();
    Ok(Some(format!(
        "https://codeforces.com/problemset/problem/{}/{}",
        p.contest_id.unwrap_or(0),
        p.index
    )))
}

fn parse_url(url: &str) -> (i64, String) {
    // .../problem/1234/A
    let parts: Vec<&str> = url.rsplit('/').take(2).collect();
    if parts.len() == 2 {
        let idx = parts[0].to_string();
        let cid = parts[1].parse().unwrap_or(0);
        (cid, idx)
    } else {
        (0, String::new())
    }
}

#[derive(Debug, Deserialize)]
struct CfProblems {
    result: CfResult,
}

#[derive(Debug, Deserialize)]
struct CfResult {
    problems: Vec<CfProblem>,
}

#[derive(Debug, Deserialize, Clone)]
struct CfProblem {
    #[serde(rename = "contestId")]
    contest_id: Option<i64>,
    index: String,
    rating: Option<i64>,
    #[serde(default)]
    tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 6, hour, 0, 0).unwrap()
    }

    #[test]
    fn daily_date_rolls_back_before_the_reset_hour() {
        assert_eq!(daily_use_date(at(3), 4), "2026-11-05");
        assert_eq!(daily_use_date(at(4), 4), "2026-11-06");
        assert_eq!(daily_use_date(at(0), 0), "2026-11-06");
    }

    #[test]
    fn plugin_provides_problem() {
        assert_eq!(ProblemPlugin.provides(), &["problem"]);
    }

    fn test_bot(reset: u32) -> BotConfig {
        BotConfig {
            command_prefix: ".".into(),
            groups: Vec::new(),
            allow_private: true,
            clist_username: String::new(),
            clist_api_key: String::new(),
            clist_limit: 0,
            contest_update_minutes: 30,
            rank_update_minutes: 30,
            daily_reset_hour: reset,
            fun_allow_mutate: false,
            fun_admins: Vec::new(),
            fun_delete_files: false,
        }
    }

    async fn open_store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fujiang-problem-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("t.db").to_str().unwrap(), dir.join("img"))
            .await
            .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn today_returns_saved_rows_for_the_daily_date() {
        let (store, dir) = open_store().await;
        let today = Local::now().format("%Y-%m-%d").to_string();
        let row = DailyProblem {
            date: today.clone(),
            band: "800-1000".into(),
            contest_id: 1,
            idx: "A".into(),
            url: "https://codeforces.com/problemset/problem/1/A".into(),
        };
        store.save_daily(&row).await.unwrap();
        store
            .save_daily(&DailyProblem {
                date: "2000-01-01".into(),
                band: "800-1000".into(),
                contest_id: 2,
                idx: "B".into(),
                url: "https://codeforces.com/problemset/problem/2/B".into(),
            })
            .await
            .unwrap();
        let svc = ProblemService {
            store,
            config: Arc::new(RwLock::new(test_bot(0))),
        };
        let got = svc.call("today", json!({})).await.unwrap();
        assert_eq!(
            got,
            json!([{
                "date": today,
                "band": "800-1000",
                "contest_id": 1,
                "idx": "A",
                "url": "https://codeforces.com/problemset/problem/1/A"
            }])
        );
        let err = svc.call("list", json!({})).await.unwrap_err();
        assert_eq!(err.to_string(), "problem 没有方法 `list`（today）");
        drop(svc);
        let _ = std::fs::remove_dir_all(dir);
    }
}
