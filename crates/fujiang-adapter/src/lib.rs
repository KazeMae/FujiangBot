mod convert;
mod http;
mod ws;

use std::sync::Arc;

use async_trait::async_trait;
use fujiang_core::{Event, Gateway, Messenger};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use http::HttpAdapter;
pub use ws::{NapcatAdapter, WsAdapter};

/// Runtime adapter selected by config.
#[derive(Clone)]
pub enum AnyAdapter {
    Ws(Arc<WsAdapter>),
    Http(Arc<HttpAdapter>),
}

impl AnyAdapter {
    pub fn messenger(&self) -> Arc<dyn Messenger> {
        match self {
            Self::Ws(a) => a.clone(),
            Self::Http(a) => a.clone(),
        }
    }
}

#[async_trait]
impl Gateway for AnyAdapter {
    async fn run(
        self: Arc<Self>,
        tx: mpsc::Sender<Event>,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        match self.as_ref() {
            Self::Ws(a) => a.clone().run(tx, stop).await,
            Self::Http(a) => a.clone().run(tx, stop).await,
        }
    }
}
