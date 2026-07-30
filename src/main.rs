//! 本机 agentic proxy: 复用自有订阅, 以官方协议暴露 Anthropic / OpenAI Codex 端点。

mod auth;
mod compat;
mod convert;
mod oauth;
mod provider;
mod proxy;
mod sse;
mod store;

use std::future::IntoFuture as _;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rand::RngCore as _;
use tracing_subscriber::EnvFilter;

use crate::auth::AuthManager;
use crate::provider::Provider;
use crate::store::now;

#[derive(Parser)]
#[command(
    name = "jj-agentic-proxy",
    version,
    about = "本机 agentic proxy: 复用自有订阅, 为其他 app 提供 Anthropic / OpenAI Codex 官方协议端点"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 启动代理 (默认命令)
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// 原生协议端口 (官方 body 原样透传)
        #[arg(long, short, default_value_t = 10000)]
        port: u16,
        /// api key 兼容端口 (Chat Completions + 原生); 0 = 关闭
        #[arg(long, default_value_t = 10001)]
        compat_port: u16,
    },
    /// Web OAuth 授权: anthropic | codex
    Login { provider: String },
    /// 删除本地凭证: anthropic | codex | all
    Logout { provider: String },
    /// 查看本地凭证状态
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve {
        host: "127.0.0.1".into(),
        port: 10000,
        compat_port: 10001,
    }) {
        Cmd::Serve {
            host,
            port,
            compat_port,
        } => serve(host, port, compat_port).await,
        Cmd::Login { provider } => login(&provider).await,
        Cmd::Logout { provider } => logout(&provider),
        Cmd::Status => status(),
    }
}

async fn serve(host: String, port: u16, compat_port: u16) -> Result<()> {
    if compat_port == port {
        bail!("--compat-port 不能与 --port 相同");
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let http = http_client()?;
    let auth = AuthManager::load(http.clone())?;

    for p in Provider::ALL {
        match auth.snapshot(p).await {
            Some(c) => tracing::info!(
                provider = %p,
                account = c.account.as_deref().unwrap_or("-"),
                "凭证已加载"
            ),
            None => tracing::warn!(provider = %p, "未登录, 执行 `jj-agentic-proxy login {p}`"),
        }
    }

    let app = Arc::new(proxy::App {
        auth,
        http,
        session_id: new_session_id(),
    });
    let native = bind(&host, port).await?;
    tracing::info!("native  listening on http://{host}:{port} (官方协议原样透传)");
    let native = tokio::spawn(
        axum::serve(native, proxy::router(app.clone()))
            .with_graceful_shutdown(shutdown())
            .into_future(),
    );

    let compat = match compat_port {
        0 => None,
        p => {
            let l = bind(&host, p).await?;
            tracing::info!(
                "compat  listening on http://{host}:{p} (api key 风格; 含 /v1/chat/completions)"
            );
            Some(tokio::spawn(
                axum::serve(l, compat::router(app))
                    .with_graceful_shutdown(shutdown())
                    .into_future(),
            ))
        }
    };

    native
        .await
        .context("原生端口任务异常")?
        .context("原生端口服务异常退出")?;
    if let Some(task) = compat {
        task.await
            .context("兼容端口任务异常")?
            .context("兼容端口服务异常退出")?;
    }
    Ok(())
}

async fn bind(host: &str, port: u16) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("监听 {host}:{port} 失败"))
}

async fn login(name: &str) -> Result<()> {
    let p = parse_provider(name)?;
    let http = http_client()?;
    let manager = AuthManager::load(http.clone())?;
    let cred = oauth::login(&http, p).await?;
    let account = cred.account.clone().unwrap_or_else(|| "-".into());
    manager.set(p, cred).await?;
    println!("{p} 授权完成: {account}");
    println!("凭证已写入 {}", store::auth_path().display());
    Ok(())
}

fn logout(name: &str) -> Result<()> {
    let targets = if name.eq_ignore_ascii_case("all") {
        Provider::ALL.to_vec()
    } else {
        vec![parse_provider(name)?]
    };
    for p in targets {
        if store::remove(p)? {
            println!("{p} 凭证已删除");
        } else {
            println!("{p} 无本地凭证");
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let store = store::load()?;
    println!("store: {}", store::auth_path().display());
    for p in Provider::ALL {
        match store.get(p.key()) {
            None => println!("- {p}: 未登录"),
            Some(c) => {
                let left = c.expires_at.saturating_sub(now());
                let state = if left == 0 {
                    "已过期(下次请求自动刷新)".to_string()
                } else {
                    format!("{}m {}s 后到期", left / 60, left % 60)
                };
                println!(
                    "- {p}: {} | plan={} | {state}",
                    c.account.as_deref().unwrap_or("-"),
                    c.plan.as_deref().unwrap_or("-"),
                );
            }
        }
    }
    Ok(())
}

fn parse_provider(name: &str) -> Result<Provider> {
    Provider::parse(name).map_or_else(
        || bail!("未知 provider `{name}`; 可用: anthropic | codex"),
        Ok,
    )
}

/// 无总超时: SSE 长连接不能被打断; 只约束建连与空闲。
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .context("构建 HTTP client 失败")
}

/// 形似一次 CLI 会话的 UUID v4。
fn new_session_id() -> String {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_id_is_uuid_v4_shaped() {
        let id = super::new_session_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
    }
}
