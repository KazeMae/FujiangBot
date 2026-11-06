use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use fujiang_core::{Event, Gateway, MessageEvent, Messenger, Segment, Source};
use napcat_sdk::{Event as NcEvent, FileResult, HttpApi, SendGroupMsg, SendPrivateMsg, SendResult};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::convert::{map_event, map_message, to_send};

/// LLOneBot / OneBot 11 HTTP: call API on `http_api`, receive events on `event_listen`.
pub struct HttpAdapter {
    api: HttpApi,
    event_listen: String,
}

impl HttpAdapter {
    pub fn new(
        http_api: impl Into<String>,
        event_listen: impl Into<String>,
        access_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            api: HttpApi::new(http_api, access_token)?,
            event_listen: event_listen.into(),
        })
    }
}

#[async_trait]
impl Gateway for HttpAdapter {
    async fn run(
        self: Arc<Self>,
        tx: mpsc::Sender<Event>,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        let addr: SocketAddr = self
            .event_listen
            .parse()
            .map_err(|e| anyhow::anyhow!("event_listen `{}`: {e}", self.event_listen))?;
        let app = Router::new()
            .route("/", post(on_event))
            .route("/onebot", post(on_event))
            .with_state(tx);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!(%addr, "llonebot http event listener");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                stop.cancelled().await;
                info!("http adapter cancelled");
            })
            .await?;
        Ok(())
    }
}

async fn on_event(
    State(tx): State<mpsc::Sender<Event>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let ev = NcEvent::parse(body);
    if let Some(mapped) = map_event(ev) {
        if tx.send(mapped).await.is_err() {
            warn!("event channel closed");
        }
    }
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

#[async_trait]
impl Messenger for HttpAdapter {
    async fn send(&self, target: Source, segs: &[Segment]) -> anyhow::Result<i64> {
        let message = segs.iter().map(to_send).collect::<Vec<_>>();
        let res: SendResult = match target {
            Source::Group { id } => {
                self.api
                    .send(
                        "send_group_msg",
                        SendGroupMsg {
                            group_id: id,
                            message,
                            auto_escape: None,
                        },
                    )
                    .await?
            }
            Source::Friend { id } => {
                self.api
                    .send(
                        "send_private_msg",
                        SendPrivateMsg {
                            user_id: id,
                            message,
                            auto_escape: None,
                        },
                    )
                    .await?
            }
        };
        Ok(res.message_id)
    }

    async fn get_message(&self, id: i64) -> anyhow::Result<MessageEvent> {
        let v: Value = self
            .api
            .send("get_msg", json!({ "message_id": id }))
            .await?;
        let nc = NcEvent::parse(v);
        map_message(&nc).ok_or_else(|| anyhow::anyhow!("get_msg: not a message"))
    }

    async fn get_image(&self, file: &str) -> anyhow::Result<std::path::PathBuf> {
        let r: FileResult = self.api.send("get_image", json!({ "file": file })).await?;
        r.file
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("get_image: empty path"))
    }

    async fn get_file(&self, file_id: &str) -> anyhow::Result<std::path::PathBuf> {
        let r: FileResult = self
            .api
            .send("get_file", json!({ "file_id": file_id }))
            .await?;
        r.file
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("get_file: empty path"))
    }

    async fn onebot(&self, action: &str, params: Value) -> anyhow::Result<Value> {
        Ok(self.api.send_value(action, params).await?)
    }
}

#[cfg(test)]
mod onebot_tests {
    use super::*;
    use fujiang_core::Messenger;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn onebot_uses_the_existing_http_client() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("POST /get_group_info "));
            assert!(req.contains("Bearer secret"));
            assert!(req.contains("\"group_id\":7"));
            let body = r#"{"status":"ok","retcode":0,"data":{"group_id":7,"group_name":"g"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        let adapter = HttpAdapter::new(format!("http://{addr}"), "127.0.0.1:9", "secret").unwrap();
        let got = adapter
            .onebot("get_group_info", json!({"group_id": 7}))
            .await
            .unwrap();
        assert_eq!(got["group_name"], "g");
    }
}
