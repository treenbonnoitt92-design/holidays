//! REST API：按日期查询节假日信息。
//!
//! | 方法 | 路径 | 说明 |
//! |------|------|------|
//! | GET  | `/health` | 健康检查，附带记录数 |
//! | GET  | `/api/v1/holiday?date=YYYY-MM-DD` | 查询单个日期 |
//! | GET  | `/api/v1/holiday/{date}` | 查询单个日期（路径写法） |
//! | GET  | `/api/v1/holidays?year=YYYY` | 列出某年全部记录 |
//! | GET  | `/api/v1/year/{year}` | 列出某年全部记录（路径写法） |
//! | GET  | `/api/v1/years` | 列出数据库里有数据的年份 |
//! | POST | `/api/v1/holidays/batch` | 批量查询，body `{"dates":["..."]}` |

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::CorsLayer;

use crate::db::Store;
use crate::model::{Category, DayQuery, Record, normalize_date};

/// 一次批量查询允许的最大日期数。
const BATCH_LIMIT: usize = 1000;

/// 组装路由。
pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/holiday", get(day_by_query))
        .route("/api/v1/holiday/{date}", get(day_by_path))
        .route("/api/v1/holidays", get(year_by_query))
        .route("/api/v1/holidays/batch", post(batch))
        .route("/api/v1/year/{year}", get(year_by_path))
        .route("/api/v1/years", get(years))
        .fallback(not_found)
        // 纯查询服务，直接放开跨域，方便前端/脚本调用
        .layer(CorsLayer::permissive())
        .with_state(store)
}

// ---------------------------------------------------------------- 错误处理

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

/// 统一错误响应：`{"error":{"code":"...","message":"..."}}`
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }

    fn internal(err: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------- 处理器

async fn health(State(store): State<Arc<Store>>) -> Result<Json<serde_json::Value>, ApiError> {
    let stats = store.stats().map_err(ApiError::internal)?;
    Ok(Json(json!({
        "status": "ok",
        "records": stats.total,
        "dates": stats.distinct_dates,
        "years": stats.years,
    })))
}

#[derive(Debug, Deserialize)]
struct DateParams {
    date: Option<String>,
}

/// `GET /api/v1/holiday?date=`
async fn day_by_query(
    State(store): State<Arc<Store>>,
    Query(params): Query<DateParams>,
) -> Result<Json<DayQuery>, ApiError> {
    let raw = params.date.ok_or_else(|| {
        ApiError::bad_request("missing_date", "缺少查询参数 date，例如 ?date=2026-10-01")
    })?;
    lookup(&store, &raw)
}

/// `GET /api/v1/holiday/{date}`
async fn day_by_path(
    State(store): State<Arc<Store>>,
    Path(date): Path<String>,
) -> Result<Json<DayQuery>, ApiError> {
    lookup(&store, &date)
}

fn lookup(store: &Store, raw: &str) -> Result<Json<DayQuery>, ApiError> {
    let (_, day) =
        normalize_date(raw).map_err(|e| ApiError::bad_request("invalid_date", e.to_string()))?;
    let result = store.query_day(day).map_err(ApiError::internal)?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct YearParams {
    year: Option<i32>,
}

#[derive(Debug, Serialize)]
struct YearResponse {
    year: i32,
    /// 该年是否有任何数据
    data_available: bool,
    total: usize,
    holidays: Vec<Record>,
    makeup_workdays: Vec<Record>,
    in_lieu_days: Vec<Record>,
}

/// `GET /api/v1/holidays?year=`
async fn year_by_query(
    State(store): State<Arc<Store>>,
    Query(params): Query<YearParams>,
) -> Result<Json<YearResponse>, ApiError> {
    let year = params.year.ok_or_else(|| {
        ApiError::bad_request("missing_year", "缺少查询参数 year，例如 ?year=2026")
    })?;
    list_year(&store, year)
}

/// `GET /api/v1/year/{year}`
async fn year_by_path(
    State(store): State<Arc<Store>>,
    Path(year): Path<i32>,
) -> Result<Json<YearResponse>, ApiError> {
    list_year(&store, year)
}

fn list_year(store: &Store, year: i32) -> Result<Json<YearResponse>, ApiError> {
    if !(1900..=2999).contains(&year) {
        return Err(ApiError::bad_request(
            "invalid_year",
            format!("年份超出合理范围（1900-2999）：{year}"),
        ));
    }

    let records = store.year_records(year).map_err(ApiError::internal)?;
    let mut holidays = Vec::new();
    let mut makeup_workdays = Vec::new();
    let mut in_lieu_days = Vec::new();
    for record in records {
        match record.category {
            Category::Holiday => holidays.push(record),
            Category::Workday => makeup_workdays.push(record),
            Category::InLieu => in_lieu_days.push(record),
        }
    }

    let total = holidays.len() + makeup_workdays.len() + in_lieu_days.len();
    Ok(Json(YearResponse {
        year,
        data_available: total > 0,
        total,
        holidays,
        makeup_workdays,
        in_lieu_days,
    }))
}

async fn years(State(store): State<Arc<Store>>) -> Result<Json<serde_json::Value>, ApiError> {
    let years = store.available_years().map_err(ApiError::internal)?;
    Ok(Json(json!({
        "count": years.len(),
        "years": years,
    })))
}

#[derive(Debug, Deserialize)]
struct BatchRequest {
    dates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BatchItemError {
    date: String,
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct BatchResponse {
    total: usize,
    succeeded: usize,
    failed: usize,
    results: Vec<DayQuery>,
    errors: Vec<BatchItemError>,
}

/// `POST /api/v1/holidays/batch`
///
/// body: `{"dates": ["2026-01-01", "2026-02-17"]}`
///
/// 单个日期非法不会中断整批，会记在 `errors` 里。
async fn batch(
    State(store): State<Arc<Store>>,
    Json(body): Json<BatchRequest>,
) -> Result<Json<BatchResponse>, ApiError> {
    if body.dates.is_empty() {
        return Err(ApiError::bad_request("empty_batch", "dates 不能为空"));
    }
    if body.dates.len() > BATCH_LIMIT {
        return Err(ApiError::bad_request(
            "batch_too_large",
            format!("单次最多查询 {BATCH_LIMIT} 个日期，当前 {}", body.dates.len()),
        ));
    }

    let mut results = Vec::with_capacity(body.dates.len());
    let mut errors = Vec::new();
    for raw in &body.dates {
        match normalize_date(raw) {
            Ok((_, day)) => match store.query_day(day) {
                Ok(item) => results.push(item),
                Err(e) => errors.push(BatchItemError {
                    date: raw.clone(),
                    code: "internal_error",
                    message: e.to_string(),
                }),
            },
            Err(e) => errors.push(BatchItemError {
                date: raw.clone(),
                code: "invalid_date",
                message: e.to_string(),
            }),
        }
    }

    Ok(Json(BatchResponse {
        total: body.dates.len(),
        succeeded: results.len(),
        failed: errors.len(),
        results,
        errors,
    }))
}

async fn not_found() -> ApiError {
    ApiError::not_found("接口不存在，可用接口：GET /api/v1/years、GET /api/v1/holiday?date=、GET /api/v1/holidays?year=、POST /api/v1/holidays/batch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::upsert;
    use crate::model::{DayType, Record};

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        let records = [
            ("2026-02-15", Category::Holiday, "Spring Festival", "春节", 4),
            ("2026-02-16", Category::Holiday, "Spring Festival", "春节", 4),
            ("2026-02-16", Category::InLieu, "Spring Festival", "春节", 4),
            ("2026-02-14", Category::Workday, "Spring Festival", "春节", 4),
        ];
        store
            .with_transaction(|tx| {
                for (date, category, name_en, name_zh, days) in records {
                    upsert(
                        tx,
                        &Record {
                            date: date.to_string(),
                            category,
                            name_en: name_en.to_string(),
                            name_zh: name_zh.to_string(),
                            statutory_days: days,
                            source_file: Some("test.json".to_string()),
                        },
                    )?;
                }
                Ok(())
            })
            .unwrap();
        Arc::new(store)
    }

    #[test]
    fn lookup_classifies_days() {
        let store = test_store();

        let holiday = lookup(&store, "2026-02-15").unwrap().0;
        assert_eq!(holiday.day_type, DayType::Holiday);
        assert!(holiday.is_holiday && holiday.data_available);
        assert_eq!(holiday.festival.unwrap().name_zh, "春节");

        let makeup = lookup(&store, "2026-02-14").unwrap().0;
        assert_eq!(makeup.day_type, DayType::MakeupWorkday);
        assert!(makeup.is_weekend && !makeup.is_rest_day);

        // 同一天既是放假日又是调休补假日
        let in_lieu = lookup(&store, "2026-02-16").unwrap().0;
        assert!(in_lieu.is_holiday && in_lieu.is_in_lieu);

        // 非法日期给出 400 + invalid_date
        let err = lookup(&store, "2026-2-15").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.code, "invalid_date");
    }

    #[test]
    fn year_listing_splits_categories() {
        let store = test_store();
        let body = list_year(&store, 2026).unwrap().0;
        assert!(body.data_available);
        assert_eq!(body.holidays.len(), 2);
        assert_eq!(body.in_lieu_days.len(), 1);
        assert_eq!(body.makeup_workdays.len(), 1);
        assert_eq!(body.total, 4);

        let empty = list_year(&store, 2011).unwrap().0;
        assert!(!empty.data_available);
        assert_eq!(empty.total, 0);
    }

    #[test]
    fn year_range_is_validated() {
        let store = test_store();
        let err = list_year(&store, 1200).unwrap_err();
        assert_eq!(err.code, "invalid_year");
    }
}
