mod admin;
mod config;
mod dynload;
mod plugins;
mod runtime;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use config::AppConfig;
use dynload::PluginHub;
use fujiang_core::{BotContext, Dispatcher};
use fujiang_store::{Store, StoreOpts};
use runtime::AppState;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
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
    let store = Store::open_opts(StoreOpts {
        db: cfg.store.db.clone(),
        image_root: PathBuf::from(&cfg.store.image_root),
        archive_dir: PathBuf::from(&cfg.store.archive_dir),
        archive_after_days: cfg.store.archive_after_days,
        archive_every_hours: cfg.store.archive_every_hours,
    })
    .await?;

    let process_stop = CancellationToken::new();
    store.spawn_archiver(process_stop.clone());

    let adapter = Arc::new(runtime::build_adapter(&cfg)?);
    let dispatcher = Arc::new(Dispatcher::new(plugins::initial(&cfg)));
    let ctx = BotContext {
        messenger: Arc::new(RwLock::new(adapter.messenger())),
        store,
        http: reqwest::Client::builder()
            .user_agent("fujiang-bot/0.1")
            .build()?,
        config: Arc::new(RwLock::new(cfg.to_bot_config())),
        plugin_configs: Arc::new(RwLock::new(plugins::all_configs(&cfg))),
    };
    dispatcher.start_all(&ctx).await?;

    let hub = Arc::new(PluginHub::new(&cfg.plugins.dir));
    hub.set_disabled(cfg.plugins.disabled.clone()).await;
    if let Err(e) = hub.scan(&dispatcher, &ctx).await {
        tracing::warn!(error = %e, "initial plugin scan");
    }
    if cfg.plugins.watch {
        let h = hub.clone();
        let d = dispatcher.clone();
        let c = ctx.clone();
        let stop = process_stop.clone();
        tokio::spawn(async move {
            h.watch_loop(d, c, stop).await;
        });
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    {
        let stop = process_stop.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.cancel();
        });
    }

    let admin_listen = cfg.admin.listen.clone();
    let state = Arc::new(AppState::new(path, cfg, ctx, dispatcher, hub, tx));
    state.start_gateway(adapter).await;

    let listener = tokio::net::TcpListener::bind(admin::parse_listen(&admin_listen)?).await?;
    info!(
        plugins = ?state.dispatcher.names().await,
        admin = %admin_listen,
        "fujiang running"
    );
    {
        let admin_state = state.clone();
        let admin_stop = process_stop.clone();
        tokio::spawn(async move {
            if let Err(e) = admin::serve(listener, admin_state, admin_stop).await {
                tracing::error!(error = %e, "admin ui stopped");
            }
        });
    }

    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                let d = state.dispatcher.clone();
                let ctx = state.ctx.clone();
                tokio::spawn(async move {
                    d.handle(&ctx, &ev).await;
                });
            }
            _ = process_stop.cancelled() => {
                info!("shutting down");
                break;
            }
        }
    }
    state.dispatcher.stop_all().await;
    state.stop_gateway().await;
    Ok(())
}

async fn migrate(from: PathBuf, path: PathBuf) -> anyhow::Result<()> {
    let cfg = AppConfig::load(&path)?;
    let store = Store::open_opts(StoreOpts {
        db: cfg.store.db.clone(),
        image_root: PathBuf::from(&cfg.store.image_root),
        archive_dir: PathBuf::from(&cfg.store.archive_dir),
        archive_after_days: cfg.store.archive_after_days,
        archive_every_hours: cfg.store.archive_every_hours,
    })
    .await?;
    let report = fujiang_store::migrate_from_python(&store, &from).await?;
    println!("{report}");
    Ok(())
}
