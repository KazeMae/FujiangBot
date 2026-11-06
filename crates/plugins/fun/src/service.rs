use async_trait::async_trait;
use fujiang_core::Service;
use fujiang_store::{Store, TagListItem};
use serde_json::{json, Value};

pub struct FunService {
    store: Store,
}

impl FunService {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    async fn tag_rows(&self) -> anyhow::Result<Vec<Value>> {
        let list = self.store.tag_list().await?;
        Ok(list.into_iter().map(tag_json).collect())
    }

    async fn star_rows(&self) -> anyhow::Result<Vec<Value>> {
        let list = self.store.star_list().await?;
        Ok(list
            .into_iter()
            .map(|row| json!({"name": row.name, "url": row.url}))
            .collect())
    }

    async fn learn_rows(&self, args: &Value) -> anyhow::Result<Vec<Value>> {
        let group_id = group_id_of(args)?;
        let trigger = optional_text(args, "trigger")?;
        let rows = self.store.learn_list(group_id, trigger.as_deref()).await?;
        Ok(rows
            .into_iter()
            .map(|row| json!({"trigger": row.trigger, "reply": row.reply}))
            .collect())
    }

    async fn search(&self, args: &Value) -> anyhow::Result<Value> {
        let q = required_text(args, "q", "fun.search 需要 q")?;
        let group_id = group_id_of(args)?;
        let scope = match group_id {
            Some(id) => json!({"group_id": id}),
            None => json!({}),
        };
        let tags = self
            .tag_rows()
            .await?
            .into_iter()
            .filter(|row| tag_hit(row, &q))
            .collect::<Vec<_>>();
        let stars = self
            .star_rows()
            .await?
            .into_iter()
            .filter(|row| text_hit(row, "name", &q) || text_hit(row, "url", &q))
            .collect::<Vec<_>>();
        let learn = self
            .learn_rows(&scope)
            .await?
            .into_iter()
            .filter(|row| text_hit(row, "trigger", &q) || text_hit(row, "reply", &q))
            .collect::<Vec<_>>();
        Ok(json!({"tags": tags, "stars": stars, "learn": learn}))
    }
}

#[async_trait]
impl Service for FunService {
    async fn call(&self, method: &str, args: Value) -> anyhow::Result<Value> {
        match method {
            "tags" => Ok(Value::Array(self.tag_rows().await?)),
            "stars" => Ok(Value::Array(self.star_rows().await?)),
            "learn" => Ok(Value::Array(self.learn_rows(&args).await?)),
            "search" => self.search(&args).await,
            other => anyhow::bail!("fun 没有方法 `{other}`（tags / stars / learn / search）"),
        }
    }
}

fn tag_json(item: TagListItem) -> Value {
    let mut aliases = item.aliases;
    aliases.sort();
    json!({
        "name": item.tag.name,
        "count": item.image_count,
        "aliases": aliases,
    })
}

fn group_id_of(args: &Value) -> anyhow::Result<Option<i64>> {
    match args.get("group_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("fun 的 group_id 要是整数")),
        Some(_) => anyhow::bail!("fun 的 group_id 要是整数"),
    }
}

fn optional_text(args: &Value, key: &str) -> anyhow::Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text.to_string()))
            }
        }
        Some(_) => anyhow::bail!("fun 的 {key} 要是字符串"),
    }
}

fn required_text(args: &Value, key: &str, missing: &'static str) -> anyhow::Result<String> {
    optional_text(args, key)?.ok_or_else(|| anyhow::anyhow!(missing))
}

fn text_hit(row: &Value, key: &str, q: &str) -> bool {
    row.get(key)
        .and_then(Value::as_str)
        .is_some_and(|text| text.contains(q))
}

fn tag_hit(row: &Value, q: &str) -> bool {
    text_hit(row, "name", q)
        || row
            .get("aliases")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.as_str().is_some_and(|text| text.contains(q)))
            })
}

#[cfg(test)]
mod tests {
    use fujiang_core::Plugin;

    use super::*;

    const WRITE_METHODS: &[&str] = &[
        "delete",
        "del",
        "retire",
        "album_del",
        "albums_delete",
        "add",
        "set",
        "merge",
        "alias",
        "attach",
        "detach",
        "learn_del",
        "star_del",
        "tag_retire",
    ];

    #[test]
    fn plugin_provides_fun() {
        assert_eq!(crate::FunPlugin.provides(), &["fun"]);
    }

    async fn open_store() -> (FunService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fujiang-fun-{}-{}",
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
        (FunService::new(store), dir)
    }

    #[tokio::test]
    async fn reads_match_the_stored_rows() {
        let (svc, dir) = open_store().await;
        assert_eq!(svc.call("tags", json!({})).await.unwrap(), json!([]));
        assert_eq!(svc.call("stars", json!({})).await.unwrap(), json!([]));
        assert_eq!(svc.call("learn", json!({})).await.unwrap(), json!([]));

        let img = svc.store.image_upsert(b"pic", "jpg", 1).await.unwrap();
        svc.store.image_attach(&img.md5, "福哥", 1).await.unwrap();
        svc.store.tag_alias("福哥", "猫").await.unwrap();
        svc.store
            .star_add("oj", "https://oj.example", 1)
            .await
            .unwrap();
        svc.store.learn_add("猫", "喵", 1, Some(9)).await.unwrap();
        svc.store.learn_add("猫咪", "长", 1, Some(9)).await.unwrap();
        svc.store.learn_add("狗", "汪", 1, Some(9)).await.unwrap();
        svc.store.learn_add("猫", "全局", 1, None).await.unwrap();

        assert_eq!(
            svc.call("tags", json!({})).await.unwrap(),
            json!([{
                "name": "福哥",
                "count": 1,
                "aliases": ["猫", "福哥"]
            }])
        );
        assert_eq!(
            svc.call("stars", json!({"name": "ignored"})).await.unwrap(),
            json!([{"name": "oj", "url": "https://oj.example"}])
        );
        assert_eq!(
            svc.call("learn", json!({"group_id": 9})).await.unwrap(),
            json!([
                {"trigger": "狗", "reply": "汪"},
                {"trigger": "猫", "reply": "喵"},
                {"trigger": "猫咪", "reply": "长"}
            ])
        );
        assert_eq!(
            svc.call("learn", json!({"group_id": 9, "trigger": "猫"}))
                .await
                .unwrap(),
            json!([{"trigger": "猫", "reply": "喵"}])
        );
        assert_eq!(
            svc.call("learn", json!({"trigger": "  "})).await.unwrap(),
            json!([{"trigger": "猫", "reply": "全局"}])
        );
        assert_eq!(
            svc.call("learn", json!({"group_id": 8})).await.unwrap(),
            json!([])
        );
        let bad_group = svc
            .call("learn", json!({"group_id": "9"}))
            .await
            .unwrap_err();
        assert_eq!(bad_group.to_string(), "fun 的 group_id 要是整数");
        assert!(dir.join("img").join(&img.rel_path).exists());
        drop(svc);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn search_filters_the_same_rows() {
        let (svc, dir) = open_store().await;
        let img = svc.store.image_upsert(b"pic", "jpg", 1).await.unwrap();
        svc.store.image_attach(&img.md5, "福哥", 1).await.unwrap();
        svc.store.tag_alias("福哥", "猫").await.unwrap();
        svc.store
            .star_add("oj", "https://oj.example", 1)
            .await
            .unwrap();
        svc.store
            .star_add("cat", "https://CAT.example", 1)
            .await
            .unwrap();
        svc.store.learn_add("猫咪", "长", 1, Some(9)).await.unwrap();
        svc.store.learn_add("猫", "全局", 1, None).await.unwrap();

        let found = svc
            .call("search", json!({"q": " 猫 ", "group_id": 9}))
            .await
            .unwrap();
        assert_eq!(
            found,
            json!({
                "tags": [{
                    "name": "福哥",
                    "count": 1,
                    "aliases": ["猫", "福哥"]
                }],
                "stars": [],
                "learn": [{"trigger": "猫咪", "reply": "长"}]
            })
        );
        let global = svc.call("search", json!({"q": "猫"})).await.unwrap();
        assert_eq!(global["learn"], json!([{"trigger": "猫", "reply": "全局"}]));
        assert_eq!(global["tags"][0]["name"], "福哥");
        let ascii = svc.call("search", json!({"q": "cat"})).await.unwrap();
        assert_eq!(
            ascii["stars"],
            json!([{"name": "cat", "url": "https://CAT.example"}])
        );
        assert_eq!(
            svc.call("search", json!({"q": "Cat"})).await.unwrap(),
            json!({"tags": [], "stars": [], "learn": []})
        );
        let missing = svc.call("search", json!({})).await.unwrap_err();
        assert_eq!(missing.to_string(), "fun.search 需要 q");
        let blank = svc.call("search", json!({"q": "  "})).await.unwrap_err();
        assert_eq!(blank.to_string(), "fun.search 需要 q");
        drop(svc);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn write_methods_leave_the_rows_in_place() {
        let (svc, dir) = open_store().await;
        let img = svc.store.image_upsert(b"pic", "jpg", 1).await.unwrap();
        svc.store.image_attach(&img.md5, "福哥", 1).await.unwrap();
        svc.store
            .star_add("oj", "https://oj.example", 1)
            .await
            .unwrap();
        svc.store.learn_add("猫", "喵", 1, Some(9)).await.unwrap();
        let tags = svc.call("tags", json!({})).await.unwrap();
        let stars = svc.call("stars", json!({})).await.unwrap();
        let learn = svc.call("learn", json!({"group_id": 9})).await.unwrap();

        for method in WRITE_METHODS {
            let err = svc
                .call(method, json!({"name": "福哥", "q": "猫"}))
                .await
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("fun 没有方法 `{method}`（tags / stars / learn / search）")
            );
        }
        assert_eq!(svc.call("tags", json!({})).await.unwrap(), tags);
        assert_eq!(svc.call("stars", json!({})).await.unwrap(), stars);
        assert_eq!(
            svc.call("learn", json!({"group_id": 9})).await.unwrap(),
            learn
        );
        assert!(dir.join("img").join(&img.rel_path).exists());
        let unknown = svc.call("list", json!({})).await.unwrap_err();
        assert_eq!(
            unknown.to_string(),
            "fun 没有方法 `list`（tags / stars / learn / search）"
        );
        drop(svc);
        let _ = std::fs::remove_dir_all(dir);
    }
}
