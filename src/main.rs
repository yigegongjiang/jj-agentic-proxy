//! 本机 agentic proxy: 复用自有订阅, 以官方协议暴露 Anthropic / OpenAI Codex 端点。

mod auth;
mod convert;
mod daemon;
mod oauth;
mod provider;
mod proxy;
mod reqlog;
mod server;
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
use crate::provider::{Provider, Surface};
use crate::store::now;

#[derive(Parser)]
#[command(
    name = "jj-agentic-proxy",
    version,
    about = "本机 agentic proxy: 复用自有订阅, 为其他 app 提供 Anthropic / OpenAI Codex 官方协议端点",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 启动 (默认命令): 后台常驻, 端口固定; 已在运行则先停再起 (= restart)
    Start,
    /// 停止
    Stop,
    /// 后台进程本体, 由 start 拉起 (不直接使用)
    #[command(hide = true)]
    Serve,
    /// Web OAuth 授权: anthropic | codex
    Login { provider: String },
    /// 删除本地凭证: anthropic | codex | all
    Logout { provider: String },
    /// 查看运行与凭证状态
    Status,
    /// 列出两家订阅当前可用的 model
    Models,
    /// 打印最近的 req/res 往返记录摘要
    Logs {
        /// 条数
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Start) {
        Cmd::Start => daemon::start(),
        Cmd::Stop => daemon::stop(),
        Cmd::Serve => serve().await,
        Cmd::Login { provider } => login(&provider).await,
        Cmd::Logout { provider } => logout(&provider),
        Cmd::Status => status(),
        Cmd::Models => models().await,
        Cmd::Logs { count } => reqlog::print_tail(count),
    }
}

/// 一个 provider 一个固定端口 -> 客户端 base url 写死即可, 无需任何参数。
async fn serve() -> Result<()> {
    init_tracing()?;
    // 独占 pid 锁 -> 第二个实例立刻失败, 且 stop / status 能凭内核锁准确判活。
    let mut instance = daemon::acquire()?;

    // 长期空跑也不留下过期记录 (平时的清理跟随写入发生)。
    reqlog::sweep();

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

    // 先全部 bind 再 serve: 端口被占用要立刻失败, 不留半个可用端口的中间态。
    let mut bound = Vec::new();
    for s in Surface::ALL {
        bound.push((s, bind(s.port()).await?));
    }

    let mut tasks = Vec::new();
    for (s, listener) in bound {
        tracing::info!(
            "{s:<13} listening on http://{}:{}",
            provider::HOST,
            s.port()
        );
        let port = proxy::Port {
            app: app.clone(),
            surface: s,
        };
        tasks.push((
            s,
            tokio::spawn(
                axum::serve(listener, server::router(port))
                    .with_graceful_shutdown(shutdown())
                    .into_future(),
            ),
        ));
    }

    daemon::mark_ready(&mut instance)?;

    for (s, task) in tasks {
        task.await
            .with_context(|| format!("{s} 端口任务异常"))?
            .with_context(|| format!("{s} 端口服务异常退出"))?;
    }
    Ok(())
}

/// 前台 -> stderr; 后台 (由 `start` 注入日志路径) -> 封顶日志文件, 无 ANSI。
fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match std::env::var(daemon::LOG_ENV) {
        Ok(path) if !path.is_empty() => {
            let log = daemon::CappedLog::open(path.clone().into())
                .with_context(|| format!("打开日志失败: {path}"))?;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(log))
                .init();
        }
        _ => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    Ok(())
}

async fn bind(port: u16) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((provider::HOST, port))
        .await
        .with_context(|| {
            format!(
                "监听 {}:{port} 失败 (端口固定, 需先释放占用)",
                provider::HOST
            )
        })
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
    match daemon::running()? {
        Some(pid) => {
            println!("运行中 (pid {pid})");
            for s in Surface::ALL {
                println!("- {s:<13} http://{}:{}", provider::HOST, s.port());
            }
            println!("日志: {}", daemon::log_path().display());
        }
        None => println!("未运行 (`jj-agentic-proxy start` 后台启动)"),
    }
    println!(
        "往返记录: {} (最近 {} 天)",
        reqlog::log_dir().display(),
        reqlog::KEEP_DAYS
    );
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

/// 直接拿本机凭证问上游要列表 -> 与后台是否在跑无关, 结果和 `GET /v1/models` 同源。
async fn models() -> Result<()> {
    let http = http_client()?;
    let auth = AuthManager::load(http.clone())?;
    let app = Arc::new(proxy::App {
        auth,
        http,
        session_id: new_session_id(),
    });
    for p in Provider::ALL {
        let ports = Surface::ALL
            .iter()
            .filter(|s| s.provider() == p)
            .map(|s| s.port().to_string())
            .collect::<Vec<_>>()
            .join(" / ");
        println!("{p} (端口 {ports})");
        if app.auth.snapshot(p).await.is_none() {
            println!("  未登录 (`jj-agentic-proxy login {p}`)");
            continue;
        }
        let list = server::list(&app, p).await;
        if list.is_empty() {
            println!("  取不到列表 (上游异常或凭证失效)");
        }
        // 上游顺序在 `/v1/models` 保持原样 (客户端据此选默认模型); 只有 CLI 打印重排。
        let mut names: Vec<&str> = list
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect::<Vec<_>>();
        names.sort_by(|a, b| natural_cmp(a, b));
        for name in names {
            println!("  {name}");
        }
    }
    Ok(())
}

/// 自然序: 数字段按数值比, 其余按字节。
///
/// 纯字典序会把 `claude-opus-4-10` 排在 `4-8` 前面; 按数值比才能让同族版本顺次排列。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&p), Some(&q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let (np, rest_x) = take_number(x);
                let (nq, rest_y) = take_number(y);
                if np != nq {
                    return np.cmp(&nq);
                }
                (x, y) = (rest_x, rest_y);
            }
            (Some(&p), Some(&q)) => {
                if p != q {
                    return p.cmp(&q);
                }
                (x, y) = (&x[1..], &y[1..]);
            }
        }
    }
}

/// 溢出退化成 `u128::MAX` (模型名里的日期串远够用), 保证不 panic。
fn take_number(s: &[u8]) -> (u128, &[u8]) {
    let end = s
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(s.len());
    let n = std::str::from_utf8(&s[..end])
        .ok()
        .and_then(|d| d.parse::<u128>().ok())
        .unwrap_or(u128::MAX);
    (n, &s[end..])
}

fn parse_provider(name: &str) -> Result<Provider> {
    Provider::parse(name).map_or_else(
        || bail!("未知 provider `{name}`; 可用: anthropic | codex"),
        Ok,
    )
}

/// 无总超时: SSE 长连接不能被打断; 只约束建连与单次读取空闲。
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(300))
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
    fn model_names_sort_by_family_then_version() {
        let mut got = vec![
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-10",
            "claude-opus-4-8",
            "gpt-5.6-sol",
            "gpt-5.4-mini",
            "gpt-5.4",
        ];
        got.sort_by(|a, b| super::natural_cmp(a, b));
        assert_eq!(
            got,
            vec![
                "claude-opus-4-8",
                // 数值序: 10 在 8 之后 (字典序会排到 8 前面)
                "claude-opus-4-10",
                "claude-opus-5",
                "claude-sonnet-5",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.6-sol",
            ]
        );
    }

    #[test]
    fn oversized_number_does_not_panic() {
        let huge = "m-".to_string() + &"9".repeat(60);
        assert_ne!(super::natural_cmp(&huge, "m-1"), std::cmp::Ordering::Less);
    }

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
