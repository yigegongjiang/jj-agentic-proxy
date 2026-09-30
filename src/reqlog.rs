//! 往返记录: 一次 req/res 一行 JSON, 落 `~/.config/jj-agentic-proxy/log/YYYY-MM-DD.jsonl`。
//!
//! 记两条腿: 客户端 <-> 代理 (header + body 原样), 代理 <-> 上游 (注入后的 header + 被改写的 body)。
//! 一行一次往返 + 单次 append 写 -> 并发请求不互相穿插, `rg` / `jq` 直接可读。
//! 只按天切文件, 永不删除、不设体积阈值 (本机自用, 完整记录比省磁盘重要; 清理由人类自行决定)。
//! 落盘要等响应结束 -> 进行中的往返另在内存登记 (落盘即注销), 查看器经 `/api/inflight` 读。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_stream::stream;
use axum::body::{Body, Bytes, HttpBody as _};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use futures_util::StreamExt as _;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::provider::Surface;
use crate::store::config_dir;

/// 时间戳固定 JST: 本机自用, 不引 tz 数据库。
const TZ_OFFSET_SECS: i64 = 9 * 3600;
const TZ_SUFFIX: &str = "+09:00";
const EXT: &str = ".jsonl";

pub fn log_dir() -> PathBuf {
    config_dir().join("log")
}

// ---------- 记录 ----------

/// 一次往返: 请求进来时建立并登记为进行中, 响应体收完 (或连接中断) 时落盘并注销。
pub struct Record {
    live: Arc<Inflight>,
    done: bool,
}

/// 进行中的往返: 查看器不必等响应结束才看得到。只在内存 -> daemon 退出即清空, 不留残留。
struct Inflight {
    /// 与落盘那行同一个 id -> 查看器据此把进行中那条换成最终记录, 选中态不丢
    id: String,
    /// 请求进来的时刻 (落盘行的 `ts` 是结束时刻)
    ts: String,
    started: Instant,
    surface: Surface,
    method: String,
    path: String,
    model: Option<String>,
    stream: bool,
    req_headers: Value,
    req_raw: Bytes,
    req: Value,
    progress: Mutex<Progress>,
}

/// 响应侧随时间增长的部分; 查看器读进行中详情时拷一份出锁再序列化, 不拖住转发。
#[derive(Clone, Default)]
struct Progress {
    status: u16,
    res_headers: Value,
    res: Vec<u8>,
    upstream: Option<Leg>,
}

static INFLIGHT: Mutex<Vec<Arc<Inflight>>> = Mutex::new(Vec::new());

fn inflight() -> std::sync::MutexGuard<'static, Vec<Arc<Inflight>>> {
    INFLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// 进程启动时刻 (ms, hex) + 进程内序号: 跨重启不撞号 (同一天文件里会有上个进程写的行)。
fn next_id() -> String {
    static BOOT: OnceLock<i64> = OnceLock::new();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let boot = *BOOT.get_or_init(now_ms);
    format!("{boot:x}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 代理 <-> 上游那一腿: header 是注入 CLI 身份后的实际值, body 是本层规范化后的实际值。
#[derive(Clone, Default)]
pub struct Leg {
    method: String,
    url: String,
    req_headers: Value,
    req_body: Bytes,
    status: u16,
    res_headers: Value,
}

#[derive(Serialize)]
struct LegLine<'a> {
    method: &'a str,
    url: &'a str,
    status: u16,
    req_headers: &'a Value,
    res_headers: &'a Value,
    /// 仅当本层改写过请求体时出现 (未改写 = 与客户端 `req` 逐字节相同)
    #[serde(skip_serializing_if = "Option::is_none")]
    req_body: Option<Value>,
}

/// 字段顺序 = 输出顺序 (struct 序列化保序): 摘要标量在前, header / body 在后
/// -> 截到 `,"req_headers":` 就是一条摘要, 不用碰大 body。
#[derive(Serialize)]
struct Line<'a> {
    ts: &'a str,
    surface: &'a str,
    method: &'a str,
    path: &'a str,
    status: u16,
    stream: bool,
    elapsed_ms: u128,
    req_bytes: usize,
    res_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    /// 仅异常时出现: 客户端断开 / 上游流中断。
    #[serde(skip_serializing_if = "Option::is_none")]
    incomplete: Option<&'a str>,
    /// 仅失败时出现: 错误信封里的那句话 (状态码 >= 400 / 流里的 error 事件), 截到 [`ERROR_CAP`] 字符
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// 响应里带 usage 时出现; 三种方言归一 (见 [`Tokens`])
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<Tokens>,
    id: &'a str,
    /// 仅进行中的快照带 (`/api/inflight/{id}`), 落盘行没有
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    inflight: bool,
    req_headers: &'a Value,
    req: &'a Value,
    res_headers: &'a Value,
    res: Value,
    /// 没打到上游 (本层直接应答 / 未登录) 时缺席
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<LegLine<'a>>,
}

impl Inflight {
    fn line(&self, p: &Progress, ts: &str, incomplete: Option<&str>, inflight: bool) -> String {
        let upstream = p.upstream.as_ref().map(|leg| LegLine {
            method: &leg.method,
            url: &leg.url,
            status: leg.status,
            req_headers: &leg.req_headers,
            res_headers: &leg.res_headers,
            // 未改写就不重复存一份 (客户端 req 即上游 req)
            req_body: (leg.req_body != self.req_raw).then(|| payload(&leg.req_body)),
        });
        let res = payload(&p.res);
        let (tokens, error) = digest(p.status, &res);
        let line = Line {
            ts,
            surface: self.surface.key(),
            method: &self.method,
            path: &self.path,
            status: p.status,
            stream: self.stream,
            elapsed_ms: self.started.elapsed().as_millis(),
            req_bytes: self.req_raw.len(),
            res_bytes: p.res.len(),
            model: self.model.as_deref(),
            incomplete,
            error,
            tokens,
            id: &self.id,
            inflight,
            req_headers: &self.req_headers,
            req: &self.req,
            res_headers: &p.res_headers,
            res,
            upstream,
        };
        // 全是 String 键 + 已解析的 Value -> 序列化不会失败
        serde_json::to_string(&line).unwrap_or_default()
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `None` = 不记录。`/health` 是本机状态查询, 无上游往返, 探活会刷屏。
pub fn start(
    surface: Surface,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &Bytes,
) -> Option<Record> {
    if crate::proxy::strip_v1(path.split('?').next().unwrap_or(path)) == "/health" {
        return None;
    }
    let req = payload(body);
    let live = Arc::new(Inflight {
        id: next_id(),
        ts: parts(now_ms()).1,
        started: Instant::now(),
        surface,
        method: method.to_string(),
        path: path.to_string(),
        model: req.get("model").and_then(Value::as_str).map(str::to_string),
        stream: req.get("stream").and_then(Value::as_bool).unwrap_or(false),
        req_headers: headers_json(headers),
        req_raw: body.clone(),
        req,
        progress: Mutex::new(Progress::default()),
    });
    inflight().push(live.clone());
    Some(Record { live, done: false })
}

/// header 原样记录 (含 `authorization`): 本机自用, 凭证本来就在同目录的 auth.json 里,
/// 抹掉反而让「上游为什么拒」无从查起。记录文件同样 0600。
fn headers_json(h: &HeaderMap) -> Value {
    let mut out = Map::new();
    for name in h.keys() {
        let joined = h
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap_or("<non-utf8>").to_string())
            .collect::<Vec<_>>()
            .join(", ");
        out.insert(name.as_str().to_string(), Value::String(joined));
    }
    Value::Object(out)
}

// ---------- 上游那一腿 (由 proxy 层沿途填) ----------

tokio::task_local! {
    static LEG: Mutex<Option<Leg>>;
}

/// 在 task-local 作用域里跑请求处理 -> 处理过程中记下的上游腿随返回值一起带出来。
pub async fn scoped<F>(fut: F) -> (Response, Option<Leg>)
where
    F: std::future::Future<Output = Response>,
{
    LEG.scope(Mutex::new(None), async move {
        let resp = fut.await;
        let leg = LEG.with(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).take());
        (resp, leg)
    })
    .await
}

/// 401 重试会调第二次 -> 后写覆盖先写, 记录的是最终真正生效的那次。
pub(crate) fn note_upstream_request(method: &Method, url: &str, headers: &HeaderMap, body: &Bytes) {
    let leg = Leg {
        method: method.to_string(),
        url: url.to_string(),
        req_headers: headers_json(headers),
        req_body: body.clone(),
        status: 0,
        res_headers: Value::Null,
    };
    let _ = LEG.try_with(|slot| {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(leg);
    });
}

pub(crate) fn note_upstream_response(status: StatusCode, headers: &HeaderMap) {
    let _ = LEG.try_with(|slot| {
        if let Some(leg) = slot.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            leg.status = status.as_u16();
            leg.res_headers = headers_json(headers);
        }
    });
}

impl Record {
    pub fn set_upstream(&mut self, leg: Option<Leg>) {
        self.live.progress().upstream = leg;
    }

    /// 已在内存的响应直接落盘 (保住 content-length); 流式响应逐块 tee, 不缓冲转发。
    pub async fn capture(mut self, resp: Response) -> Response {
        {
            let mut p = self.live.progress();
            p.status = resp.status().as_u16();
            p.res_headers = headers_json(resp.headers());
        }
        let (parts, body) = resp.into_parts();
        if body.size_hint().exact().is_some() {
            let bytes = axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap_or_default();
            self.live.progress().res.extend_from_slice(&bytes);
            self.flush(None);
            return Response::from_parts(parts, Body::from(bytes));
        }

        let mut upstream = Box::pin(body.into_data_stream());
        // self 移进生成器: 客户端中途断开 -> 生成器被丢弃 -> Drop 兜底落盘。
        let teed = stream! {
            let mut rec = self;
            while let Some(item) = upstream.next().await {
                match item {
                    Ok(chunk) => {
                        rec.live.progress().res.extend_from_slice(&chunk);
                        yield Ok(chunk);
                    }
                    Err(e) => {
                        rec.flush(Some(&format!("上游流中断: {e}")));
                        yield Err(e);
                        return;
                    }
                }
            }
            rec.flush(None);
        };
        Response::from_parts(parts, Body::from_stream(teed))
    }

    /// 落一行并注销进行中; 幂等 (Drop 兜底时不会重复写)。
    fn flush(&mut self, incomplete: Option<&str>) {
        if self.done {
            return;
        }
        self.done = true;
        let (day, ts) = parts(now_ms());
        let mut text = {
            let p = self.live.progress();
            self.live.line(&p, &ts, incomplete, false)
        };
        text.push('\n');
        append(&day, &text);
        // 先落盘再注销: 查看器先取进行中再扫文件 -> 交接瞬间最多两边都有 (按 id 去重), 不会两边都没有
        inflight().retain(|r| !Arc::ptr_eq(r, &self.live));
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        self.flush(Some("客户端断开"));
    }
}

// ---------- 进行中 (查看器读) ----------

/// 进行中列表的一条: 与落盘行的摘要段同名同义, `ts` = 请求进来的时刻。
#[derive(Serialize)]
pub struct InflightSummary {
    id: String,
    ts: String,
    surface: &'static str,
    method: String,
    path: String,
    /// 0 = 上游还没回响应头
    status: u16,
    stream: bool,
    elapsed_ms: u128,
    req_bytes: usize,
    res_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

/// 旧 -> 新 (登记顺序 = 请求进来的顺序)。
pub fn inflight_list() -> Vec<InflightSummary> {
    let live: Vec<Arc<Inflight>> = inflight().clone();
    live.iter()
        .map(|r| {
            let (status, res_bytes) = {
                let p = r.progress();
                (p.status, p.res.len())
            };
            InflightSummary {
                id: r.id.clone(),
                ts: r.ts.clone(),
                surface: r.surface.key(),
                method: r.method.clone(),
                path: r.path.clone(),
                status,
                stream: r.stream,
                elapsed_ms: r.started.elapsed().as_millis(),
                req_bytes: r.req_raw.len(),
                res_bytes,
                model: r.model.clone(),
            }
        })
        .collect()
}

/// 进行中那条的整行快照 (与落盘行同形状 + `inflight: true`); 已结束 = `None` (去文件里找)。
pub fn inflight_line(id: &str) -> Option<String> {
    let live = inflight().iter().find(|r| r.id == id).cloned()?;
    // 拷出锁再序列化: 大 body 的序列化不占着转发路径要的锁
    let snapshot = live.progress().clone();
    Some(live.line(&snapshot, &live.ts, None, true))
}

/// JSON body 存成 JSON (可 `jq`), 其余 (SSE / 文本 / 二进制) 存成字符串。
fn payload(raw: &[u8]) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(raw)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(raw).into_owned()))
}

// ---------- 摘要: token 用量 / 错误 (查看器列表只读摘要段, 不碰 body) ----------

const ERROR_CAP: usize = 200;

/// `input` 一律含缓存命中: OpenAI 两种方言本来就含, Anthropic 的 `input_tokens` 不含 -> 加回
/// `cache_read` + `cache_creation`, 否则 Claude Code 的请求只显示个位数输入。
#[derive(Serialize, Default, Clone, Copy, PartialEq, Debug)]
struct Tokens {
    input: u64,
    output: u64,
    /// 输入里命中缓存的部分
    #[serde(skip_serializing_if = "is_zero")]
    cached: u64,
    /// 本次写入缓存的部分 (Anthropic cache_creation / Responses cache_write)
    #[serde(skip_serializing_if = "is_zero")]
    cache_write: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Tokens {
    /// 一个 usage 对象 -> 归一; 只读顶层已知字段 (Responses 的 usage 里还嵌着逐条 attribution)。
    fn of(u: &Value) -> Option<Self> {
        let n = |ptr: &str| u.pointer(ptr).and_then(Value::as_u64).unwrap_or(0);
        if !u.is_object() {
            return None;
        }
        let t = if u.get("prompt_tokens").is_some() {
            Tokens {
                input: n("/prompt_tokens"),
                output: n("/completion_tokens"),
                cached: n("/prompt_tokens_details/cached_tokens"),
                cache_write: 0,
            }
        } else if u.get("cache_read_input_tokens").is_some()
            || u.get("cache_creation_input_tokens").is_some()
        {
            let (read, write) = (
                n("/cache_read_input_tokens"),
                n("/cache_creation_input_tokens"),
            );
            Tokens {
                input: n("/input_tokens") + read + write,
                output: n("/output_tokens"),
                cached: read,
                cache_write: write,
            }
        } else {
            Tokens {
                input: n("/input_tokens"),
                output: n("/output_tokens"),
                cached: n("/input_tokens_details/cached_tokens"),
                cache_write: n("/input_tokens_details/cache_write_tokens"),
            }
        };
        Some(t)
    }

    /// Anthropic 把用量拆在 message_start / message_delta 两帧, 且计数只增不减 -> 逐字段取大。
    fn merge(&mut self, o: Self) {
        self.input = self.input.max(o.input);
        self.output = self.output.max(o.output);
        self.cached = self.cached.max(o.cached);
        self.cache_write = self.cache_write.max(o.cache_write);
    }
}

/// 响应体 -> (token 用量, 错误那句话)。解析不了的帧直接跳过: 这里在流收尾的路径上, 绝不 panic。
fn digest(status: u16, res: &Value) -> (Option<Tokens>, Option<String>) {
    let mut tokens: Option<Tokens> = None;
    let mut error: Option<String> = None;
    let mut absorb = |v: &Value, error: &mut Option<String>| {
        for u in [
            v.get("usage"),
            v.pointer("/message/usage"),
            v.pointer("/response/usage"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(t) = Tokens::of(u) {
                tokens.get_or_insert_with(Tokens::default).merge(t);
            }
        }
        if error.is_none() {
            *error = error_text(v);
        }
    };
    match res {
        Value::Object(_) => {
            absorb(res, &mut error);
            if status >= 400 && error.is_none() {
                error = res
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some(res.to_string()));
            }
        }
        Value::String(text) => {
            for line in text.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                // 只解析可能有料的帧: 一次流几百帧, 大多是正文增量
                if !data.contains("usage") && !data.contains("error") {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    absorb(&v, &mut error);
                }
            }
            if status >= 400 && error.is_none() && !text.trim().is_empty() {
                error = Some(text.trim().to_string());
            }
        }
        _ => {}
    }
    let error = error.map(|e| {
        let e = e.trim();
        match e.char_indices().nth(ERROR_CAP) {
            Some((at, _)) => format!("{}…", &e[..at]),
            None => e.to_string(),
        }
    });
    (tokens.filter(|t| *t != Tokens::default()), error)
}

/// 错误信封的那句话: 两家方言 / Responses 的 `response.failed` / 本代理自产, `null` 不算。
fn error_text(v: &Value) -> Option<String> {
    let err = [v.get("error"), v.pointer("/response/error")]
        .into_iter()
        .flatten()
        .find(|e| !e.is_null())?;
    Some(match err {
        Value::String(s) => s.clone(),
        e => e
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| e.to_string(), str::to_string),
    })
}

// ---------- 写入 ----------

struct Writer {
    day: String,
    file: File,
}

static WRITER: Mutex<Option<Writer>> = Mutex::new(None);

/// 按天切文件: 换天即 reopen (旧文件原样留着)。写失败只警告, 绝不影响代理本身。
fn append(day: &str, line: &str) {
    let mut guard = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    if !matches!(guard.as_ref(), Some(w) if w.day == day) {
        match open(day) {
            Ok(w) => *guard = Some(w),
            Err(e) => {
                tracing::warn!(error = %e, "打开往返记录失败");
                return;
            }
        }
    }
    let Some(w) = guard.as_mut() else { return };
    // 单次 write: O_APPEND 下不与其他写入穿插 -> 无需额外分隔约定。
    if let Err(e) = w.file.write_all(line.as_bytes()) {
        tracing::warn!(error = %e, "写入往返记录失败");
        *guard = None;
    }
}

fn open(day: &str) -> io::Result<Writer> {
    let dir = log_dir();
    fs::create_dir_all(&dir)?;
    // 0600: 记录里含原样 header (授权头在内), 与 auth.json 同权限
    let path = dir.join(format!("{day}{EXT}"));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    // mode 只在创建时生效 -> 已存在的旧文件 (含升级前留下的) 每次开都收紧一次
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Writer {
        day: day.to_string(),
        file,
    })
}

// ---------- 读取 (logs 子命令) ----------

/// 打印最近 `n` 条摘要 (旧 -> 新)。完整 body 在文件里, 摘要只给定位用的字段。
pub fn print_tail(n: usize) -> Result<()> {
    let dir = log_dir();
    let mut days: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(EXT))
        .collect();
    days.sort();

    let mut lines: Vec<String> = Vec::new();
    // 新文件优先, 够数即停 -> 与总体积无关。
    for path in days.iter().rev() {
        if lines.len() >= n {
            break;
        }
        match tail_lines(path, n - lines.len()) {
            Ok(mut got) => {
                got.extend(lines);
                lines = got;
            }
            Err(e) => eprintln!("读取 {} 失败: {e}", path.display()),
        }
    }

    if lines.is_empty() {
        println!("暂无往返记录");
    }
    for raw in &lines {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => println!("{}", summary(&v)),
            Err(_) => println!("{raw}"),
        }
    }
    println!("目录 {} (按天分文件, 不自动清理)", dir.display());
    Ok(())
}

fn summary(v: &Value) -> String {
    let s = |k: &str| v[k].as_str().unwrap_or("-").to_string();
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    // status 0 = 没等到响应 (客户端提前断开) -> 显式区分于真实状态码。
    let status = match n("status") {
        0 => "-".to_string(),
        code => code.to_string(),
    };
    let mut out = format!(
        "{} {:<13} {:<4} {:<34} {:>3} {:>8} req {:>9} res {:>9}",
        s("ts").replace('T', " ").replace(TZ_SUFFIX, ""),
        s("surface"),
        s("method"),
        s("path"),
        status,
        span(n("elapsed_ms")),
        size(n("req_bytes")),
        size(n("res_bytes")),
    );
    if let Some(m) = v["model"].as_str() {
        out.push_str(&format!("  {m}"));
    }
    if v["stream"].as_bool() == Some(true) {
        out.push_str(" stream");
    }
    if let Some(t) = v.get("tokens") {
        let n = |k: &str| t[k].as_u64().unwrap_or(0);
        out.push_str(&format!("  tok {}→{}", n("input"), n("output")));
    }
    if let Some(bad) = v["incomplete"].as_str() {
        out.push_str(&format!("  [{bad}]"));
    }
    if let Some(err) = v["error"].as_str() {
        out.push_str(&format!("  ! {err}"));
    }
    out
}

fn size(bytes: u64) -> String {
    match bytes {
        0 => "-".to_string(),
        b if b < 1024 => format!("{b}B"),
        b if b < 1024 * 1024 => format!("{:.1}KB", b as f64 / 1024.0),
        b => format!("{:.1}MB", b as f64 / (1024.0 * 1024.0)),
    }
}

fn span(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// 从文件尾部按块回读, 只取最后 `want` 行 -> 单日文件再大也不整读。
fn tail_lines(path: &Path, want: usize) -> io::Result<Vec<String>> {
    const CHUNK: u64 = 256 * 1024;
    let mut f = File::open(path)?;
    let mut pos = f.metadata()?.len();
    // 块首那段行可能被切断 -> 留给下一轮 (更早的块) 拼回去。
    let mut head: Vec<u8> = Vec::new();
    let mut out: Vec<String> = Vec::new();

    while pos > 0 && out.len() < want {
        let step = CHUNK.min(pos);
        pos -= step;
        f.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0u8; step as usize];
        f.read_exact(&mut chunk)?;
        chunk.append(&mut head);

        let mut segs: Vec<&[u8]> = chunk.split(|b| *b == b'\n').collect();
        head = segs.remove(0).to_vec();
        for seg in segs.into_iter().rev() {
            if seg.is_empty() {
                continue;
            }
            out.push(String::from_utf8_lossy(seg).into_owned());
            if out.len() >= want {
                break;
            }
        }
    }
    if pos == 0 && out.len() < want && !head.is_empty() {
        out.push(String::from_utf8_lossy(&head).into_owned());
    }
    out.reverse();
    Ok(out)
}

// ---------- 时间 (固定 JST, 无依赖) ----------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `(YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS.mmm+09:00)`
fn parts(epoch_ms: i64) -> (String, String) {
    let local = epoch_ms + TZ_OFFSET_SECS * 1000;
    let (days, rem) = (local.div_euclid(86_400_000), local.rem_euclid(86_400_000));
    let (y, m, d) = civil_from_days(days);
    let day = format!("{y:04}-{m:02}-{d:02}");
    let ts = format!(
        "{day}T{:02}:{:02}:{:02}.{:03}{TZ_SUFFIX}",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    );
    (day, ts)
}

/// Howard Hinnant 的 days -> civil 算法 (proleptic Gregorian, 1970-01-01 = 0)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (yoe as i64 + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    /// 测试里造的记录不落盘: 否则 Drop 兜底会往真实日志目录写一行。
    fn discard(mut rec: Record) {
        rec.done = true;
        inflight().retain(|r| !Arc::ptr_eq(r, &rec.live));
    }

    #[test]
    fn timestamp_is_jst_and_day_matches_file_name() {
        assert_eq!(
            parts(0),
            (
                "1970-01-01".to_string(),
                "1970-01-01T09:00:00.000+09:00".to_string()
            )
        );
        // JST 当日 00:00 = UTC 前一日 15:00 -> 文件按 JST 日期切
        let (day, ts) = parts(1_785_461_829_964);
        assert_eq!(day, "2026-07-31");
        assert_eq!(ts, "2026-07-31T10:37:09.964+09:00");
        assert!(ts.starts_with(&day));
    }

    /// 闰年 / 世纪年 / 负数天 (1970 之前) 都要对。
    #[test]
    fn civil_from_days_covers_edges() {
        for (days, ymd) in [
            (0, (1970, 1, 1)),
            (11_016, (2000, 2, 29)),
            (20_665, (2026, 7, 31)),
            (47_541, (2100, 3, 1)),
            (-1, (1969, 12, 31)),
        ] {
            assert_eq!(civil_from_days(days), ymd);
        }
    }

    #[test]
    fn json_body_stays_json_and_sse_becomes_text() {
        assert_eq!(payload(b""), Value::Null);
        assert_eq!(payload(br#"{"a":1}"#)["a"], serde_json::json!(1));
        let sse = b"event: ping\ndata: {\"x\":1}\n\n";
        assert!(payload(sse).is_string());
        assert!(payload(sse).as_str().unwrap().contains("event: ping"));
    }

    fn client_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", "sk-client".parse().unwrap());
        h.insert("content-type", "application/json".parse().unwrap());
        h
    }

    fn record(body: &str) -> Record {
        start(
            Surface::ClaudeCode,
            &Method::POST,
            "/v1/messages",
            &client_headers(),
            &Bytes::from(body.to_string()),
        )
        .expect("该路径应记录")
    }

    #[test]
    fn health_is_not_recorded() {
        let h = HeaderMap::new();
        for path in ["/health", "/v1/health", "/health?x=1"] {
            assert!(start(Surface::Codex, &Method::GET, path, &h, &Bytes::new()).is_none());
        }
        let models = start(
            Surface::Codex,
            &Method::GET,
            "/v1/models",
            &h,
            &Bytes::new(),
        );
        discard(models.expect("非 /health 应记录"));
    }

    #[test]
    fn headers_keep_every_value_verbatim() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer secret".parse().unwrap());
        h.append("anthropic-beta", "oauth-2025-04-20".parse().unwrap());
        h.append("anthropic-beta", "extra".parse().unwrap());
        let v = headers_json(&h);
        // 原样保留 (含授权头): 抹掉就查不了「上游为什么拒」
        assert_eq!(v["authorization"], serde_json::json!("Bearer secret"));
        // 同名多值合成一条, 不丢
        assert_eq!(
            v["anthropic-beta"],
            serde_json::json!("oauth-2025-04-20, extra")
        );
    }

    /// 摘要标量在前、header / body 在后。
    #[test]
    fn line_shape_is_summary_first_then_payload() {
        let mut rec = record(r#"{"model":"claude-opus-5","stream":true}"#);
        // 上游那一腿: body 被本层改写过 -> 单独记一份
        rec.set_upstream(Some(Leg {
            method: "POST".into(),
            url: "https://api.anthropic.com/v1/messages".into(),
            req_headers: headers_json(&client_headers()),
            req_body: Bytes::from_static(br#"{"model":"claude-opus-5","system":[]}"#),
            status: 200,
            res_headers: headers_json(&client_headers()),
        }));
        let text = {
            let mut p = rec.live.progress();
            p.status = 200;
            p.res = b"data: hi\n\n".to_vec();
            p.res_headers = headers_json(&client_headers());
            rec.live
                .line(&p, "2026-07-31T19:57:09.964+09:00", None, false)
        };

        assert!(
            text.starts_with(r#"{"ts":"2026-07-31T19:57:09.964+09:00","surface":"claude-code""#)
        );
        assert!(text.contains(r#""model":"claude-opus-5""#));
        // 摘要段 = `,"req_headers":` 之前那截, 只有标量; id 在摘要段内 (查看器只解析这截)
        let (head, _) = text.split_at(text.find(r#","req_headers":"#).unwrap());
        assert!(head.contains("res_bytes"));
        assert!(head.contains(&format!(r#""id":"{}""#, rec.live.id)));
        assert!(!head.contains("inflight"));
        assert!(!head.contains("x-api-key"));
        // 两条腿都在
        assert!(text.contains(r#""x-api-key":"sk-client""#));
        assert!(text.contains(r#""url":"https://api.anthropic.com/v1/messages""#));
        assert!(text.contains(r#""req_body":{"model":"claude-opus-5","system":[]}"#));

        discard(rec);
    }

    /// 上游 body 与客户端逐字节相同 -> 不重复存第二份。
    #[test]
    fn unchanged_upstream_body_is_not_duplicated() {
        let raw = r#"{"model":"m"}"#;
        let mut rec = record(raw);
        rec.set_upstream(Some(Leg {
            req_body: Bytes::from(raw.to_string()),
            ..Leg::default()
        }));
        let text = rec.live.line(&rec.live.progress(), "t", None, false);
        assert!(text.contains(r#""upstream":{"#));
        assert!(!text.contains("req_body"));
        discard(rec);
    }

    /// 进行中: 请求一进来就可见, 响应逐块长; 无论正常结束还是中途被丢弃, 都注销且落盘一行。
    #[test]
    fn inflight_is_visible_until_flushed_and_never_leaks() {
        let _dir_guard = store::TEST_DIR_LOCK.blocking_lock();
        let dir = std::env::temp_dir().join(format!("jj-reqlog-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        *store::TEST_CONFIG_DIR.lock().unwrap() = Some(dir.clone());

        let rec = record(r#"{"model":"m","stream":true}"#);
        let id = rec.live.id.clone();
        assert!(inflight_list().iter().any(|s| s.id == id));
        rec.live
            .progress()
            .res
            .extend_from_slice(b"data: partial\n\n");
        let snap = inflight_line(&id).expect("进行中应可取快照");
        assert!(snap.contains(r#""inflight":true"#));
        assert!(snap.contains("data: partial"));

        // 未 flush 直接丢弃 (= 请求 future 被取消) -> Drop 兜底落盘 + 注销
        drop(rec);
        assert!(inflight_list().iter().all(|s| s.id != id));
        assert!(inflight_line(&id).is_none());
        let (day, _) = parts(now_ms());
        let saved = fs::read_to_string(log_dir().join(format!("{day}{EXT}"))).unwrap();
        let last = saved.lines().last().unwrap();
        assert!(last.contains(&format!(r#""id":"{id}""#)));
        assert!(last.contains(r#""incomplete":"客户端断开""#));
        assert!(!last.contains("inflight"));

        *WRITER.lock().unwrap() = None;
        *store::TEST_CONFIG_DIR.lock().unwrap() = None;
        let _ = fs::remove_dir_all(&dir);
    }

    /// 三种方言的 usage 形状取自真实日志; input 一律含缓存。
    #[test]
    fn digest_normalizes_usage_across_dialects() {
        let t = |status, res: Value| digest(status, &res).0.expect("应有用量");
        // Anthropic SSE: 用量拆两帧, input 要加回缓存
        let anthropic = concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3,"cache_creation_input_tokens":100,"cache_read_input_tokens":2000,"output_tokens":1}}}"#,
            "\n\nevent: content_block_delta\n",
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}"#,
            "\n\nevent: message_delta\n",
            r#"data: {"type":"message_delta","usage":{"output_tokens":603}}"#,
            "\n\n"
        );
        assert_eq!(
            t(200, Value::String(anthropic.into())),
            Tokens {
                input: 2103,
                output: 603,
                cached: 2000,
                cache_write: 100
            }
        );
        // Chat Completions 末帧
        let chat = r#"data: {"choices":[],"usage":{"completion_tokens":74,"prompt_tokens":236,"prompt_tokens_details":{"cached_tokens":128},"total_tokens":310}}"#;
        assert_eq!(
            t(200, Value::String(format!("{chat}\n\ndata: [DONE]\n\n"))),
            Tokens {
                input: 236,
                output: 74,
                cached: 128,
                cache_write: 0
            }
        );
        // Responses: usage 在 response.completed 里, 嵌套的 attribution 不参与
        let responses = r#"data: {"type":"response.completed","response":{"error":null,"usage":{"attribution":{"items":{"m":{"input_tokens":999}}},"input_tokens":8,"input_tokens_details":{"cached_tokens":0},"output_tokens":5}}}"#;
        let (tok, err) = digest(200, &Value::String(responses.into()));
        assert_eq!(
            tok,
            Some(Tokens {
                input: 8,
                output: 5,
                cached: 0,
                cache_write: 0
            })
        );
        assert_eq!(err, None, "error: null 不算错误");
        // 非流式 JSON
        let json = serde_json::json!({"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":0}});
        assert_eq!(
            t(200, json),
            Tokens {
                input: 10,
                output: 2,
                cached: 0,
                cache_write: 0
            }
        );
        // 没有 usage (客户端没要 include_usage / models 列表) -> 不出字段
        assert_eq!(
            digest(200, &Value::String("data: {\"choices\":[]}\n\n".into())).0,
            None
        );
        assert_eq!(digest(200, &serde_json::json!({"data":[]})).0, None);
    }

    #[test]
    fn digest_extracts_the_error_sentence() {
        let e = |status, res: Value| digest(status, &res).1;
        assert_eq!(
            e(
                429,
                serde_json::json!({"error":{"message":"rate limited","type":"x"}})
            )
            .as_deref(),
            Some("rate limited")
        );
        assert_eq!(
            e(400, serde_json::json!({"detail":"bad model"})).as_deref(),
            Some("bad model")
        );
        assert_eq!(
            e(502, Value::String("upstream down".into())).as_deref(),
            Some("upstream down")
        );
        // 流中途的 error 事件 (200 开头)
        let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert_eq!(
            e(200, Value::String(sse.into())).as_deref(),
            Some("Overloaded")
        );
        // 成功响应不带
        assert_eq!(e(200, serde_json::json!({"id":"x"})), None);
        assert_eq!(e(0, Value::Null), None);
        // 超长截断, 按字符不按字节 (中文不被劈半)
        let long = "错".repeat(ERROR_CAP + 50);
        let got = e(500, serde_json::json!({"error":{"message": long}})).unwrap();
        assert_eq!(got.chars().count(), ERROR_CAP + 1);
        assert!(got.ends_with('…'));
    }

    #[test]
    fn tail_reads_last_lines_across_chunk_boundary() {
        let dir = std::env::temp_dir().join(format!("jj-proxy-reqlog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-07-31.jsonl");

        // 单行超过一个读块 -> 必须跨块拼回
        let big = "x".repeat(300 * 1024);
        let mut f = File::create(&path).unwrap();
        for i in 0..5 {
            writeln!(f, "line{i}-{big}").unwrap();
        }
        f.flush().unwrap();

        let got = tail_lines(&path, 2).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].starts_with("line3-"));
        assert!(got[1].starts_with("line4-"));
        assert_eq!(got[1].len(), "line4-".len() + big.len());

        // 要的比现有多 -> 全给, 顺序仍是旧 -> 新
        let all = tail_lines(&path, 99).unwrap();
        assert_eq!(all.len(), 5);
        assert!(all[0].starts_with("line0-"));
        let _ = fs::remove_dir_all(&dir);
    }
}
