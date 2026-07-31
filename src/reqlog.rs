//! 往返记录: 一次 req/res 一行 JSON, 落 `~/.config/jj-agentic-proxy/log/YYYY-MM-DD.jsonl`。
//!
//! 一行一次往返 + 单次 append 写 -> 并发请求不互相穿插, `rg` / `jq` 直接可读。
//! 只按天保留最近 7 天, 不设体积阈值 (本机自用, 完整 body 比省磁盘重要)。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_stream::stream;
use axum::body::{Body, Bytes, HttpBody as _};
use axum::http::Method;
use axum::response::Response;
use futures_util::StreamExt as _;
use serde::Serialize;
use serde_json::Value;

use crate::provider::Surface;
use crate::store::config_dir;

/// 保留天数 (含当天)。
pub const KEEP_DAYS: i64 = 7;
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
    req: Value,
    req_bytes: usize,
    status: u16,
    res: Vec<u8>,
    done: bool,
}

/// 字段顺序 = 输出顺序 (struct 序列化保序): 摘要在前, 大 body 在后 -> `cut` 一刀就能看摘要。
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
    req: &'a Value,
    res: Value,
}

/// `None` = 不记录。`/health` 是本机状态查询, 无上游往返, 探活会刷屏。
pub fn start(surface: Surface, method: &Method, path: &str, body: &Bytes) -> Option<Record> {
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
        req_bytes: body.len(),
        req,
        status: 0,
        res: Vec::new(),
        done: false,
    })
}

impl Record {
    /// 已在内存的响应直接落盘 (保住 content-length); 流式响应逐块 tee, 不缓冲转发。
    pub async fn capture(mut self, resp: Response) -> Response {
        self.status = resp.status().as_u16();
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
            req: &self.req,
            res: payload(&self.res),
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

/// 按天切文件: 换天即 reopen + 清理过期。写失败只警告, 绝不影响代理本身。
fn append(day: &str, line: &str) {
    let mut guard = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    if !matches!(guard.as_ref(), Some(w) if w.day == day) {
        match open(day) {
            Ok(w) => {
                *guard = Some(w);
                purge(day);
            }
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
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{day}{EXT}")))?;
    Ok(Writer {
        day: day.to_string(),
        file,
    })
}

/// 启动时清一次: 长期空跑也不会留下上个月的文件。
pub fn sweep() {
    purge(&parts(now_ms()).0);
}

/// 文件名即日期 -> ISO 日期字典序 = 时间序, 直接字符串比较。
fn purge(today: &str) {
    let Some(cutoff) = shift_days(today, -(KEEP_DAYS - 1)) else {
        return;
    };
    let Ok(entries) = fs::read_dir(log_dir()) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(day) = name.strip_suffix(EXT) else {
            continue;
        };
        if day.len() == "0000-00-00".len() && day < cutoff.as_str() {
            let _ = fs::remove_file(e.path());
        }
    }
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
    println!("目录 {} (保留最近 {KEEP_DAYS} 天)", dir.display());
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

/// `YYYY-MM-DD` 加减天数; 形状不对返回 `None`。
fn shift_days(day: &str, delta: i64) -> Option<String> {
    let mut it = day.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let (y, m, d) = civil_from_days(days_from_civil(y, m, d) + delta);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Howard Hinnant 的 civil <-> days 算法 (proleptic Gregorian, 1970-01-01 = 0)。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = i64::from(m) + if m > 2 { -3 } else { 9 }; // [0, 11]
    let doy = ((153 * mp + 2) / 5) as u64 + u64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

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

    #[test]
    fn civil_days_round_trip() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2026, 7, 31),
            (2100, 3, 1),
            (1969, 12, 31),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn retention_cutoff_keeps_seven_days_and_crosses_month() {
        // 保留含当天共 7 天 -> 截止日 = 今天 - 6
        assert_eq!(
            shift_days("2026-07-31", -(KEEP_DAYS - 1)).unwrap(),
            "2026-07-25"
        );
        assert_eq!(shift_days("2026-03-03", -6).unwrap(), "2026-02-25");
        assert_eq!(shift_days("2026-01-03", -6).unwrap(), "2025-12-28");
        assert!(shift_days("2026-01", -6).is_none());
        assert!(shift_days("not-a-day", -6).is_none());
        // 字典序 = 时间序 (purge 的前提)
        assert!("2026-07-24" < "2026-07-25");
        assert!("2025-12-31" < "2026-01-01");
    }

    #[test]
    fn json_body_stays_json_and_sse_becomes_text() {
        assert_eq!(payload(b""), Value::Null);
        assert_eq!(payload(br#"{"a":1}"#)["a"], serde_json::json!(1));
        let sse = b"event: ping\ndata: {\"x\":1}\n\n";
        assert!(payload(sse).is_string());
        assert!(payload(sse).as_str().unwrap().contains("event: ping"));
    }

    fn record(body: &str) -> Record {
        start(
            Surface::ClaudeCode,
            &Method::POST,
            "/v1/messages",
            &Bytes::from(body.to_string()),
        )
        .expect("该路径应记录")
    }

    #[test]
    fn health_is_not_recorded() {
        for path in ["/health", "/v1/health", "/health?x=1"] {
            assert!(start(Surface::Codex, &Method::GET, path, &Bytes::new()).is_none());
        }
        assert!(start(Surface::Codex, &Method::GET, "/v1/models", &Bytes::new()).is_some());
    }

    /// 摘要字段在前、body 在后, 且只写一次 (Drop 不重复)。
    #[test]
    fn line_shape_is_summary_first_then_bodies() {
        let mut rec = record(r#"{"model":"claude-opus-5","stream":true}"#);
        rec.status = 200;
        rec.res = b"data: hi\n\n".to_vec();

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
            req: &rec.req,
            res: payload(&rec.res),
        })
        .unwrap();
        assert!(
            text.starts_with(r#"{"ts":"2026-07-31T19:57:09.964+09:00","surface":"claude-code""#)
        );
        assert!(text.contains(r#""model":"claude-opus-5""#));
        assert!(text.contains(r#""stream":true"#));
        // 大 body 在末尾
        let (head, _) = text.split_at(text.find(r#""req":"#).unwrap());
        assert!(head.contains("res_bytes"));

        // 记录只落一次: 手动 flush 后 Drop 不再写
        rec.done = true;
        drop(rec);
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
