use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::AppConfig;
use crate::runtime::{AppState, ApplyReport};

pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    info!(%addr, "admin ui");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            stop.cancelled().await;
        })
        .await?;
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/status", get(get_status))
        .route("/api/plugins", get(get_plugins))
        .route("/api/plugins/scan", post(scan_plugins))
        .route("/api/plugins/load", post(load_plugin))
        .route("/api/plugins/unload", post(unload_plugin))
        .route("/api/plugins/reload", post(reload_plugin))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/", get(index))
        .merge(api)
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

#[derive(Serialize)]
struct ConfigView {
    config: AppConfig,
    status: StatusView,
    restart_fields: &'static [&'static str],
}

#[derive(Serialize)]
struct StatusView {
    plugins: Vec<String>,
    backend: crate::config::AdapterBackend,
    config_path: String,
    admin_token_required: bool,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<ConfigView> {
    Json(view(&state).await)
}

async fn get_status(State(state): State<Arc<AppState>>) -> Json<StatusView> {
    Json(status_view(&state).await)
}

#[derive(Serialize)]
struct PluginsView {
    dir: String,
    watch: bool,
    builtin: Vec<fujiang_core::PluginSnapshot>,
    dynamic: Vec<crate::dynload::DynamicPluginView>,
}

#[derive(serde::Deserialize)]
struct PathBody {
    path: String,
}

#[derive(serde::Deserialize)]
struct NameBody {
    name: String,
}

async fn get_plugins(State(state): State<Arc<AppState>>) -> Json<PluginsView> {
    Json(plugins_view(&state).await)
}

async fn scan_plugins(State(state): State<Arc<AppState>>) -> Response {
    match state.hub.scan(&state.dispatcher, &state.ctx).await {
        Ok(changed) => Json(serde_json::json!({
            "ok": true,
            "changed": changed,
            "plugins": plugins_view(&state).await,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

async fn load_plugin(State(state): State<Arc<AppState>>, Json(body): Json<PathBody>) -> Response {
    match state
        .hub
        .load(
            std::path::Path::new(&body.path),
            &state.dispatcher,
            &state.ctx,
        )
        .await
    {
        Ok(name) => Json(serde_json::json!({
            "ok": true,
            "name": name,
            "plugins": plugins_view(&state).await,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

async fn unload_plugin(State(state): State<Arc<AppState>>, Json(body): Json<NameBody>) -> Response {
    match state.hub.unload(&body.name, &state.dispatcher).await {
        Ok(true) => Json(serde_json::json!({
            "ok": true,
            "plugins": plugins_view(&state).await,
        }))
        .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("未加载「{}」", body.name),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

async fn reload_plugin(State(state): State<Arc<AppState>>, Json(body): Json<NameBody>) -> Response {
    match state
        .hub
        .reload(&body.name, &state.dispatcher, &state.ctx)
        .await
    {
        Ok(name) => Json(serde_json::json!({
            "ok": true,
            "name": name,
            "plugins": plugins_view(&state).await,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

async fn plugins_view(state: &AppState) -> PluginsView {
    let cfg = state.cfg.read().await;
    let running = state.dispatcher.names().await;
    let all = state.dispatcher.info_list().await;
    let builtin = all
        .into_iter()
        .filter(|p| crate::plugins::NAMES.contains(&p.name.as_str()))
        .collect();
    PluginsView {
        dir: cfg.plugins.dir.clone(),
        watch: cfg.plugins.watch,
        builtin,
        dynamic: state.hub.list(&running).await,
    }
}

async fn put_config(State(state): State<Arc<AppState>>, Json(body): Json<AppConfig>) -> Response {
    match state.apply(body).await {
        Ok(report) => {
            let body = ApplyOk {
                ok: true,
                report,
                view: view(&state).await,
            };
            Json(body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct ApplyOk {
    ok: bool,
    #[serde(flatten)]
    report: ApplyReport,
    view: ConfigView,
}

async fn view(state: &AppState) -> ConfigView {
    let cfg = state.cfg.read().await.clone();
    ConfigView {
        config: cfg.redacted(),
        status: status_of(state, &cfg).await,
        restart_fields: &[
            "store.db",
            "store.image_root",
            "store.archive_dir",
            "admin.listen",
            "plugins.dir",
            "plugins.watch",
        ],
    }
}

async fn status_view(state: &AppState) -> StatusView {
    let cfg = state.cfg.read().await.clone();
    status_of(state, &cfg).await
}

async fn status_of(state: &AppState, cfg: &AppConfig) -> StatusView {
    StatusView {
        plugins: state.dispatcher.names().await,
        backend: cfg.adapter.backend,
        config_path: state.config_path.display().to_string(),
        admin_token_required: !cfg.admin.token.is_empty(),
    }
}

async fn require_token(
    State(state): State<Arc<AppState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let expected = state.cfg.read().await.admin.token.clone();
    if expected.is_empty() {
        return next.run(req).await;
    }
    let got = req
        .headers()
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            req.headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .map(|s| s.to_string())
        });
    if got.as_deref() == Some(expected.as_str()) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "unauthorized".into(),
            }),
        )
            .into_response()
    }
}

pub fn parse_listen(s: &str) -> anyhow::Result<SocketAddr> {
    s.parse()
        .map_err(|e| anyhow::anyhow!("admin.listen `{s}`: {e}"))
}
