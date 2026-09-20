//! 领域模型，以及 `data/*.json` 的解析规则。

use std::collections::BTreeMap;
use std::fmt;

use chrono::{NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `data/YYYY.json` 的原始结构。
///
/// ```json
/// {
///   "holidays":   { "2026-01-01": "New Year's Day,元旦,1" },
///   "workdays":   { "2026-01-04": "New Year's Day,元旦,1" },
///   "inLieuDays": { "2026-01-02": "New Year's Day,元旦,1" }
/// }
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct YearFile {
    /// 放假的日子。
    #[serde(default)]
    pub holidays: BTreeMap<String, String>,
    /// 调休上班的日子（多为周末，需要补班）。
    #[serde(default)]
    pub workdays: BTreeMap<String, String>,
    /// 放假日中由调休换来的那些天。
    #[serde(default, rename = "inLieuDays")]
    pub in_lieu_days: BTreeMap<String, String>,
}

/// 某个类别的全部记录，便于统一入库。
pub struct Entry<'a> {
    pub category: Category,
    pub records: &'a BTreeMap<String, String>,
}

impl YearFile {
    pub fn entries(&self) -> [Entry<'_>; 3] {
        [
            Entry {
                category: Category::Holiday,
                records: &self.holidays,
            },
            Entry {
                category: Category::Workday,
                records: &self.workdays,
            },
            Entry {
                category: Category::InLieu,
                records: &self.in_lieu_days,
            },
        ]
    }

    /// 文件内的记录总数。
    pub fn len(&self) -> usize {
        self.holidays.len() + self.workdays.len() + self.in_lieu_days.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 记录类别，对应数据文件里的三个集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// 放假日
    Holiday,
    /// 调休上班日
    Workday,
    /// 调休补假日
    InLieu,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::Holiday, Category::Workday, Category::InLieu];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Holiday => "holiday",
            Category::Workday => "workday",
            Category::InLieu => "in_lieu",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "holiday" => Some(Category::Holiday),
            "workday" => Some(Category::Workday),
            "in_lieu" => Some(Category::InLieu),
            _ => None,
        }
    }

    pub fn label_cn(self) -> &'static str {
        match self {
            Category::Holiday => "放假",
            Category::Workday => "调休上班",
            Category::InLieu => "调休补假",
        }
    }
}

/// 数据库中的一条记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Record {
    /// 规范化后的日期，`YYYY-MM-DD`
    #[schema(example = "2026-02-17")]
    pub date: String,
    /// 记录类别：放假 / 调休上班 / 调休补假
    pub category: Category,
    /// 节日英文名
    #[schema(example = "Spring Festival")]
    pub name_en: String,
    /// 节日中文名
    #[schema(example = "春节")]
    pub name_zh: String,
    /// 该节日的法定节假日天数（不是放假总天数）
    #[schema(example = 4)]
    pub statutory_days: i64,
    /// 数据来源文件，便于回溯；手工写入的数据为 null
    #[schema(example = "2026.json")]
    pub source_file: Option<String>,
}

/// 与日期关联的节日信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Festival {
    /// 节日英文名
    #[schema(example = "National Day")]
    pub name_en: String,
    /// 节日中文名
    #[schema(example = "国庆节")]
    pub name_zh: String,
    /// 该节日的法定节假日天数（如国庆 3 天、春节 4 天），不是放假总天数
    #[schema(example = 3)]
    pub statutory_days: i64,
}

/// 日期归类，给调用方一个可以直接判断的枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DayType {
    /// 法定放假日
    Holiday,
    /// 调休上班日（多为周末补班）
    MakeupWorkday,
    /// 普通周末
    Weekend,
    /// 普通工作日
    Workday,
}

/// 单个日期的查询结果。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DayQuery {
    /// 规范化后的日期，`YYYY-MM-DD`
    #[schema(example = "2026-10-01")]
    pub date: String,
    /// 年
    #[schema(example = 2026)]
    pub year: i32,
    /// 月，1-12
    #[schema(example = 10)]
    pub month: u32,
    /// 日，1-31
    #[schema(example = 1)]
    pub day: u32,
    /// 星期，1 = 周一 … 7 = 周日
    #[schema(example = 4)]
    pub day_of_week: u32,
    /// 星期的中文写法
    #[schema(value_type = String, example = "周四")]
    pub day_of_week_cn: &'static str,
    /// 是否周六或周日（只看日历，不考虑调休）
    #[schema(example = false)]
    pub is_weekend: bool,
    /// 数据库中是否存在该年份的数据。
    /// `false` 表示「这一年没有数据」，而不是「这一天不是节假日」——不要据此判定为工作日
    #[schema(example = true)]
    pub data_available: bool,
    /// 日期归类，四态之一；判断「是不是节假日」优先看这个字段
    pub day_type: DayType,
    /// 是否在放假日清单里
    #[schema(example = true)]
    pub is_holiday: bool,
    /// 是否需要调休上班（放假通知里被指定为上班的周末）
    #[schema(example = false)]
    pub is_makeup_workday: bool,
    /// 该放假日是否由调休换来（调休补假日）
    #[schema(example = false)]
    pub is_in_lieu: bool,
    /// 实际上不用上班：法定放假日，或未被调休占用的周末
    #[schema(example = true)]
    pub is_rest_day: bool,
    /// 命中的节日信息；该日没有对应记录时为 null
    pub festival: Option<Festival>,
}

/// 日期格式/取值非法。
#[derive(Debug, Clone)]
pub struct DateError(String);

impl DateError {
    pub fn new(message: impl Into<String>) -> Self {
        DateError(message.into())
    }
}

impl fmt::Display for DateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DateError {}

/// 校验并规范化日期，接受 `YYYY-MM-DD` 且必须真实存在（如 2026-02-30 会被拒绝）。
///
/// 返回 `(规范化字符串, NaiveDate)`。
pub fn normalize_date(raw: &str) -> Result<(String, NaiveDate), DateError> {
    let s = raw.trim();
    let b = s.as_bytes();
    let well_formed = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit);

    if !well_formed {
        return Err(DateError::new(format!(
            "日期格式应为 YYYY-MM-DD（如 2026-10-01），实际为 {raw:?}"
        )));
    }

    let day = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| DateError::new(format!("日期 {raw:?} 不合法：{e}")))?;

    Ok((day.format("%Y-%m-%d").to_string(), day))
}

/// 解析后的记录值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedValue {
    pub name_en: String,
    pub name_zh: String,
    pub statutory_days: i64,
}

/// 解析记录值，格式为 `英文名,中文名,法定天数`，例如 `Spring Festival,春节,4`。
///
/// 天数从最右侧的逗号分段取，英文名与中文名则以第一个逗号分隔，
/// 这样即便英文名里带逗号也不会把天数错位。
pub fn parse_value(raw: &str) -> Result<ParsedValue, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("值为空".to_string());
    }

    let (head, days) = raw
        .rsplit_once(',')
        .ok_or_else(|| format!("缺少法定天数分段：{raw:?}"))?;

    let (name_en, name_zh) = head
        .split_once(',')
        .ok_or_else(|| format!("缺少中文名分段：{raw:?}"))?;

    let statutory_days = days
        .trim()
        .parse::<i64>()
        .map_err(|_| format!("法定天数不是整数：{:?}", days.trim()))?;

    let name_en = name_en.trim();
    let name_zh = name_zh.trim();
    if name_en.is_empty() || name_zh.is_empty() {
        return Err(format!("中英文名不能为空：{raw:?}"));
    }

    Ok(ParsedValue {
        name_en: name_en.to_string(),
        name_zh: name_zh.to_string(),
        statutory_days,
    })
}

/// 星期的中文写法。
pub fn weekday_cn(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "周一",
        Weekday::Tue => "周二",
        Weekday::Wed => "周三",
        Weekday::Thu => "周四",
        Weekday::Fri => "周五",
        Weekday::Sat => "周六",
        Weekday::Sun => "周日",
    }
}

/// 是否周末。
pub fn is_weekend(weekday: Weekday) -> bool {
    matches!(weekday, Weekday::Sat | Weekday::Sun)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn parses_plain_value() {
        let v = parse_value("Spring Festival,春节,4").unwrap();
        assert_eq!(v.name_en, "Spring Festival");
        assert_eq!(v.name_zh, "春节");
        assert_eq!(v.statutory_days, 4);
    }

    #[test]
    fn parses_value_with_apostrophe() {
        let v = parse_value("New Year's Day,元旦,1").unwrap();
        assert_eq!(v.name_en, "New Year's Day");
        assert_eq!(v.statutory_days, 1);
    }

    #[test]
    fn parses_value_with_commas_in_english_name() {
        // 天数取最右分段，中英文名以第一个逗号切分，整体 trim
        let v = parse_value("Dragon Boat, Festival,端午,1").unwrap();
        assert_eq!(v.name_en, "Dragon Boat");
        assert_eq!(v.name_zh, "Festival,端午");
        assert_eq!(v.statutory_days, 1);
    }

    #[test]
    fn rejects_malformed_values() {
        assert!(parse_value("").is_err());
        assert!(parse_value("元旦").is_err());
        assert!(parse_value("New Year,元旦").is_err());
        assert!(parse_value("New Year,元旦,一天").is_err());
        assert!(parse_value("New Year,,1").is_err());
    }

    #[test]
    fn normalizes_valid_dates() {
        let (canon, day) = normalize_date("2026-10-01").unwrap();
        assert_eq!(canon, "2026-10-01");
        assert_eq!(day.year(), 2026);
        // 允许两侧空白
        assert_eq!(normalize_date("  2026-01-01 ").unwrap().0, "2026-01-01");
    }

    #[test]
    fn rejects_bad_dates() {
        for bad in [
            "2026-1-1",
            "2026/10/01",
            "20261001",
            "2026-02-30",
            "2025-02-29",
            "abc",
            "",
        ] {
            assert!(normalize_date(bad).is_err(), "{bad} 不应通过校验");
        }
        // 闰年是合法的
        assert!(normalize_date("2024-02-29").is_ok());
    }

    #[test]
    fn year_file_entries_cover_all_categories() {
        let json = r#"{
            "holidays": {"2026-01-01": "New Year's Day,元旦,1"},
            "workdays": {"2026-01-04": "New Year's Day,元旦,1"},
            "inLieuDays": {"2026-01-02": "New Year's Day,元旦,1"}
        }"#;
        let file: YearFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.len(), 3);
        for entry in file.entries() {
            assert_eq!(entry.records.len(), 1);
        }
    }

    #[test]
    fn year_file_tolerates_missing_keys() {
        let file: YearFile = serde_json::from_str(r#"{"holidays":{}}"#).unwrap();
        assert!(file.is_empty());
        assert_eq!(file.entries().len(), 3);
    }

    #[test]
    fn category_round_trips() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
        assert_eq!(Category::parse("nope"), None);
    }
}
