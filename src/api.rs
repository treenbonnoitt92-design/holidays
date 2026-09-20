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
//!
//! 接口文档（新增，只读，不影响上面任何接口）：
//!
//! | 方法 | 路径 | 说明 |
//! |------|------|------|
//! | GET | `/docs` | Swagger UI，可查看每个字段说明并单独调试 |
//! | GET | `/api-docs/openapi.json` | OpenAPI 3 原始文档，可导入 Postman / Apifox / 代码生成器 |

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
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::db::Store;
use crate::model::{Category, DayQuery, Record, normalize_date};

mod docs;

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
        .with_state(store)
        // 接口文档：Swagger UI + 原始 OpenAPI JSON，均为只读，与业务接口互不影响
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", docs::ApiDoc::openapi()))
        // 纯查询服务，直接放开跨域，方便前端/脚本调用
        .layer(CorsLayer::permissive())
}

// ---------------------------------------------------------------- 错误处理

/// 统一错误响应外壳：`{"error":{"code":"...","message":"..."}}`
#[derive(Debug, Serialize, ToSchema)]
struct ErrorBody {
    /// 错误详情
    error: ErrorDetail,
}

/// 错误码与错误说明
#[derive(Debug, Serialize, ToSchema)]
struct ErrorDetail {
    /// 机器可读的错误码：`missing_date`、`invalid_date`、`missing_year`、`invalid_year`、
    /// `empty_batch`、`batch_too_large`、`not_found`、`internal_error`
    #[schema(value_type = String, example = "invalid_date")]
    code: &'static str,
    /// 面向调用方的错误说明
    #[schema(example = "日期格式应为 YYYY-MM-DD（如 2026-10-01），实际为 \"2026-2-15\"")]
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

/// 服务健康状态
///
/// 供容器 HEALTHCHECK 与负载均衡探活使用，无查询参数。
#[utoipa::path(
    get,
    path = "/health",
    tag = "元数据",
    summary = "健康检查",
    description = "返回服务状态与库内数据概览。容器 HEALTHCHECK 依赖该接口，请勿改动路径。",
    responses(
        (status = 200, description = "服务正常", body = docs::HealthResponse),
        (status = 500, description = "数据库不可用", body = ErrorBody),
    )
)]
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

/// 按日期查询节假日信息（查询参数写法）
///
/// `day_type` 直接给出四态归类，`is_rest_day` 是「实际是否不用上班」的最终答案；
/// `data_available` 为 `false` 表示该年份没有数据，而不是「不是节假日」。
#[utoipa::path(
    get,
    path = "/api/v1/holiday",
    tag = "查询",
    summary = "按日期查询（?date=YYYY-MM-DD）",
    params(
        ("date" = String, Query, description = "要查询的日期，格式 YYYY-MM-DD，必须真实存在", example = "2026-10-01")
    ),
    responses(
        (status = 200, description = "查询成功", body = DayQuery),
        (status = 400, description = "缺少 date 参数（`missing_date`）或日期非法（`invalid_date`）", body = ErrorBody),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
async fn day_by_query(
    State(store): State<Arc<Store>>,
    Query(params): Query<DateParams>,
) -> Result<Json<DayQuery>, ApiError> {
    let raw = params.date.ok_or_else(|| {
        ApiError::bad_request("missing_date", "缺少查询参数 date，例如 ?date=2026-10-01")
    })?;
    lookup(&store, &raw)
}

/// 按日期查询节假日信息（路径写法）
///
/// 与 `?date=` 写法返回完全相同的结构。
#[utoipa::path(
    get,
    path = "/api/v1/holiday/{date}",
    tag = "查询",
    summary = "按日期查询（/holiday/{date}）",
    params(
        ("date" = String, Path, description = "要查询的日期，格式 YYYY-MM-DD，必须真实存在", example = "2026-02-14")
    ),
    responses(
        (status = 200, description = "查询成功", body = DayQuery),
        (status = 400, description = "日期非法（`invalid_date`），如 2026-02-30", body = ErrorBody),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
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

/// 某年全部记录，按类别分组。
#[derive(Debug, Serialize, ToSchema)]
struct YearResponse {
    /// 年份
    #[schema(example = 2026)]
    year: i32,
    /// 该年是否有任何数据。`false` 时三个数组均为空
    #[schema(example = true)]
    data_available: bool,
    /// 三个数组的条数之和
    #[schema(example = 146)]
    total: usize,
    /// 放假日
    holidays: Vec<Record>,
    /// 调休上班日（多为周末补班）
    makeup_workdays: Vec<Record>,
    /// 调休补假日
    in_lieu_days: Vec<Record>,
}

/// 按年份查询该年全部记录（查询参数写法）
#[utoipa::path(
    get,
    path = "/api/v1/holidays",
    tag = "查询",
    summary = "按年份查询（?year=YYYY）",
    params(
        ("year" = i32, Query, description = "要查询的年份，允许范围 1900-2999", example = 2026)
    ),
    responses(
        (status = 200, description = "查询成功；该年无数据时 total 为 0、data_available 为 false", body = YearResponse),
        (status = 400, description = "缺少 year 参数（`missing_year`）或年份越界（`invalid_year`）", body = ErrorBody),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
async fn year_by_query(
    State(store): State<Arc<Store>>,
    Query(params): Query<YearParams>,
) -> Result<Json<YearResponse>, ApiError> {
    let year = params.year.ok_or_else(|| {
        ApiError::bad_request("missing_year", "缺少查询参数 year，例如 ?year=2026")
    })?;
    list_year(&store, year)
}

/// 按年份查询该年全部记录（路径写法）
///
/// 与 `?year=` 写法返回完全相同的结构。
#[utoipa::path(
    get,
    path = "/api/v1/year/{year}",
    tag = "查询",
    summary = "按年份查询（/year/{year}）",
    params(
        ("year" = i32, Path, description = "要查询的年份，允许范围 1900-2999", example = 2026)
    ),
    responses(
        (status = 200, description = "查询成功；该年无数据时 total 为 0、data_available 为 false", body = YearResponse),
        (status = 400, description = "年份越界（`invalid_year`）", body = ErrorBody),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
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

/// 列出数据库中有数据的年份
///
/// 用来判断某一年是否有数据，避免把「无数据」误读成「工作日」。
#[utoipa::path(
    get,
    path = "/api/v1/years",
    tag = "元数据",
    summary = "有数据的年份列表",
    responses(
        (status = 200, description = "年份列表，升序", body = docs::YearsResponse),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
async fn years(State(store): State<Arc<Store>>) -> Result<Json<serde_json::Value>, ApiError> {
    let years = store.available_years().map_err(ApiError::internal)?;
    Ok(Json(json!({
        "count": years.len(),
        "years": years,
    })))
}

/// 批量查询请求体。
#[derive(Debug, Deserialize, ToSchema)]
struct BatchRequest {
    /// 要查询的日期列表，格式 `YYYY-MM-DD`，最多 1000 个
    #[schema(example = json!(["2026-01-01", "2026-02-17", "2026-10-01"]))]
    dates: Vec<String>,
}

/// 批量查询里单条失败的原因。
#[derive(Debug, Serialize, ToSchema)]
struct BatchItemError {
    /// 请求里原样的日期字符串，未做规范化
    #[schema(example = "2026-2-15")]
    date: String,
    /// 错误码：`invalid_date` 或 `internal_error`
    #[schema(value_type = String, example = "invalid_date")]
    code: &'static str,
    /// 错误说明
    message: String,
}

/// 批量查询响应。
#[derive(Debug, Serialize, ToSchema)]
struct BatchResponse {
    /// 请求的日期个数
    #[schema(example = 3)]
    total: usize,
    /// 查询成功的个数，等于 `results` 的长度
    #[schema(example = 2)]
    succeeded: usize,
    /// 失败的个数，等于 `errors` 的长度
    #[schema(example = 1)]
    failed: usize,
    /// 成功的结果，保持请求顺序，不含失败项
    results: Vec<DayQuery>,
    /// 失败明细；单个日期非法不会中断整批
    errors: Vec<BatchItemError>,
}

/// 批量查询多个日期
///
/// 单个日期非法不会中断整批，失败项记在 `errors` 里，成功项在 `results` 里。
#[utoipa::path(
    post,
    path = "/api/v1/holidays/batch",
    tag = "查询",
    summary = "批量查询（最多 1000 个日期）",
    request_body = BatchRequest,
    responses(
        (status = 200, description = "返回统计与逐条结果；HTTP 状态码始终为 200，单条失败看 errors", body = BatchResponse),
        (status = 400, description = "dates 为空（`empty_batch`）或超过上限（`batch_too_large`）", body = ErrorBody),
        (status = 500, description = "数据库查询失败（`internal_error`）", body = ErrorBody),
    )
)]
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
