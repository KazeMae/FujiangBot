use std::collections::HashMap;
use std::sync::Arc;

use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::{
    CfUser, ContestRow, DailyProblem, ImageRow, LearnReply, RemindGroup, StandingRow, Star, Store,
    Tag,
};

const SETTINGS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
)
"#;

impl Store {
    pub(crate) async fn contest_pool(&self) -> anyhow::Result<SqlitePool> {
        self.ensure_plugin("contest").await
    }

    pub(crate) async fn rank_pool(&self) -> anyhow::Result<SqlitePool> {
        self.ensure_plugin("rank").await
    }

    pub(crate) async fn problem_pool(&self) -> anyhow::Result<SqlitePool> {
        self.ensure_plugin("problem").await
    }

    pub(crate) async fn fun_pool(&self) -> anyhow::Result<SqlitePool> {
        self.ensure_plugin("fun").await
    }

    /// Cached pool for a built-in plugin, with that plugin's schema applied.
    pub async fn ensure_plugin(&self, name: &str) -> anyhow::Result<SqlitePool> {
        {
            let g = self.plugin_pools.lock().await;
            if let Some(p) = g.get(name) {
                return Ok(p.clone());
            }
        }
        let pool = self.open_plugin_db(name).await?;
        init_schema(name, &pool).await?;
        let mut g = self.plugin_pools.lock().await;
        Ok(g.entry(name.to_string()).or_insert(pool).clone())
    }

    pub async fn plugin_get(&self, plugin: &str, key: &str) -> anyhow::Result<Option<String>> {
        let pool = self.ensure_plugin(plugin).await?;
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    pub async fn plugin_set(&self, plugin: &str, key: &str, value: &str) -> anyhow::Result<()> {
        let pool = self.ensure_plugin(plugin).await?;
        sqlx::query(
            "INSERT INTO settings(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&pool)
        .await?;
        Ok(())
    }

    /// Move built-in plugin tables out of `fujiang.db` into `plugin-data/<name>/plugin.sqlite`.
    pub(crate) async fn split_plugin_tables(&self) -> anyhow::Result<()> {
        let mut moved = Vec::new();
        moved.extend(self.split_contest().await?);
        moved.extend(self.split_rank().await?);
        moved.extend(self.split_problem().await?);
        moved.extend(self.split_fun().await?);
        self.split_plugin_settings().await?;
        if !moved.is_empty() {
            info!(tables = ?moved, "moved plugin tables out of host db");
        }
        Ok(())
    }

    async fn split_contest(&self) -> anyhow::Result<Vec<&'static str>> {
        let tables = ["contests", "remind_groups"];
        if !any_host_table(&self.pool, &tables).await? {
            return Ok(vec![]);
        }
        if count_any(&self.pool, &tables).await? > 0 {
            let dest = self.contest_pool().await?;
            copy_contests(&self.pool, &dest).await?;
            copy_reminds(&self.pool, &dest).await?;
        }
        drop_host(&self.pool, &tables).await?;
        Ok(Vec::from(tables))
    }

    async fn split_rank(&self) -> anyhow::Result<Vec<&'static str>> {
        let tables = ["contest_standings", "cf_users"];
        if !any_host_table(&self.pool, &["cf_users", "contest_standings"]).await? {
            return Ok(vec![]);
        }
        if count_any(&self.pool, &["cf_users", "contest_standings"]).await? > 0 {
            let dest = self.rank_pool().await?;
            copy_cf_users(&self.pool, &dest).await?;
            copy_standings(&self.pool, &dest).await?;
        }
        drop_host(&self.pool, &tables).await?;
        Ok(vec!["cf_users", "contest_standings"])
    }

    async fn split_problem(&self) -> anyhow::Result<Vec<&'static str>> {
        if !host_table(&self.pool, "daily_problems").await? {
            return Ok(vec![]);
        }
        if count_rows(&self.pool, "daily_problems").await? > 0 {
            let dest = self.problem_pool().await?;
            copy_daily(&self.pool, &dest).await?;
        }
        drop_host(&self.pool, &["daily_problems"]).await?;
        Ok(vec!["daily_problems"])
    }

    async fn split_fun(&self) -> anyhow::Result<Vec<&'static str>> {
        let tables = [
            "learn_replies",
            "stars",
            "images",
            "tags",
            "tag_aliases",
            "image_tags",
        ];
        if !any_host_table(&self.pool, &tables).await? {
            return Ok(vec![]);
        }
        if count_any(&self.pool, &tables).await? > 0 {
            let dest = self.fun_pool().await?;
            copy_learn(&self.pool, &dest).await?;
            copy_stars(&self.pool, &dest).await?;
            copy_tags(&self.pool, &dest).await?;
            copy_images(&self.pool, &dest).await?;
            copy_tag_aliases(&self.pool, &dest).await?;
            copy_image_tags(&self.pool, &dest).await?;
        }
        drop_host(
            &self.pool,
            &[
                "image_tags",
                "tag_aliases",
                "images",
                "tags",
                "learn_replies",
                "stars",
            ],
        )
        .await?;
        Ok(Vec::from(tables))
    }

    async fn split_plugin_settings(&self) -> anyhow::Result<()> {
        if !host_table(&self.pool, "settings").await? {
            return Ok(());
        }
        let rows = sqlx::query("SELECT key, value FROM settings")
            .fetch_all(&self.pool)
            .await?;
        let mut delete = Vec::new();
        for r in rows {
            let key: String = r.get(0);
            let value: String = r.get(1);
            let plugin = if key.starts_with("rank_") {
                Some("rank")
            } else if key.starts_with("contest_") || key.starts_with("preremind:") {
                Some("contest")
            } else {
                None
            };
            if let Some(p) = plugin {
                self.plugin_set(p, &key, &value).await?;
                delete.push(key);
            }
        }
        for k in delete {
            sqlx::query("DELETE FROM settings WHERE key = ?")
                .bind(k)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

fn empty_pools() -> Arc<tokio::sync::Mutex<HashMap<String, SqlitePool>>> {
    Arc::new(tokio::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn new_plugin_pools() -> Arc<tokio::sync::Mutex<HashMap<String, SqlitePool>>> {
    empty_pools()
}

async fn init_schema(name: &str, pool: &SqlitePool) -> anyhow::Result<()> {
    match name {
        "contest" => {
            exec(pool, SETTINGS_DDL).await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS contests (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    oj TEXT NOT NULL,
                    title TEXT NOT NULL,
                    begin_ts INTEGER NOT NULL,
                    end_ts INTEGER NOT NULL,
                    url TEXT NOT NULL,
                    source TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL
                )"#,
            )
            .await?;
            exec(
                pool,
                "CREATE INDEX IF NOT EXISTS idx_contests_oj ON contests(oj)",
            )
            .await?;
            exec(
                pool,
                "CREATE INDEX IF NOT EXISTS idx_contests_begin ON contests(begin_ts)",
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS remind_groups (
                    group_id INTEGER PRIMARY KEY,
                    hour INTEGER NOT NULL,
                    min INTEGER NOT NULL
                )"#,
            )
            .await?;
        }
        "rank" => {
            exec(pool, SETTINGS_DDL).await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS cf_users (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    year INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    handle TEXT NOT NULL UNIQUE,
                    last_rating INTEGER NOT NULL DEFAULT 0,
                    max_rating INTEGER NOT NULL DEFAULT 0,
                    solved INTEGER NOT NULL DEFAULT 0,
                    last_month INTEGER NOT NULL DEFAULT 0,
                    valid_rating INTEGER NOT NULL DEFAULT 0,
                    is_main INTEGER NOT NULL DEFAULT 0,
                    updated_at INTEGER NOT NULL DEFAULT 0
                )"#,
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS contest_standings (
                    contest_id TEXT NOT NULL,
                    handle TEXT NOT NULL,
                    label TEXT NOT NULL,
                    rank INTEGER NOT NULL,
                    old_rating INTEGER NOT NULL,
                    new_rating INTEGER NOT NULL,
                    PRIMARY KEY (contest_id, handle)
                )"#,
            )
            .await?;
        }
        "problem" => {
            exec(pool, SETTINGS_DDL).await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS daily_problems (
                    date TEXT NOT NULL,
                    band TEXT NOT NULL,
                    contest_id INTEGER NOT NULL,
                    idx TEXT NOT NULL,
                    url TEXT NOT NULL,
                    PRIMARY KEY (date, band)
                )"#,
            )
            .await?;
        }
        "fun" => {
            exec(pool, SETTINGS_DDL).await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS learn_replies (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    trigger TEXT NOT NULL,
                    reply TEXT NOT NULL,
                    created_by INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    group_id INTEGER
                )"#,
            )
            .await?;
            exec(
                pool,
                "CREATE INDEX IF NOT EXISTS idx_learn_trigger ON learn_replies(trigger)",
            )
            .await?;
            exec(
                pool,
                "CREATE INDEX IF NOT EXISTS idx_learn_group_trigger ON learn_replies(group_id, trigger)",
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS stars (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE,
                    url TEXT NOT NULL,
                    created_by INTEGER NOT NULL DEFAULT 0,
                    updated_at INTEGER NOT NULL
                )"#,
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS images (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    md5 TEXT NOT NULL UNIQUE,
                    rel_path TEXT NOT NULL,
                    added_by INTEGER NOT NULL DEFAULT 0,
                    added_at INTEGER NOT NULL
                )"#,
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS tags (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE
                )"#,
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS tag_aliases (
                    alias TEXT PRIMARY KEY,
                    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE
                )"#,
            )
            .await?;
            exec(
                pool,
                r#"CREATE TABLE IF NOT EXISTS image_tags (
                    image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
                    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                    added_by INTEGER NOT NULL DEFAULT 0,
                    added_at INTEGER NOT NULL,
                    PRIMARY KEY (image_id, tag_id)
                )"#,
            )
            .await?;
            exec(
                pool,
                "CREATE INDEX IF NOT EXISTS idx_image_tags_tag ON image_tags(tag_id)",
            )
            .await?;
        }
        _ => {
            exec(pool, SETTINGS_DDL).await?;
        }
    }
    Ok(())
}

async fn exec(pool: &SqlitePool, sql: &str) -> anyhow::Result<()> {
    sqlx::query(sql).execute(pool).await?;
    Ok(())
}

async fn host_table(pool: &SqlitePool, name: &str) -> anyhow::Result<bool> {
    let n: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(n.is_some())
}

async fn any_host_table(pool: &SqlitePool, names: &[&str]) -> anyhow::Result<bool> {
    for n in names {
        if host_table(pool, n).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn count_rows(pool: &SqlitePool, table: &str) -> anyhow::Result<i64> {
    if !host_table(pool, table).await? {
        return Ok(0);
    }
    Ok(sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await?)
}

async fn count_any(pool: &SqlitePool, tables: &[&str]) -> anyhow::Result<i64> {
    let mut n = 0i64;
    for t in tables {
        n += count_rows(pool, t).await?;
    }
    Ok(n)
}

async fn drop_host(pool: &SqlitePool, tables: &[&str]) -> anyhow::Result<()> {
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(pool)
        .await?;
    for t in tables {
        if host_table(pool, t).await? {
            sqlx::query(&format!("DROP TABLE {t}"))
                .execute(pool)
                .await?;
        }
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await?;
    Ok(())
}

async fn copy_contests(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "contests").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, ContestRow>(
        "SELECT id, oj, title, begin_ts, end_ts, url, source, updated_at FROM contests",
    )
    .fetch_all(from)
    .await?;
    let mut tx = to.begin().await?;
    for c in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO contests(id, oj, title, begin_ts, end_ts, url, source, updated_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(c.id)
        .bind(&c.oj)
        .bind(&c.title)
        .bind(c.begin_ts)
        .bind(c.end_ts)
        .bind(&c.url)
        .bind(&c.source)
        .bind(c.updated_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_reminds(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "remind_groups").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, RemindGroup>("SELECT group_id, hour, min FROM remind_groups")
        .fetch_all(from)
        .await?;
    let mut tx = to.begin().await?;
    for r in rows {
        sqlx::query("INSERT OR IGNORE INTO remind_groups(group_id, hour, min) VALUES(?, ?, ?)")
            .bind(r.group_id)
            .bind(r.hour)
            .bind(r.min)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_cf_users(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "cf_users").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, CfUser>(
        "SELECT id, year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at
         FROM cf_users",
    )
    .fetch_all(from)
    .await?;
    let mut tx = to.begin().await?;
    for u in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO cf_users(id, year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(u.id)
        .bind(u.year)
        .bind(&u.name)
        .bind(&u.handle)
        .bind(u.last_rating)
        .bind(u.max_rating)
        .bind(u.solved)
        .bind(u.last_month)
        .bind(u.valid_rating)
        .bind(u.is_main)
        .bind(u.updated_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_standings(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "contest_standings").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, StandingRow>(
        "SELECT contest_id, handle, label, rank, old_rating, new_rating FROM contest_standings",
    )
    .fetch_all(from)
    .await?;
    let mut tx = to.begin().await?;
    for s in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO contest_standings(contest_id, handle, label, rank, old_rating, new_rating)
             VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(&s.contest_id)
        .bind(&s.handle)
        .bind(&s.label)
        .bind(s.rank)
        .bind(s.old_rating)
        .bind(s.new_rating)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_daily(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "daily_problems").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, DailyProblem>(
        "SELECT date, band, contest_id, idx, url FROM daily_problems",
    )
    .fetch_all(from)
    .await?;
    let mut tx = to.begin().await?;
    for p in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO daily_problems(date, band, contest_id, idx, url)
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(&p.date)
        .bind(&p.band)
        .bind(p.contest_id)
        .bind(&p.idx)
        .bind(&p.url)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_learn(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "learn_replies").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, LearnReply>(
        "SELECT id, trigger, reply, created_by, created_at, group_id FROM learn_replies",
    )
    .fetch_all(from)
    .await?;
    let mut tx = to.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO learn_replies(id, trigger, reply, created_by, created_at, group_id)
             VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(r.id)
        .bind(&r.trigger)
        .bind(&r.reply)
        .bind(r.created_by)
        .bind(r.created_at)
        .bind(r.group_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_stars(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "stars").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, Star>("SELECT id, name, url, created_by, updated_at FROM stars")
        .fetch_all(from)
        .await?;
    let mut tx = to.begin().await?;
    for s in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO stars(id, name, url, created_by, updated_at) VALUES(?, ?, ?, ?, ?)",
        )
        .bind(s.id)
        .bind(&s.name)
        .bind(&s.url)
        .bind(s.created_by)
        .bind(s.updated_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_tags(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "tags").await? {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, Tag>("SELECT id, name FROM tags")
        .fetch_all(from)
        .await?;
    let mut tx = to.begin().await?;
    for t in rows {
        sqlx::query("INSERT OR IGNORE INTO tags(id, name) VALUES(?, ?)")
            .bind(t.id)
            .bind(&t.name)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_images(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "images").await? {
        return Ok(());
    }
    let rows =
        sqlx::query_as::<_, ImageRow>("SELECT id, md5, rel_path, added_by, added_at FROM images")
            .fetch_all(from)
            .await?;
    let mut tx = to.begin().await?;
    for img in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO images(id, md5, rel_path, added_by, added_at) VALUES(?, ?, ?, ?, ?)",
        )
        .bind(img.id)
        .bind(&img.md5)
        .bind(&img.rel_path)
        .bind(img.added_by)
        .bind(img.added_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_tag_aliases(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "tag_aliases").await? {
        return Ok(());
    }
    let rows = sqlx::query("SELECT alias, tag_id FROM tag_aliases")
        .fetch_all(from)
        .await?;
    let mut tx = to.begin().await?;
    for r in rows {
        let alias: String = r.get(0);
        let tag_id: i64 = r.get(1);
        sqlx::query("INSERT OR IGNORE INTO tag_aliases(alias, tag_id) VALUES(?, ?)")
            .bind(alias)
            .bind(tag_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn copy_image_tags(from: &SqlitePool, to: &SqlitePool) -> anyhow::Result<()> {
    if !host_table(from, "image_tags").await? {
        return Ok(());
    }
    let rows = sqlx::query("SELECT image_id, tag_id, added_by, added_at FROM image_tags")
        .fetch_all(from)
        .await?;
    let mut tx = to.begin().await?;
    for r in rows {
        let image_id: i64 = r.get(0);
        let tag_id: i64 = r.get(1);
        let added_by: i64 = r.get(2);
        let added_at: i64 = r.get(3);
        sqlx::query(
            "INSERT OR IGNORE INTO image_tags(image_id, tag_id, added_by, added_at) VALUES(?, ?, ?, ?)",
        )
        .bind(image_id)
        .bind(tag_id)
        .bind(added_by)
        .bind(added_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
