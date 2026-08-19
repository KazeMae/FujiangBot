use std::path::Path;

use sqlx::Row;
use tracing::info;

use crate::{md5_hex, AttachResult, DetachResult, Idea, IdeaListItem, ImageRow, Store};

impl Store {
    pub async fn idea_ensure(&self, name: &str) -> anyhow::Result<Idea> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "idea 名为空");
        if let Some(i) = self.idea_by_alias(name).await? {
            return Ok(i);
        }
        sqlx::query("INSERT OR IGNORE INTO ideas(name) VALUES(?)")
            .bind(name)
            .execute(&self.pool)
            .await?;
        let idea = self
            .idea_by_name(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to create idea"))?;
        sqlx::query("INSERT OR IGNORE INTO idea_aliases(alias, idea_id) VALUES(?, ?)")
            .bind(name)
            .bind(idea.id)
            .execute(&self.pool)
            .await?;
        Ok(idea)
    }

    pub async fn idea_by_name(&self, name: &str) -> anyhow::Result<Option<Idea>> {
        Ok(
            sqlx::query_as::<_, Idea>("SELECT id, name FROM ideas WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn idea_by_alias(&self, alias: &str) -> anyhow::Result<Option<Idea>> {
        Ok(sqlx::query_as::<_, Idea>(
            "SELECT i.id, i.name FROM ideas i
             JOIN idea_aliases a ON a.idea_id = i.id
             WHERE a.alias = ?",
        )
        .bind(alias)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn idea_list(&self) -> anyhow::Result<Vec<IdeaListItem>> {
        let ideas = sqlx::query_as::<_, Idea>("SELECT id, name FROM ideas ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for idea in ideas {
            let image_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM image_ideas WHERE idea_id = ?")
                    .bind(idea.id)
                    .fetch_one(&self.pool)
                    .await?;
            let aliases: Vec<String> =
                sqlx::query("SELECT alias FROM idea_aliases WHERE idea_id = ?")
                    .bind(idea.id)
                    .fetch_all(&self.pool)
                    .await?
                    .into_iter()
                    .map(|r| r.get::<String, _>(0))
                    .collect();
            out.push(IdeaListItem {
                idea,
                image_count,
                aliases,
            });
        }
        Ok(out)
    }

    pub async fn idea_alias(&self, name: &str, alias: &str) -> anyhow::Result<String> {
        let idea = self.idea_ensure(name).await?;
        let alias = alias.trim();
        anyhow::ensure!(!alias.is_empty(), "别名为空");
        sqlx::query(
            "INSERT INTO idea_aliases(alias, idea_id) VALUES(?, ?)
             ON CONFLICT(alias) DO UPDATE SET idea_id = excluded.idea_id",
        )
        .bind(alias)
        .bind(idea.id)
        .execute(&self.pool)
        .await?;
        Ok(format!("{alias} -> {}", idea.name))
    }

    pub async fn idea_merge(&self, from: &str, to: &str) -> anyhow::Result<String> {
        let src = self
            .idea_by_alias(from)
            .await?
            .ok_or_else(|| anyhow::anyhow!("idea「{from}」不存在"))?;
        let dst = self.idea_ensure(to).await?;
        if src.id == dst.id {
            return Ok("已经是同一个 idea".into());
        }
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO image_ideas(image_id, idea_id, added_by, added_at)
             SELECT image_id, ?, 0, ? FROM image_ideas WHERE idea_id = ?",
        )
        .bind(dst.id)
        .bind(now)
        .bind(src.id)
        .execute(&self.pool)
        .await?;
        Ok(format!(
            "已给 {} 张带「{}」的图挂上「{}」（「{}」仍保留）",
            r.rows_affected(),
            src.name,
            dst.name,
            src.name
        ))
    }

    pub async fn idea_retire(&self, name: &str) -> anyhow::Result<bool> {
        let Some(idea) = self.idea_by_alias(name).await? else {
            return Ok(false);
        };
        sqlx::query("DELETE FROM ideas WHERE id = ?")
            .bind(idea.id)
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    pub async fn image_by_md5(&self, md5: &str) -> anyhow::Result<Option<ImageRow>> {
        Ok(sqlx::query_as::<_, ImageRow>(
            "SELECT id, md5, rel_path, added_by, added_at FROM images WHERE md5 = ?",
        )
        .bind(md5)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn image_upsert(
        &self,
        bytes: &[u8],
        ext: &str,
        added_by: i64,
    ) -> anyhow::Result<ImageRow> {
        let md5 = md5_hex(bytes);
        if let Some(img) = self.image_by_md5(&md5).await? {
            return Ok(img);
        }
        let ext = ext.trim_start_matches('.').to_lowercase();
        let ext = if ext.is_empty() { "jpg".into() } else { ext };
        let rel = format!("{md5}.{ext}");
        let path = self.image_root.join(&rel);
        if !path.exists() {
            tokio::fs::write(&path, bytes).await?;
        }
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO images(md5, rel_path, added_by, added_at) VALUES(?, ?, ?, ?)
             ON CONFLICT(md5) DO NOTHING",
        )
        .bind(&md5)
        .bind(&rel)
        .bind(added_by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.image_by_md5(&md5)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to upsert image"))
    }

    pub async fn image_attach(
        &self,
        md5: &str,
        idea_name: &str,
        added_by: i64,
    ) -> anyhow::Result<AttachResult> {
        let Some(img) = self.image_by_md5(md5).await? else {
            anyhow::bail!("unknown image");
        };
        let idea = self.idea_ensure(idea_name).await?;
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO image_ideas(image_id, idea_id, added_by, added_at)
             VALUES(?, ?, ?, ?)",
        )
        .bind(img.id)
        .bind(idea.id)
        .bind(added_by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(if r.rows_affected() == 0 {
            AttachResult::Already
        } else {
            AttachResult::Attached
        })
    }

    pub async fn image_detach(&self, md5: &str, idea_name: &str) -> anyhow::Result<DetachResult> {
        let Some(img) = self.image_by_md5(md5).await? else {
            return Ok(DetachResult::UnknownImage);
        };
        let Some(idea) = self.idea_by_alias(idea_name).await? else {
            return Ok(DetachResult::NoSuchIdea);
        };
        let r = sqlx::query("DELETE FROM image_ideas WHERE image_id = ? AND idea_id = ?")
            .bind(img.id)
            .bind(idea.id)
            .execute(&self.pool)
            .await?;
        Ok(if r.rows_affected() == 0 {
            DetachResult::NoSuchIdea
        } else {
            DetachResult::Detached
        })
    }

    pub async fn image_ideas(&self, md5: &str) -> anyhow::Result<Vec<String>> {
        let Some(img) = self.image_by_md5(md5).await? else {
            return Ok(vec![]);
        };
        let rows = sqlx::query(
            "SELECT i.name FROM ideas i
             JOIN image_ideas x ON x.idea_id = i.id
             WHERE x.image_id = ? ORDER BY i.name",
        )
        .bind(img.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }

    pub async fn random_by_idea(&self, alias: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
        let Some(idea) = self.idea_by_alias(alias).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT img.rel_path FROM images img
             JOIN image_ideas x ON x.image_id = img.id
             WHERE x.idea_id = ? ORDER BY RANDOM() LIMIT 1",
        )
        .bind(idea.id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| self.image_root.join(r.get::<String, _>(0))))
    }

    pub async fn migrate_legacy_albums(&self) -> anyhow::Result<u64> {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'albums'",
        )
        .fetch_optional(&self.pool)
        .await?;
        if exists.is_none() {
            return Ok(0);
        }

        let albums = sqlx::query("SELECT id, name, dir FROM albums")
            .fetch_all(&self.pool)
            .await?;
        let mut n = 0u64;
        for album in &albums {
            let aid: i64 = album.get(0);
            let name: String = album.get(1);
            let idea = self.idea_ensure(&name).await?;
            let aliases = sqlx::query("SELECT alias FROM album_aliases WHERE album_id = ?")
                .bind(aid)
                .fetch_all(&self.pool)
                .await?;
            for a in aliases {
                let alias: String = a.get(0);
                if alias != name {
                    self.idea_alias(&name, &alias).await.ok();
                }
            }
            let imgs = sqlx::query(
                "SELECT rel_path, md5, added_by, added_at FROM album_images WHERE album_id = ?",
            )
            .bind(aid)
            .fetch_all(&self.pool)
            .await?;
            for img in imgs {
                let rel: String = img.get(0);
                let md5: String = img.get(1);
                let added_by: i64 = img.get(2);
                let added_at: i64 = img.get(3);
                let src = self.image_root.join(&rel);
                let ext = Path::new(&rel)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("jpg");
                let new_rel = format!("{md5}.{ext}");
                let dst = self.image_root.join(&new_rel);
                if src.exists() && !dst.exists() {
                    let _ = tokio::fs::copy(&src, &dst).await;
                }
                if let Some(img_row) = self.image_by_md5(&md5).await? {
                    let _ = img_row;
                } else if dst.exists() || src.exists() {
                    sqlx::query(
                        "INSERT OR IGNORE INTO images(md5, rel_path, added_by, added_at)
                         VALUES(?, ?, ?, ?)",
                    )
                    .bind(&md5)
                    .bind(&new_rel)
                    .bind(added_by)
                    .bind(added_at)
                    .execute(&self.pool)
                    .await?;
                }
                if self.image_by_md5(&md5).await?.is_some() {
                    self.image_attach(&md5, &idea.name, added_by).await?;
                    n += 1;
                }
            }
        }

        rename_if_exists(&self.pool, "album_images", "album_images_legacy").await?;
        rename_if_exists(&self.pool, "album_aliases", "album_aliases_legacy").await?;
        rename_if_exists(&self.pool, "albums", "albums_legacy").await?;
        if n > 0 {
            info!(n, "migrated legacy albums to image ideas");
        }
        Ok(n)
    }
}

async fn rename_if_exists(pool: &sqlx::SqlitePool, from: &str, to: &str) -> anyhow::Result<()> {
    let exists: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(from)
            .fetch_optional(pool)
            .await?;
    if exists.is_none() {
        return Ok(());
    }
    let dest: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(to)
            .fetch_optional(pool)
            .await?;
    if dest.is_some() {
        sqlx::query(&format!("DROP TABLE {from}"))
            .execute(pool)
            .await?;
        return Ok(());
    }
    sqlx::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .execute(pool)
        .await?;
    Ok(())
}
