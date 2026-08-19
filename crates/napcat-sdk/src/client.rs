use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::events::Event;
use crate::types::{ApiRequest, ApiResponse};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub ws_url: String,
    pub access_token: String,
    pub reconnect: bool,
    pub reconnect_attempts: u32,
    pub reconnect_delay: Duration,
    pub api_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            ws_url: "ws://127.0.0.1:3001".into(),
            access_token: String::new(),
            reconnect: true,
            reconnect_attempts: 10,
            reconnect_delay: Duration::from_secs(5),
            api_timeout: Duration::from_secs(120),
        }
    }
}

struct Pending {
    tx: oneshot::Sender<ApiResponse>,
}

struct Inner {
    outgoing: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pending: Mutex<HashMap<String, Pending>>,
    events: broadcast::Sender<Event>,
}

/// Forward-WebSocket NapCat client (node-napcat-ts `NCWebsocket` equivalent).
#[derive(Clone)]
pub struct NapcatClient {
    config: ClientConfig,
    inner: Arc<Inner>,
}

impl NapcatClient {
    pub fn new(config: ClientConfig) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            config,
            inner: Arc::new(Inner {
                outgoing: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    fn connect_url(&self) -> Result<url::Url> {
        let mut url = url::Url::parse(&self.config.ws_url)?;
        if !self.config.access_token.is_empty() {
            url.query_pairs_mut()
                .append_pair("access_token", &self.config.access_token);
        }
        Ok(url)
    }

    /// Connect and serve until `stop` is cancelled or reconnects are exhausted.
    pub async fn run(&self, stop: CancellationToken) -> Result<()> {
        let mut attempt = 0u32;
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => return Ok(()),
                session_res = self.session() => {
                    match session_res {
                        Ok(()) => attempt = 0,
                        Err(e) => warn!(error = %e, "napcat session ended"),
                    }
                }
            }
            if stop.is_cancelled() {
                return Ok(());
            }
            if !self.config.reconnect {
                return Err(Error::Other("disconnected".into()));
            }
            attempt += 1;
            if attempt > self.config.reconnect_attempts {
                return Err(Error::Other("reconnect attempts exhausted".into()));
            }
            info!(attempt, "reconnecting to napcat");
            tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                _ = tokio::time::sleep(self.config.reconnect_delay) => {}
            }
        }
    }

    async fn session(&self) -> Result<()> {
        let url = self.connect_url()?;
        info!(%url, "connecting napcat websocket");
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| Error::WebSocket(e.to_string()))?;
        info!("napcat websocket open");

        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *self.inner.outgoing.lock().await = Some(tx);

        let write = async {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            Result::<()>::Ok(())
        };

        let read = async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => self.on_text(&text).await,
                    Ok(Message::Binary(bin)) => {
                        if let Ok(text) = std::str::from_utf8(&bin) {
                            self.on_text(text).await;
                        }
                    }
                    Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                    Ok(Message::Close(c)) => {
                        warn!(?c, "napcat closed");
                        break;
                    }
                    Err(e) => {
                        error!(error = %e, "websocket read");
                        break;
                    }
                    _ => {}
                }
            }
            Result::<()>::Ok(())
        };

        let res = tokio::select! {
            r = write => r,
            r = read => r,
        };
        *self.inner.outgoing.lock().await = None;
        res
    }

    async fn on_text(&self, text: &str) {
        let trimmed = text.trim();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            warn!("non-json frame");
            return;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "json parse failed");
                return;
            }
        };
        debug!(%value, "recv");
        if let Some(echo) = value.get("echo").and_then(|e| e.as_str()) {
            if let Ok(resp) = serde_json::from_value::<ApiResponse>(value.clone()) {
                let mut pending = self.inner.pending.lock().await;
                if let Some(p) = pending.remove(echo) {
                    let _ = p.tx.send(resp);
                    return;
                }
            }
        }
        if value.get("post_type").is_some() {
            let ev = Event::parse(value);
            let _ = self.inner.events.send(ev);
        }
    }

    pub async fn send<P, R>(&self, action: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let echo = Uuid::new_v4().to_string();
        let req = ApiRequest {
            action: action.to_string(),
            params,
            echo: echo.clone(),
        };
        let payload = serde_json::to_string(&req)?;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().await;
            pending.insert(echo.clone(), Pending { tx });
        }
        {
            let guard = self.inner.outgoing.lock().await;
            let Some(out) = guard.as_ref() else {
                self.inner.pending.lock().await.remove(&echo);
                return Err(Error::NotConnected);
            };
            if out.send(payload).is_err() {
                self.inner.pending.lock().await.remove(&echo);
                return Err(Error::NotConnected);
            }
        }

        let resp = tokio::time::timeout(self.config.api_timeout, rx)
            .await
            .map_err(|_| {
                // best-effort cleanup
                Error::Timeout { echo: echo.clone() }
            })?;

        let resp = match resp {
            Ok(r) => r,
            Err(_) => {
                self.inner.pending.lock().await.remove(&echo);
                return Err(Error::Other("caller dropped".into()));
            }
        };

        if !resp.is_ok() {
            return Err(Error::Api {
                retcode: resp.retcode,
                message: if resp.message.is_empty() {
                    resp.wording.unwrap_or_default()
                } else {
                    resp.message
                },
            });
        }
        Ok(serde_json::from_value(resp.data)?)
    }

    pub async fn send_value(&self, action: &str, params: Value) -> Result<Value> {
        self.send(action, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn echo_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            use futures_util::{SinkExt, StreamExt};
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                let echo = v["echo"].as_str().unwrap().to_string();
                let resp = serde_json::json!({
                    "status": "ok",
                    "retcode": 0,
                    "data": {"pong": true},
                    "echo": echo
                });
                ws.send(Message::Text(resp.to_string().into()))
                    .await
                    .unwrap();
            }
        });

        let client = NapcatClient::new(ClientConfig {
            ws_url: format!("ws://{addr}"),
            reconnect: false,
            api_timeout: Duration::from_secs(2),
            ..ClientConfig::default()
        });
        let c = client.clone();
        let stop = CancellationToken::new();
        let stop2 = stop.clone();
        tokio::spawn(async move {
            let _ = c.run(stop2).await;
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        let v: Value = client
            .send("get_status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(v["pong"], true);
        stop.cancel();
    }
}
