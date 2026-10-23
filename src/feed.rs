//! 进行中往返的只读接口: macOS app 经此读还没落盘的记录 (落盘要等响应结束)。
//!
//! 只吐 `reqlog` 内存表, 不读文件、不做服务操作; 已落盘的记录 app 直接读 `.jsonl`。

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::reqlog;

pub fn router() -> Router {
    Router::new()
        .route("/api/inflight", get(list))
        .route("/api/inflight/{id}", get(detail))
}

/// 摘要列表: app 每拍先取这里再扫文件, 按 `id` 去重 -> 交接瞬间不丢不重。
async fn list() -> Response {
    Json(json!({ "records": reqlog::inflight_list() })).into_response()
}

/// 与日志行同形状的整行快照 (响应体是截至此刻的部分); 已结束 -> 404, app 改去文件里取。
async fn detail(Path(id): Path<String>) -> Response {
    match reqlog::inflight_line(&id) {
        Some(raw) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            raw,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "这条已结束" })),
        )
            .into_response(),
    }
}
