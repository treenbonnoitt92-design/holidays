//! OpenAPI 接口文档。
//!
//! **只描述已有接口，不改变任何路由与响应结构**；字段说明直接取各结构体字段上的文档注释，
//! 改注释即改文档。
//!
//! 由 [`super::router`] 挂到两个只读入口：
//!
//! | 方法 | 路径 | 说明 |
//! |------|------|------|
//! | GET | `/docs` | Swagger UI，可查看字段说明并单独发起请求调试 |
//! | GET | `/api-docs/openapi.json` | OpenAPI 3 原始文档，可导入 Postman / Apifox / 代码生成器 |

use serde::Serialize;
use utoipa::{OpenApi, ToSchema};

use crate::model::{Category, DayQuery, DayType, Festival, Record};

use super::{BatchItemError, BatchRequest, BatchResponse, ErrorBody, ErrorDetail, YearResponse};

/// `GET /health` 的响应体。
///
/// 处理器直接拼 JSON 返回，这里定义一份形状相同的结构体，仅用于生成文档。
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// 固定为 `ok`，能取到该响应即说明服务存活
    #[schema(value_type = String, example = "ok")]
    pub status: &'static str,
    /// 数据库中的记录总数（同一天可能有多条记录）
    #[schema(example = 920)]
    pub records: usize,
    /// 去重后的日期数
    #[schema(example = 769)]
    pub dates: usize,
    /// 有数据的年份，升序
    #[schema(example = json!([2004, 2025, 2026]))]
    pub years: Vec<i32>,
}

/// `GET /api/v1/years` 的响应体。同上，仅用于生成文档。
#[derive(Debug, Serialize, ToSchema)]
pub struct YearsResponse {
    /// 年份个数
    #[schema(example = 23)]
    pub count: usize,
    /// 有数据的年份，升序
    #[schema(example = json!([2004, 2025, 2026]))]
    pub years: Vec<i32>,
}

/// 服务的 OpenAPI 文档。
///
/// 直接由 `data/*.json` 导入的数据提供查询，无需鉴权、无写入接口。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "中国节假日查询服务",
        version = env!("CARGO_PKG_VERSION"),
        description = "按日期查询中国法定节假日、调休上班与调休补假信息。\n\n\
                       判读要点：\n\
                       - day_type 是四态归类：holiday / makeup_workday / weekend / workday\n\
                       - is_rest_day 是「实际是否不用上班」的最终答案\n\
                       - data_available 为 false 表示该年份没有数据，不代表这一天不是节假日\n\n\
                       错误响应统一为带 error / code / message 的包装对象；未匹配的路径返回 404 not_found。\n\
                       数据来源为国务院放假通知，覆盖年份见 GET /api/v1/years。",
    ),
    tags(
        (name = "查询", description = "按日期或年份查询节假日数据"),
        (name = "元数据", description = "服务状态与数据覆盖范围"),
    ),
    paths(
        super::health,
        super::day_by_query,
        super::day_by_path,
        super::year_by_query,
        super::year_by_path,
        super::years,
        super::batch,
    ),
    components(schemas(
        DayQuery,
        DayType,
        Festival,
        Record,
        Category,
        HealthResponse,
        YearsResponse,
        YearResponse,
        BatchRequest,
        BatchResponse,
        BatchItemError,
        ErrorBody,
        ErrorDetail,
    )),
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    /// 加了路由却忘了同步文档时，这里会失败。
    #[test]
    fn openapi_covers_every_route() {
        let doc = ApiDoc::openapi();
        let paths: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
        let expected = [
            "/health",
            "/api/v1/holiday",
            "/api/v1/holiday/{date}",
            "/api/v1/holidays",
            "/api/v1/holidays/batch",
            "/api/v1/year/{year}",
            "/api/v1/years",
        ];
        for path in expected {
            assert!(paths.contains(&path), "OpenAPI 文档缺少路径 {path}");
        }
        assert_eq!(
            paths.len(),
            expected.len(),
            "路由数量与文档不一致，请同步 src/api/docs.rs"
        );
    }

    /// 响应字段必须有说明，否则文档等于白给。
    #[test]
    fn schema_fields_carry_descriptions() {
        let value = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schemas = &value["components"]["schemas"];
        // 可空字段的说明会落在 oneOf 分支里，所以只检查该属性的 JSON 里出现过 description
        let cases: [(&str, &[&str]); 5] = [
            (
                "DayQuery",
                &["date", "day_type", "is_rest_day", "data_available", "festival"],
            ),
            ("Record", &["statutory_days", "source_file", "category"]),
            ("YearResponse", &["data_available", "total", "in_lieu_days"]),
            ("BatchResponse", &["results", "errors"]),
            ("HealthResponse", &["status", "records", "dates", "years"]),
        ];
        for (schema, fields) in cases {
            for field in fields {
                let prop = &schemas[schema]["properties"][field];
                assert!(!prop.is_null(), "{schema}.{field} 不存在");
                assert!(
                    prop.to_string().contains("\"description\""),
                    "{schema}.{field} 缺少字段说明"
                );
            }
        }
    }
}
