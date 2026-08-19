use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("websocket: {0}")]
    WebSocket(String),
    #[error("not connected")]
    NotConnected,
    #[error("api timeout (echo={echo})")]
    Timeout { echo: String },
    #[error("api failed: retcode={retcode} message={message}")]
    Api { retcode: i64, message: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
