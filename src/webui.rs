//! Web 查看器: 浏览器里的查看器 app, 与 `app/` 的 macOS 版同源同数据。
//!
//! 数据只读: 记录由代理自己写, 本模块只按 (offset, length) 取行, 从不回写。
//! 语义解析 (原始报文拼装 / SSE 重建 / 三方言归一) 全在前端做 -> 本层零解析负担,
//! 也不必在 Rust 与 Swift 之间维护第二份渲染实现。
//!
//! 服务操作转调本进程内的同一套实现 (`oauth` / `store`), 不 exec 自己的 CLI:
//! 同一份代码路径, 不存在「app 与 CLI 判断不一致」的缝。
//! `start` 例外 —— 页面由本进程提供, 进程没起来时页面也不存在, 天然只能在终端执行。

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use crate::provider::{self, Provider, Surface};
use crate::proxy::App;
use crate::store::{self, now};
use crate::{daemon, oauth, reqlog, server};

const EXT: &str = ".jsonl";
/// 索引扫描块大小: 单日文件可以是 GB 级 (body 全量且永不清理), 不整读进内存。
const CHUNK: usize = 1 << 20;
/// 摘要段固定在行首几百字节内 -> 限定搜索窗口, 不在 MB 级 body 里扫标记。
const SUMMARY_WINDOW: usize = 4096;
const SUMMARY_MARKER: &[u8] = br#","req_headers":"#;

pub fn router(app: Arc<App>) -> Router {
    let ui = Arc::new(Ui {
        app,
        login: Mutex::new(LoginState::idle()),
    });
    Router::new()
        .route("/", get(page))
        .route("/api/status", get(status))
        .route("/api/days", get(day_list))
        // 参数走路径而非 query: axum 的 query 提取要额外开 feature, 这里只有定长标量, 不值当
        .route("/api/scan/{day}/{from}/{seq}", get(scan_api))
        .route("/api/detail/{day}/{offset}/{length}", get(detail_api))
        .route("/api/models", get(models))
        .route("/api/login", get(login_state))
        .route("/api/login/{provider}", post(login_start))
        .route("/api/logout/{provider}", post(logout))
        .route("/api/stop", post(stop))
        .fallback(get(page))
        .with_state(ui)
}

struct Ui {
    app: Arc<App>,
    login: Mutex<LoginState>,
}

impl Ui {
    fn login_state(&self) -> LoginState {
        self.login.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_login(&self, next: LoginState) {
        *self.login.lock().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

type Ctx = State<Arc<Ui>>;

async fn page() -> Html<&'static str> {
    Html(include_str!("webui/app.html"))
}

// ---------- 状态 ----------

#[derive(Serialize)]
struct StatusOut {
    pid: u32,
    endpoints: Vec<EndpointOut>,
    ui_port: u16,
    lan_ip: Option<String>,
    daemon_log: String,
    record_dir: String,
    auth_path: String,
    creds: Vec<CredOut>,
}

#[derive(Serialize)]
struct EndpointOut {
    surface: String,
    port: u16,
}

#[derive(Serialize)]
struct CredOut {
    provider: String,
    ports: String,
    logged_in: bool,
    account: Option<String>,
    plan: Option<String>,
    /// 距到期秒数; 0 = 已过期 (下次请求自动刷新)
    expires_in: u64,
}

/// 页面能打开就说明本进程活着 -> pid 直接取自身。
///
/// 刻意不走 `daemon::running()`: 那条路径用 `try_lock` 试探 pid 文件锁, 一旦在持锁进程内
/// 侥幸拿到锁就会顺手 unlock, 把自己的单实例保护解掉。
async fn status() -> Response {
    // 与 `status` 子命令同源: 读文件而非内存态 -> 外部 CLI 的 login/logout 立刻反映出来。
    let store = match store::load() {
        Ok(s) => s,
        Err(e) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("读取凭证失败: {e}"),
            )
        }
    };
    let creds = Provider::ALL
        .iter()
        .map(|p| {
            let ports = Surface::ALL
                .iter()
                .filter(|s| s.provider() == *p)
                .map(|s| s.port().to_string())
                .collect::<Vec<_>>()
                .join(" / ");
            match store.get(p.key()) {
                None => CredOut {
                    provider: p.key().to_string(),
                    ports,
                    logged_in: false,
                    account: None,
                    plan: None,
                    expires_in: 0,
                },
                Some(c) => CredOut {
                    provider: p.key().to_string(),
                    ports,
                    logged_in: true,
                    account: c.account.clone(),
                    plan: c.plan.clone(),
                    expires_in: c.expires_at.saturating_sub(now()),
                },
            }
        })
        .collect();
    Json(StatusOut {
        pid: std::process::id(),
        endpoints: Surface::ALL
            .iter()
            .map(|s| EndpointOut {
                surface: s.key().to_string(),
                port: s.port(),
            })
            .collect(),
        ui_port: provider::UI_PORT,
        lan_ip: crate::lan_ip().map(|ip| ip.to_string()),
        daemon_log: daemon::log_path().display().to_string(),
        record_dir: reqlog::log_dir().display().to_string(),
        auth_path: store::auth_path().display().to_string(),
        creds,
    })
    .into_response()
}

// ---------- 往返记录 (只读) ----------

async fn day_list() -> Response {
    match tokio::task::spawn_blocking(days).await {
        Ok(days) => Json(json!({ "days": days })).into_response(),
        Err(e) => fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("枚举日期失败: {e}"),
        ),
    }
}

/// `from` = 上次读到的字节位置 -> follow 只读新追加的那段。
async fn scan_api(Path((day, from, seq)): Path<(String, u64, usize)>) -> Response {
    if !valid_day(&day) {
        return fail(StatusCode::BAD_REQUEST, "day 必须是 YYYY-MM-DD");
    }
    match tokio::task::spawn_blocking(move || scan(&day, from, seq)).await {
        Ok(batch) => Json(batch).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("扫描失败: {e}")),
    }
}

/// 整行原文交给前端: 两种读法 (原始报文 / 核心内容) 都由前端从同一份 JSON 渲染,
/// 切换视图不再回后端, 也不会出现两份解析实现对不上的情况。
async fn detail_api(Path((day, offset, length)): Path<(String, u64, usize)>) -> Response {
    if !valid_day(&day) {
        return fail(StatusCode::BAD_REQUEST, "day 必须是 YYYY-MM-DD");
    }
    match tokio::task::spawn_blocking(move || read_line(&day, offset, length)).await {
        Ok(Some(raw)) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            raw,
        )
            .into_response(),
        Ok(None) => fail(StatusCode::NOT_FOUND, "读不到这一行 (文件已变化?)"),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("读取失败: {e}")),
    }
}

#[derive(Serialize)]
struct Batch {
    records: Vec<RecordOut>,
    consumed: u64,
    next_seq: usize,
    /// 文件被换掉 (变短) -> 前端丢弃已有记录重建
    reset: bool,
}

/// 列表只要摘要标量; 完整 body 留在文件里按 (offset, length) 现取。
#[derive(Serialize)]
struct RecordOut {
    seq: usize,
    offset: u64,
    length: usize,
    ts: String,
    surface: String,
    method: String,
    path: String,
    /// 0 = 没等到响应 (客户端提前断开)
    status: u64,
    stream: bool,
    elapsed_ms: u64,
    req_bytes: u64,
    res_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    incomplete: Option<String>,
}

fn log_dir() -> PathBuf {
    reqlog::log_dir()
}

fn day_file(day: &str) -> PathBuf {
    log_dir().join(format!("{day}{EXT}"))
}

/// 只认 `YYYY-MM-DD`: 日期是拼进文件名的, 放任别的形状等于允许读任意路径。
fn valid_day(day: &str) -> bool {
    day.len() == 10
        && day.as_bytes().iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// 已有记录的日期, 新 -> 旧。
fn days() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(log_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(EXT).map(str::to_string))
        .filter(|d| valid_day(d))
        .collect();
    out.sort_by(|a, b| b.cmp(a));
    out
}

fn scan(day: &str, from: u64, start_seq: usize) -> Batch {
    scan_file(&day_file(day), from, start_seq)
}

/// 从 `from` 字节处继续索引。尾部未换行的半行留给下一次 (写入方正在追加)。
fn scan_file(path: &std::path::Path, from: u64, start_seq: usize) -> Batch {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let reset = size < from;
    let cursor = if reset { 0 } else { from };
    let mut batch = Batch {
        records: Vec::new(),
        consumed: cursor,
        next_seq: if reset { 0 } else { start_seq },
        reset,
    };
    if size <= cursor {
        return batch;
    }
    let Ok(mut f) = File::open(path) else {
        return batch;
    };
    if f.seek(SeekFrom::Start(cursor)).is_err() {
        return batch;
    }

    let mut seq = batch.next_seq;
    let mut pending: Vec<u8> = Vec::new();
    let mut line_start = cursor;
    let mut base = cursor; // 当前块首字节的绝对偏移
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let bytes = &buf[..n];
        let mut i = 0;
        while i < n {
            match bytes[i..].iter().position(|b| *b == b'\n') {
                None => {
                    pending.extend_from_slice(&bytes[i..]);
                    break;
                }
                Some(rel) => {
                    let nl = i + rel;
                    pending.extend_from_slice(&bytes[i..nl]);
                    // 解析失败不占号: seq 只在真的收下一条时前进
                    if let Some(rec) = parse_record(&pending, seq, line_start) {
                        batch.records.push(rec);
                        seq += 1;
                    }
                    pending.clear();
                    line_start = base + nl as u64 + 1;
                    i = nl + 1;
                }
            }
        }
        base += n as u64;
    }
    batch.consumed = line_start;
    batch.next_seq = seq;
    batch
}

/// 写入侧刻意把摘要字段排在 `req` 之前 -> 只解析行首那截, 不碰大 body。
/// 切不出摘要段 (格式变了) 时回退整行解析, 宁慢不丢记录。
fn parse_record(line: &[u8], seq: usize, offset: u64) -> Option<RecordOut> {
    let head = summary_head(line);
    let v: Value = serde_json::from_slice(head.as_deref().unwrap_or(line)).ok()?;
    let ts = v.get("ts")?.as_str()?.to_string();
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let opt = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(RecordOut {
        seq,
        offset,
        length: line.len(),
        ts,
        surface: text("surface"),
        method: text("method"),
        path: text("path"),
        status: num("status"),
        stream: v.get("stream").and_then(Value::as_bool).unwrap_or(false),
        elapsed_ms: num("elapsed_ms"),
        req_bytes: num("req_bytes"),
        res_bytes: num("res_bytes"),
        model: opt("model"),
        incomplete: opt("incomplete"),
    })
}

/// 截到 `,"req_headers":` (第一个非摘要字段) 之前并补 `}` -> 一个只含标量的小对象。
fn summary_head(line: &[u8]) -> Option<Vec<u8>> {
    let window = &line[..line.len().min(SUMMARY_WINDOW)];
    let at = window
        .windows(SUMMARY_MARKER.len())
        .position(|w| w == SUMMARY_MARKER)?;
    let mut head = line[..at].to_vec();
    head.push(b'}');
    Some(head)
}

fn read_line(day: &str, offset: u64, length: usize) -> Option<Vec<u8>> {
    read_line_at(&day_file(day), offset, length)
}

fn read_line_at(path: &std::path::Path, offset: u64, length: usize) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(length.min(1 << 20));
    // read_exact 会因文件被截断而整条丢失 -> take + 读到多少算多少
    f.take(length as u64).read_to_end(&mut buf).ok()?;
    (!buf.is_empty()).then_some(buf)
}

// ---------- 模型列表 ----------

async fn models(State(ui): Ctx) -> Response {
    let mut out = Vec::new();
    for p in Provider::ALL {
        let ports = Surface::ALL
            .iter()
            .filter(|s| s.provider() == p)
            .map(|s| s.port().to_string())
            .collect::<Vec<_>>()
            .join(" / ");
        if ui.app.auth.snapshot(p).await.is_none() {
            out.push(json!({
                "provider": p.key(), "ports": ports, "logged_in": false, "models": [],
            }));
            continue;
        }
        let list = server::list(&ui.app, p).await;
        // 上游顺序在 `/v1/models` 保持原样; 只有给人看的列表重排。
        let mut names: Vec<&str> = list.iter().filter_map(|m| m["id"].as_str()).collect();
        names.sort_by(|a, b| crate::natural_cmp(a, b));
        out.push(json!({
            "provider": p.key(), "ports": ports, "logged_in": true, "models": names,
        }));
    }
    Json(json!({ "providers": out })).into_response()
}

// ---------- 登录 / 登出 / 停止 ----------

/// 一次登录的进度。回调端口写死在上游 allow-list -> 浏览器必须开在本机,
/// 所以页面只负责发起与显示 URL, 授权动作在跑着代理的那台机器上完成。
#[derive(Clone, Serialize)]
struct LoginState {
    state: &'static str,
    provider: Option<String>,
    url: Option<String>,
    message: Option<String>,
}

impl LoginState {
    fn idle() -> Self {
        Self {
            state: "idle",
            provider: None,
            url: None,
            message: None,
        }
    }
}

async fn login_state(State(ui): Ctx) -> Response {
    Json(ui.login_state()).into_response()
}

async fn login_start(State(ui): Ctx, Path(name): Path<String>) -> Response {
    let Some(p) = Provider::parse(&name) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "未知 provider; 可用: anthropic | codex",
        );
    };
    {
        let mut guard = ui.login.lock().unwrap_or_else(|e| e.into_inner());
        if guard.state == "waiting" {
            return (StatusCode::CONFLICT, Json(guard.clone())).into_response();
        }
        *guard = LoginState {
            state: "waiting",
            provider: Some(p.key().to_string()),
            url: None,
            message: Some("正在准备授权链接…".into()),
        };
    }

    let task = ui.clone();
    tokio::spawn(async move {
        let http = task.app.http.clone();
        let sink = task.clone();
        let result = oauth::login_reporting(&http, p, move |url| {
            sink.set_login(LoginState {
                state: "waiting",
                provider: Some(p.key().to_string()),
                url: Some(url.to_string()),
                message: Some("在本机浏览器完成授权 (最多 5 分钟)".into()),
            });
        })
        .await;
        let next = match result {
            Ok(cred) => {
                let account = cred.account.clone().unwrap_or_else(|| "-".into());
                match task.app.auth.set(p, cred).await {
                    Ok(()) => LoginState {
                        state: "done",
                        provider: Some(p.key().to_string()),
                        url: None,
                        message: Some(format!("{p} 授权完成: {account}")),
                    },
                    Err(e) => LoginState {
                        state: "error",
                        provider: Some(p.key().to_string()),
                        url: None,
                        message: Some(format!("凭证写入失败: {e}")),
                    },
                }
            }
            Err(e) => LoginState {
                state: "error",
                provider: Some(p.key().to_string()),
                url: None,
                message: Some(format!("{e}")),
            },
        };
        task.set_login(next);
    });

    Json(ui.login_state()).into_response()
}

/// 只删文件里的凭证: 代理每次取 token 都重读 `auth.json`, 内存态无需另行清理。
async fn logout(Path(name): Path<String>) -> Response {
    let targets = if name.eq_ignore_ascii_case("all") {
        Provider::ALL.to_vec()
    } else {
        match Provider::parse(&name) {
            Some(p) => vec![p],
            None => {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "未知 provider; 可用: anthropic | codex | all",
                )
            }
        }
    };
    let mut messages = Vec::new();
    for p in targets {
        match store::remove(p) {
            Ok(true) => messages.push(format!("{p} 凭证已删除")),
            Ok(false) => messages.push(format!("{p} 无本地凭证")),
            Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{p}: {e}")),
        }
    }
    Json(json!({ "messages": messages })).into_response()
}

/// 走与 `stop` 子命令完全相同的路径 (SIGTERM -> graceful shutdown),
/// 延后一拍发信号, 让这条响应先出门。
async fn stop() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        // SAFETY: raise 只给本进程递信号, 无参数无返回值资源。
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    });
    Json(json!({
        "ok": true,
        "message": "已发送停止信号; 重新启动需在终端执行 `jj-agentic-proxy start`",
    }))
    .into_response()
}

fn fail(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn only_iso_days_are_accepted() {
        assert!(valid_day("2026-08-04"));
        // 放任别的形状 = 允许把任意路径拼进文件名
        for bad in [
            "2026-8-4",
            "../../etc/passwd",
            "2026-08-04.jsonl",
            "",
            "20260804..",
        ] {
            assert!(!valid_day(bad), "{bad} 不该通过");
        }
    }

    #[test]
    fn summary_head_stops_before_body() {
        let line = br#"{"ts":"t","status":200,"req_headers":{"a":"b"},"req":{"big":1}}"#;
        let head = summary_head(line).expect("应切出摘要段");
        let v: Value = serde_json::from_slice(&head).unwrap();
        assert_eq!(v["status"], json!(200));
        assert!(v.get("req_headers").is_none());
        // 标记不在窗口内 -> 交给调用方整行解析
        let mut far = br#"{"ts":"t","pad":""#.to_vec();
        far.extend(std::iter::repeat_n(b'x', SUMMARY_WINDOW));
        far.extend_from_slice(br#"","req_headers":{}}"#);
        assert!(summary_head(&far).is_none());
    }

    #[test]
    fn record_falls_back_to_whole_line_without_marker() {
        let line = br#"{"ts":"2026-08-04T10:00:00.000+09:00","surface":"codex","method":"POST","path":"/v1/responses","status":200,"stream":true,"elapsed_ms":12,"req_bytes":3,"res_bytes":4,"model":"gpt"}"#;
        let rec = parse_record(line, 7, 42).expect("整行也应解析出来");
        assert_eq!((rec.seq, rec.offset, rec.length), (7, 42, line.len()));
        assert_eq!(rec.model.as_deref(), Some("gpt"));
        assert!(rec.stream);
        assert!(rec.incomplete.is_none());
        // 没有 ts 的行不是记录
        assert!(parse_record(br#"{"surface":"codex"}"#, 0, 0).is_none());
        assert!(parse_record(b"not json", 0, 0).is_none());
    }

    /// 增量扫描: 续读只吃新追加的那段, 且不把正在写的半行当成记录。
    #[test]
    fn scan_resumes_and_leaves_partial_line() {
        let dir = std::env::temp_dir().join(format!("jj-webui-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-08-04.jsonl");
        let line = |i: usize| {
            format!(
                r#"{{"ts":"2026-08-04T10:00:0{i}.000+09:00","surface":"codex","method":"POST","path":"/v1/responses","status":200,"req_headers":{{}},"req":null}}"#
            )
        };

        let mut f = File::create(&path).unwrap();
        writeln!(f, "{}", line(0)).unwrap();
        writeln!(f, "{}", line(1)).unwrap();
        write!(f, "{}", line(2)).unwrap(); // 半行: 还没换行
        f.flush().unwrap();

        let first = scan_file(&path, 0, 0);
        assert_eq!(first.records.len(), 2);
        assert_eq!(first.next_seq, 2);
        assert!(!first.reset);
        // 半行不计入 consumed -> 下次从它的行首重读
        assert_eq!(first.consumed, (line(0).len() + line(1).len() + 2) as u64);

        // 补上换行 + 再追加一条 -> 续读只拿到后两条
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f).unwrap();
        writeln!(f, "{}", line(3)).unwrap();
        f.flush().unwrap();
        let second = scan_file(&path, first.consumed, first.next_seq);
        assert_eq!(second.records.len(), 2);
        assert_eq!(second.records[0].seq, 2);
        assert_eq!(second.next_seq, 4);

        // 单行大于一个读块 -> 必须跨块拼回
        let big = format!(
            r#"{{"ts":"2026-08-04T11:00:00.000+09:00","surface":"codex","method":"POST","path":"/v1/responses","status":200,"pad":"{}","req_headers":{{}},"req":null}}"#,
            "x".repeat(CHUNK + 1024)
        );
        std::fs::write(&path, format!("{big}\n")).unwrap();
        let across = scan_file(&path, 0, 0);
        assert_eq!(across.records.len(), 1);
        assert_eq!(across.records[0].length, big.len());
        assert_eq!(across.records[0].surface, "codex");

        // 文件被换成更短的 -> 要求前端重建
        std::fs::write(&path, "").unwrap();
        let third = scan_file(&path, across.consumed, across.next_seq);
        assert!(third.reset);
        assert_eq!(third.next_seq, 0);
        assert!(third.records.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_line_survives_truncated_tail() {
        let dir = std::env::temp_dir().join(format!("jj-webui-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-08-04.jsonl");
        std::fs::write(&path, "abcdef\n").unwrap();

        assert_eq!(read_line_at(&path, 0, 6).as_deref(), Some(&b"abcdef"[..]));
        // 要的比文件剩下的多 -> 给到多少算多少, 不整条丢
        assert_eq!(read_line_at(&path, 3, 999).as_deref(), Some(&b"def\n"[..]));
        assert!(read_line_at(&path, 99, 10).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
