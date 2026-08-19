mod config;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use config::AppConfig;
use fujiang_adapter::NapcatAdapter;
use fujiang_core::{BotContext, Dispatcher, Gateway, Plugin};
use fujiang_plugin_contest::ContestPlugin;
use fujiang_plugin_fun::FunPlugin;
use fujiang_plugin_luck::LuckPlugin;
use fujiang_plugin_problem::ProblemPlugin;
use fujiang_plugin_rank::RankPlugin;
use fujiang_store::Store;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "fujiang", about = "福酱 — ACM QQ 机器人")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 连接 NapCat 并处理消息
    Run {
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// 从旧 Python 目录导入 JSON / 图库
    Migrate {
        #[arg(long)]
        from: PathBuf,
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    match Cli::parse().cmd {
        Cmd::Run { config } => run(config).await,
        Cmd::Migrate { from, config } => migrate(from, config).await,
    }
}

async fn run(path: PathBuf) -> anyhow::Result<()> {
    let cfg = AppConfig::load(&path)?;
    let store = Store::open(&cfg.store.db, &cfg.store.image_root).await?;
    let adapter = Arc::new(NapcatAdapter::new(
        cfg.napcat.ws_url.clone(),
        cfg.napcat.access_token.clone(),
    ));

    let mut plugins: Vec<Arc<dyn Plugin>> = Vec::new();
    if cfg.plugins.contest.enabled {
        plugins.push(Arc::new(ContestPlugin));
    }
    if cfg.plugins.rank.enabled {
        plugins.push(Arc::new(RankPlugin));
    }
    if cfg.plugins.problem.enabled {
        plugins.push(Arc::new(ProblemPlugin));
    }
    if cfg.plugins.fun.enabled {
        plugins.push(Arc::new(FunPlugin));
    }
    if cfg.plugins.luck.enabled {
        plugins.push(Arc::new(LuckPlugin));
    }

    let dispatcher = Arc::new(Dispatcher::new(plugins));
    let ctx = BotContext {
        messenger: adapter.clone(),
        store,
        http: reqwest::Client::builder()
            .user_agent("fujiang-bot/0.1")
            .build()?,
        config: Arc::new(cfg.to_bot_config()),
    };
    dispatcher.start_all(&ctx).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let gw = adapter.clone();
    tokio::spawn(async move {
        if let Err(e) = gw.run(tx).await {
            tracing::error!(error = %e, "gateway stopped");
        }
    });

    info!("fujiang running, {} plugins", dispatcher_len(&dispatcher));
    while let Some(ev) = rx.recv().await {
        let d = dispatcher.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            d.handle(&ctx, &ev).await;
        });
    }
    Ok(())
}

fn dispatcher_len(d: &Dispatcher) -> &'static str {
    let _ = d;
    "enabled"
}

async fn migrate(from: PathBuf, path: PathBuf) -> anyhow::Result<()> {
    let cfg = AppConfig::load(&path)?;
    let store = Store::open(&cfg.store.db, &cfg.store.image_root).await?;
    let report = fujiang_store::migrate_from_python(&store, &from).await?;
    println!("{report}");
    Ok(())
}
