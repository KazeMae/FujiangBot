use std::path::Path;

use sqlx::Row;
use tracing::info;

use crate::{md5_hex, AttachResult, DetachResult, ImageRow, Store, Tag, TagListItem};

impl Store {
    pub async fn tag_ensure(&self, name: &str) -> anyhow::Result<Tag> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "tag 名为空");
        if let Some(t) = self.tag_by_alias(name).await? {
            return Ok(t);
        }
        sqlx::query("INSERT OR IGNORE INTO tags(name) VALUES(?)")
            .bind(name)
            .execute(&self.pool)
            .await?;
        let tag = self
            .tag_by_name(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to create tag"))?;
        sqlx::query("INSERT OR IGNORE INTO tag_aliases(alias, tag_id) VALUES(?, ?)")
            .bind(name)
            .bind(tag.id)
            .execute(&self.pool)
            .await?;
        Ok(tag)
    }

    pub async fn tag_by_name(&self, name: &str) -> anyhow::Result<Option<Tag>> {
        Ok(
            sqlx::query_as::<_, Tag>("SELECT id, name FROM tags WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn tag_by_alias(&self, alias: &str) -> anyhow::Result<Option<Tag>> {
        Ok(sqlx::query_as::<_, Tag>(
            "SELECT t.id, t.name FROM tags t
             JOIN tag_aliases a ON a.tag_id = t.id
             WHERE a.alias = ?",
        )
        .bind(alias)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn tag_list(&self) -> anyhow::Result<Vec<TagListItem>> {
        let tags = sqlx::query_as::<_, Tag>("SELECT id, name FROM tags ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for tag in tags {
            let image_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM image_tags WHERE tag_id = ?")
                    .bind(tag.id)
                    .fetch_one(&self.pool)
                    .await?;
            let aliases: Vec<String> =
                sqlx::query("SELECT alias FROM tag_aliases WHERE tag_id = ?")
                    .bind(tag.id)
                    .fetch_all(&self.pool)
                    .await?
                    .into_iter()
                    .map(|r| r.get::<String, _>(0))
                    .collect();
            out.push(TagListItem {
                tag,
                image_count,
                aliases,
            });
        }
        Ok(out)
    }

    pub async fn tag_alias(&self, name: &str, alias: &str) -> anyhow::Result<String> {
        let tag = self.tag_ensure(name).await?;
        let alias = alias.trim();
        anyhow::ensure!(!alias.is_empty(), "别名为空");
        sqlx::query(
            "INSERT INTO tag_aliases(alias, tag_id) VALUES(?, ?)
             ON CONFLICT(alias) DO UPDATE SET tag_id = excluded.tag_id",
        )
        .bind(alias)
        .bind(tag.id)
        .execute(&self.pool)
        .await?;
        Ok(format!("{alias} -> {}", tag.name))
    }

    pub async fn tag_merge(&self, from: &str, to: &str) -> anyhow::Result<String> {
        let src = self
            .tag_by_alias(from)
            .await?
            .ok_or_else(|| anyhow::anyhow!("tag「{from}」不存在"))?;
        let dst = self.tag_ensure(to).await?;
        if src.id == dst.id {
            return Ok("已经是同一个 tag".into());
        }
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO image_tags(image_id, tag_id, added_by, added_at)
             SELECT image_id, ?, 0, ? FROM image_tags WHERE tag_id = ?",
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

    pub async fn tag_retire(&self, name: &str) -> anyhow::Result<bool> {
        let Some(tag) = self.tag_by_alias(name).await? else {
            return Ok(false);
        };
        sqlx::query("DELETE FROM tags WHERE id = ?")
            .bind(tag.id)
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
        tag_name: &str,
        added_by: i64,
    ) -> anyhow::Result<AttachResult> {
        let Some(img) = self.image_by_md5(md5).await? else {
            anyhow::bail!("unknown image");
        };
        let tag = self.tag_ensure(tag_name).await?;
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO image_tags(image_id, tag_id, added_by, added_at)
             VALUES(?, ?, ?, ?)",
        )
        .bind(img.id)
        .bind(tag.id)
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

    pub async fn image_detach(&self, md5: &str, tag_name: &str) -> anyhow::Result<DetachResult> {
        let Some(img) = self.image_by_md5(md5).await? else {
            return Ok(DetachResult::UnknownImage);
        };
        let Some(tag) = self.tag_by_alias(tag_name).await? else {
            return Ok(DetachResult::NoSuchTag);
        };
        let r = sqlx::query("DELETE FROM image_tags WHERE image_id = ? AND tag_id = ?")
            .bind(img.id)
            .bind(tag.id)
            .execute(&self.pool)
            .await?;
        Ok(if r.rows_affected() == 0 {
            DetachResult::NoSuchTag
        } else {
            DetachResult::Detached
        })
    }

    pub async fn image_tags(&self, md5: &str) -> anyhow::Result<Vec<String>> {
        let Some(img) = self.image_by_md5(md5).await? else {
            return Ok(vec![]);
        };
        let rows = sqlx::query(
            "SELECT t.name FROM tags t
             JOIN image_tags x ON x.tag_id = t.id
             WHERE x.image_id = ? ORDER BY t.name",
        )
        .bind(img.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }

    pub async fn random_by_tag(&self, alias: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
        let Some(tag) = self.tag_by_alias(alias).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT img.rel_path FROM images img
             JOIN image_tags x ON x.image_id = img.id
             WHERE x.tag_id = ? ORDER BY RANDOM() LIMIT 1",
        )
        .bind(tag.id)
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
            let tag = self.tag_ensure(&name).await?;
            let aliases = sqlx::query("SELECT alias FROM album_aliases WHERE album_id = ?")
                .bind(aid)
                .fetch_all(&self.pool)
                .await?;
            for a in aliases {
                let alias: String = a.get(0);
                if alias != name {
                    self.tag_alias(&name, &alias).await.ok();
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
                if self.image_by_md5(&md5).await?.is_none() && (dst.exists() || src.exists()) {
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
                    self.image_attach(&md5, &tag.name, added_by).await?;
                    n += 1;
                }
            }
        }

        rename_if_exists(&self.pool, "album_images", "album_images_legacy").await?;
        rename_if_exists(&self.pool, "album_aliases", "album_aliases_legacy").await?;
        rename_if_exists(&self.pool, "albums", "albums_legacy").await?;
        if n > 0 {
            info!(n, "migrated legacy albums to image tags");
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
