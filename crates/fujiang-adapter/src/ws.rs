use std::sync::Arc;

use async_trait::async_trait;
use fujiang_core::{Event, Gateway, MessageEvent, Messenger, Segment, Source};
use napcat_sdk::{ClientConfig, Event as NcEvent, NapcatClient, SendGroupMsg, SendPrivateMsg};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::convert::{map_event, map_message, to_send};

/// OneBot 11 forward WebSocket (NapCat / LLOneBot).
pub struct WsAdapter {
    client: NapcatClient,
}

pub type NapcatAdapter = WsAdapter;

impl WsAdapter {
    pub fn new(ws_url: String, access_token: String) -> Self {
        let client = NapcatClient::new(ClientConfig {
            ws_url,
            access_token,
            ..ClientConfig::default()
        });
        Self { client }
    }
}

#[async_trait]
impl Gateway for WsAdapter {
    async fn run(
        self: Arc<Self>,
        tx: mpsc::Sender<Event>,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut rx = self.client.subscribe();
        let client = self.client.clone();
        let client_stop = stop.clone();
        let mut runner = tokio::spawn(async move { client.run(client_stop).await });

        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Ok(nc) => {
                            if let Some(mapped) = map_event(nc) {
                                if tx.send(mapped).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(n, "event lag");
                        }
                        Err(_) => break,
                    }
                }
                _ = stop.cancelled() => {
                    info!("ws adapter cancelled");
                    break;
                }
            }
        }
        tokio::select! {
            _ = &mut runner => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                warn!("ws client did not stop in time, abort");
                runner.abort();
                let _ = runner.await;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Messenger for WsAdapter {
    async fn send(&self, target: Source, segs: &[Segment]) -> anyhow::Result<i64> {
        let message = segs.iter().map(to_send).collect::<Vec<_>>();
        let id = match target {
            Source::Group { id } => {
                self.client
                    .send_group_msg(SendGroupMsg {
                        group_id: id,
                        message,
                        auto_escape: None,
                    })
                    .await?
                    .message_id
            }
            Source::Friend { id } => {
                self.client
                    .send_private_msg(SendPrivateMsg {
                        user_id: id,
                        message,
                        auto_escape: None,
                    })
                    .await?
                    .message_id
            }
        };
        Ok(id)
    }

    async fn get_message(&self, id: i64) -> anyhow::Result<MessageEvent> {
        let v = self.client.get_msg(id).await?;
        let nc = NcEvent::parse(v);
        map_message(&nc).ok_or_else(|| anyhow::anyhow!("get_msg: not a message"))
    }

    async fn get_image(&self, file: &str) -> anyhow::Result<std::path::PathBuf> {
        let r = self.client.get_image(file).await?;
        r.file
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("get_image: empty path"))
    }

    async fn get_file(&self, file_id: &str) -> anyhow::Result<std::path::PathBuf> {
        let r = self.client.get_file(file_id).await?;
        r.file
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("get_file: empty path"))
    }
}
