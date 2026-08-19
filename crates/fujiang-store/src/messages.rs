use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use tracing::{info, warn};

use crate::{MessageLog, Store, StoredMessage};

const DDL_COLS: &str = r#"
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL,
    time INTEGER NOT NULL,
    self_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    group_id INTEGER,
    nickname TEXT,
    card TEXT,
    raw_text TEXT NOT NULL,
    segments_json TEXT NOT NULL DEFAULT '[]',
    inserted_at INTEGER NOT NULL
"#;

#[derive(Debug, Default)]
pub struct ArchiveReport {
    pub moved: u64,
    pub files: Vec<String>,
}

pub fn shard_table(group_id: Option<i64>) -> anyhow::Result<String> {
    match group_id {
        None => Ok("msg_pm".into()),
        Some(id) if id > 0 => Ok(format!("msg_g{id}")),
        Some(id) => anyhow::bail!("invalid group_id {id}"),
    }
}

pub fn valid_shard(name: &str) -> bool {
    if name == "msg_pm" {
        return true;
    }
    name.strip_prefix("msg_g")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

impl Store {
    pub async fn insert_message(&self, m: &MessageLog) -> anyhow::Result<()> {
        let table = self.ensure_shard(m.group_id).await?;
        let now = chrono::Utc::now().timestamp();
        let sql = format!(
            "INSERT OR IGNORE INTO {table}(
                message_id, time, self_id, user_id, group_id,
                nickname, card, raw_text, segments_json, inserted_at
             ) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        sqlx::query(&sql)
            .bind(m.message_id)
            .bind(m.time)
            .bind(m.self_id)
            .bind(m.user_id)
            .bind(m.group_id)
            .bind(&m.nickname)
            .bind(&m.card)
            .bind(&m.raw_text)
            .bind(&m.segments_json)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn ensure_shard(&self, group_id: Option<i64>) -> anyhow::Result<String> {
        let table = shard_table(group_id)?;
        debug_assert!(valid_shard(&table));
        let create = format!("CREATE TABLE IF NOT EXISTS {table} ({DDL_COLS})");
        sqlx::query(&create).execute(&self.pool).await?;
        sqlx::query(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {table}_mid ON {table}(message_id)"
        ))
        .execute(&self.pool)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {table}_time ON {table}(time)"
        ))
        .execute(&self.pool)
        .await?;
        sqlx::query("INSERT OR IGNORE INTO message_shards(name, group_id) VALUES(?, ?)")
            .bind(&table)
            .bind(group_id)
            .execute(&self.pool)
            .await?;
        Ok(table)
    }

    pub async fn split_legacy_messages(&self) -> anyhow::Result<u64> {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
        )
        .fetch_optional(&self.pool)
        .await?;
        if exists.is_none() {
            return Ok(0);
        }
        let rows = sqlx::query_as::<_, StoredMessage>(
            "SELECT id, message_id, time, self_id, user_id, group_id,
                    nickname, card, raw_text, segments_json, inserted_at
             FROM messages",
        )
        .fetch_all(&self.pool)
        .await?;
        let n = rows.len() as u64;
        for row in &rows {
            self.insert_message(&MessageLog {
                message_id: row.message_id,
                time: row.time,
                self_id: row.self_id,
                user_id: row.user_id,
                group_id: row.group_id,
                nickname: row.nickname.clone(),
                card: row.card.clone(),
                raw_text: row.raw_text.clone(),
                segments_json: row.segments_json.clone(),
            })
            .await?;
        }
        sqlx::query("DROP TABLE messages")
            .execute(&self.pool)
            .await?;
        if n > 0 {
            info!(n, "split legacy messages into per-group tables");
        }
        Ok(n)
    }

    pub async fn shard_names(&self) -> anyhow::Result<Vec<(String, Option<i64>)>> {
        let rows = sqlx::query("SELECT name, group_id FROM message_shards")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.get(0);
                if !valid_shard(&name) {
                    warn!(name, "skip invalid shard name");
                    return None;
                }
                Some((name, r.get(1)))
            })
            .collect())
    }

    pub async fn count_in_shard(&self, table: &str) -> anyhow::Result<i64> {
        if !valid_shard(table) {
            anyhow::bail!("invalid shard {table}");
        }
        Ok(sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&self.pool)
            .await?)
    }

    pub fn set_archive_params(&self, after_days: u64, every_hours: u64) {
        self.archive_after_days.store(after_days, Ordering::Relaxed);
        self.archive_every_secs
            .store(every_hours.max(1).saturating_mul(3600), Ordering::Relaxed);
    }

    /// Move messages older than `archive_after_days` into monthly sqlite files.
    pub async fn archive_due(&self) -> anyhow::Result<ArchiveReport> {
        let days = self.archive_after_days.load(Ordering::Relaxed);
        if days == 0 {
            return Ok(ArchiveReport::default());
        }
        let cutoff = Utc::now().timestamp() - (days as i64) * 86400;
        tokio::fs::create_dir_all(&self.archive_dir).await.ok();
        let mut report = ArchiveReport::default();
        for (table, group_id) in self.shard_names().await? {
            let moved = self
                .archive_shard(&table, group_id, cutoff, &mut report)
                .await?;
            report.moved += moved;
        }
        if report.moved > 0 {
            info!(moved = report.moved, files = ?report.files, "archived messages");
            self.set_setting(
                "messages_archived_at",
                &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            )
            .await?;
        }
        Ok(report)
    }

    async fn archive_shard(
        &self,
        table: &str,
        group_id: Option<i64>,
        cutoff: i64,
        report: &mut ArchiveReport,
    ) -> anyhow::Result<u64> {
        let rows = sqlx::query_as::<_, StoredMessage>(&format!(
            "SELECT id, message_id, time, self_id, user_id, group_id,
                    nickname, card, raw_text, segments_json, inserted_at
             FROM {table} WHERE time < ? ORDER BY time"
        ))
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        let mut by_month: HashMap<String, Vec<&StoredMessage>> = HashMap::new();
        for row in &rows {
            by_month.entry(year_month(row.time)).or_default().push(row);
        }

        let mut ids: Vec<i64> = Vec::new();
        for (ym, batch) in by_month {
            let path = self.archive_dir.join(format!("{table}_{ym}.sqlite"));
            write_archive_file(&path, group_id, &batch).await?;
            let p = path.display().to_string();
            if !report.files.contains(&p) {
                report.files.push(p);
            }
            ids.extend(batch.iter().map(|r| r.id));
        }

        let mut tx = self.pool.begin().await?;
        for id in &ids {
            sqlx::query(&format!("DELETE FROM {table} WHERE id = ?"))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(ids.len() as u64)
    }

    pub fn spawn_archiver(&self, stop: tokio_util::sync::CancellationToken) {
        let store = self.clone();
        tokio::spawn(async move {
            loop {
                if stop.is_cancelled() {
                    break;
                }
                if store.archive_after_days.load(Ordering::Relaxed) != 0 {
                    if let Err(e) = store.archive_due().await {
                        warn!(error = %e, "message archive failed");
                    }
                }
                let every = store.archive_every_secs.load(Ordering::Relaxed).max(60);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(every)) => {}
                }
            }
        });
    }
}

fn year_month(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%Y%m").to_string())
        .unwrap_or_else(|| "unknown".into())
}

async fn write_archive_file(
    path: &Path,
    group_id: Option<i64>,
    rows: &[&StoredMessage],
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(opts).await?;
    sqlx::query(&format!("CREATE TABLE IF NOT EXISTS messages ({DDL_COLS})"))
        .execute(&pool)
        .await?;
    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS messages_mid ON messages(message_id)")
        .execute(&pool)
        .await?;
    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO messages(
                message_id, time, self_id, user_id, group_id,
                nickname, card, raw_text, segments_json, inserted_at
             ) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.message_id)
        .bind(row.time)
        .bind(row.self_id)
        .bind(row.user_id)
        .bind(group_id.or(row.group_id))
        .bind(&row.nickname)
        .bind(&row.card)
        .bind(&row.raw_text)
        .bind(&row.segments_json)
        .bind(row.inserted_at)
        .execute(&pool)
        .await?;
    }
    pool.close().await;
    Ok(())
}

pub fn default_archive_dir(image_root: &Path) -> PathBuf {
    image_root
        .parent()
        .unwrap_or(Path::new("data"))
        .join("archive")
}
