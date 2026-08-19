use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::types::ApiResponse;

/// OneBot 11 HTTP API client (LLOneBot / go-cqhttp style).
#[derive(Clone)]
pub struct HttpApi {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl HttpApi {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let base = base.into().trim_end_matches('/').to_string();
        Ok(Self {
            base,
            token: token.into(),
            client: reqwest::Client::builder()
                .user_agent("fujiang-onebot-http/0.1")
                .build()
                .map_err(|e| Error::Other(e.to_string()))?,
        })
    }

    pub async fn send<P, R>(&self, action: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let url = format!("{}/{}", self.base, action.trim_start_matches('/'));
        let mut req = self.client.post(&url).json(&params);
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let resp = req.send().await.map_err(|e| Error::Other(e.to_string()))?;
        let body: ApiResponse = resp.json().await.map_err(|e| Error::Other(e.to_string()))?;
        if !body.is_ok() {
            return Err(Error::Api {
                retcode: body.retcode,
                message: if body.message.is_empty() {
                    body.wording.unwrap_or_default()
                } else {
                    body.message
                },
            });
        }
        Ok(serde_json::from_value(body.data)?)
    }

    pub async fn send_value(&self, action: &str, params: Value) -> Result<Value> {
        self.send(action, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn http_send_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("send_group_msg"));
            assert!(req.contains("Bearer secret"));
            let body = r#"{"status":"ok","retcode":0,"data":{"message_id":99}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });

        let api = HttpApi::new(format!("http://{addr}"), "secret").unwrap();
        let v: Value = api
            .send("send_group_msg", serde_json::json!({"group_id": 1}))
            .await
            .unwrap();
        assert_eq!(v["message_id"], 99);
    }
}
