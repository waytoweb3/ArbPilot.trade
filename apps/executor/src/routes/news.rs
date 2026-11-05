use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;

use crate::{news::NewsResponse, state::AppState};

#[derive(Debug, Deserialize)]
pub struct NewsQuery {
    /// 返回的最大条目数，默认 100，最多 400。
    limit: Option<usize>,
}

/// 上币公告：按发布时间倒序返回缓存条目，并附带各来源的采集状态。
pub async fn list(State(state): State<AppState>, Query(query): Query<NewsQuery>) -> Json<NewsResponse> {
    let limit = query.limit.unwrap_or(100).clamp(1, 400);
    Json(state.news.snapshot(limit))
}
