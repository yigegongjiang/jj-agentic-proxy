//! 往返记录: 一次 req/res 一行 JSON, 落 `~/.config/jj-agentic-proxy/log/YYYY-MM-DD.jsonl`。
//!
//! 记两条腿: 客户端 <-> 代理 (header + body 原样), 代理 <-> 上游 (注入后的 header + 被改写的 body)。
//! 一行一次往返 + 单次 append 写 -> 并发请求不互相穿插, `rg` / `jq` 直接可读。
//! 只按天切文件, 永不删除、不设体积阈值 (本机自用, 完整记录比省磁盘重要; 清理由人类自行决定)。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// 一次往返: 请求进来时建立, 响应体收完 (或连接中断) 时落盘。
pub struct Record {
    started: Instant,
    surface: Surface,
    method: String,
    path: String,
    model: Option<String>,
    stream: bool,
    req_headers: Value,
    req_raw: Bytes,
    req: Value,
    req_bytes: usize,
    status: u16,
    res_headers: Value,
    res: Vec<u8>,
    upstream: Option<Leg>,
    done: bool,
}

/// 代理 <-> 上游那一腿: header 是注入 CLI 身份后的实际值, body 是本层规范化后的实际值。
#[derive(Default)]
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
    req_headers: &'a Value,
    req: &'a Value,
    res_headers: &'a Value,
    res: Value,
    /// 没打到上游 (本层直接应答 / 未登录) 时缺席
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<LegLine<'a>>,
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
    Some(Record {
        started: Instant::now(),
        surface,
        method: method.to_string(),
        path: path.to_string(),
        model: req.get("model").and_then(Value::as_str).map(str::to_string),
        stream: req.get("stream").and_then(Value::as_bool).unwrap_or(false),
        req_headers: headers_json(headers),
        req_raw: body.clone(),
        req_bytes: body.len(),
        req,
        status: 0,
        res_headers: Value::Null,
        res: Vec::new(),
        upstream: None,
        done: false,
    })
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
        self.upstream = leg;
    }

    /// 已在内存的响应直接落盘 (保住 content-length); 流式响应逐块 tee, 不缓冲转发。
    pub async fn capture(mut self, resp: Response) -> Response {
        self.status = resp.status().as_u16();
        self.res_headers = headers_json(resp.headers());
        let (parts, body) = resp.into_parts();
        if body.size_hint().exact().is_some() {
            let bytes = axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap_or_default();
            self.res.extend_from_slice(&bytes);
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
                        rec.res.extend_from_slice(&chunk);
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

    /// 落一行; 幂等 (Drop 兜底时不会重复写)。
    fn flush(&mut self, incomplete: Option<&str>) {
        if self.done {
            return;
        }
        self.done = true;
        let (day, ts) = parts(now_ms());
        let upstream = self.upstream.as_ref().map(|leg| LegLine {
            method: &leg.method,
            url: &leg.url,
            status: leg.status,
            req_headers: &leg.req_headers,
            res_headers: &leg.res_headers,
            // 未改写就不重复存一份 (客户端 req 即上游 req)
            req_body: (leg.req_body != self.req_raw).then(|| payload(&leg.req_body)),
        });
        let line = Line {
            ts: &ts,
            surface: self.surface.key(),
            method: &self.method,
            path: &self.path,
            status: self.status,
            stream: self.stream,
            elapsed_ms: self.started.elapsed().as_millis(),
            req_bytes: self.req_bytes,
            res_bytes: self.res.len(),
            model: self.model.as_deref(),
            incomplete,
            req_headers: &self.req_headers,
            req: &self.req,
            res_headers: &self.res_headers,
            res: payload(&self.res),
            upstream,
        };
        match serde_json::to_string(&line) {
            Ok(mut text) => {
                text.push('\n');
                append(&day, &text);
            }
            Err(e) => tracing::warn!(error = %e, "往返记录序列化失败"),
        }
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        self.flush(Some("客户端断开"));
    }
}

/// JSON body 存成 JSON (可 `jq`), 其余 (SSE / 文本 / 二进制) 存成字符串。
fn payload(raw: &[u8]) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(raw)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(raw).into_owned()))
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
    if let Some(bad) = v["incomplete"].as_str() {
        out.push_str(&format!("  [{bad}]"));
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
        assert!(start(
            Surface::Codex,
            &Method::GET,
            "/v1/models",
            &h,
            &Bytes::new()
        )
        .is_some());
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

    /// 摘要标量在前、header / body 在后, 且只写一次 (Drop 不重复)。
    #[test]
    fn line_shape_is_summary_first_then_payload() {
        let mut rec = record(r#"{"model":"claude-opus-5","stream":true}"#);
        rec.status = 200;
        rec.res = b"data: hi\n\n".to_vec();
        rec.res_headers = headers_json(&client_headers());
        // 上游那一腿: body 被本层改写过 -> 单独记一份
        rec.upstream = Some(Leg {
            method: "POST".into(),
            url: "https://api.anthropic.com/v1/messages".into(),
            req_headers: headers_json(&client_headers()),
            req_body: Bytes::from_static(br#"{"model":"claude-opus-5","system":[]}"#),
            status: 200,
            res_headers: headers_json(&client_headers()),
        });

        let upstream = rec.upstream.as_ref().map(|leg| LegLine {
            method: &leg.method,
            url: &leg.url,
            status: leg.status,
            req_headers: &leg.req_headers,
            res_headers: &leg.res_headers,
            req_body: (leg.req_body != rec.req_raw).then(|| payload(&leg.req_body)),
        });
        let text = serde_json::to_string(&Line {
            ts: "2026-07-31T19:57:09.964+09:00",
            surface: rec.surface.key(),
            method: &rec.method,
            path: &rec.path,
            status: rec.status,
            stream: rec.stream,
            elapsed_ms: 12,
            req_bytes: rec.req_bytes,
            res_bytes: rec.res.len(),
            model: rec.model.as_deref(),
            incomplete: None,
            req_headers: &rec.req_headers,
            req: &rec.req,
            res_headers: &rec.res_headers,
            res: payload(&rec.res),
            upstream,
        })
        .unwrap();

        assert!(
            text.starts_with(r#"{"ts":"2026-07-31T19:57:09.964+09:00","surface":"claude-code""#)
        );
        assert!(text.contains(r#""model":"claude-opus-5""#));
        // 摘要段 = `,"req_headers":` 之前那截, 只有标量
        let (head, _) = text.split_at(text.find(r#","req_headers":"#).unwrap());
        assert!(head.contains("res_bytes"));
        assert!(!head.contains("x-api-key"));
        // 两条腿都在
        assert!(text.contains(r#""x-api-key":"sk-client""#));
        assert!(text.contains(r#""url":"https://api.anthropic.com/v1/messages""#));
        assert!(text.contains(r#""req_body":{"model":"claude-opus-5","system":[]}"#));

        // 记录只落一次: 手动 flush 后 Drop 不再写
        rec.done = true;
        drop(rec);
    }

    /// 上游 body 与客户端逐字节相同 -> 不重复存第二份。
    #[test]
    fn unchanged_upstream_body_is_not_duplicated() {
        let raw = r#"{"model":"m"}"#;
        let rec = record(raw);
        let leg = Leg {
            req_body: Bytes::from(raw.to_string()),
            ..Leg::default()
        };
        assert!(!(leg.req_body != rec.req_raw));
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
