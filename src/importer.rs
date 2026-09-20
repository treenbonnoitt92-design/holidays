//! 把 `data/` 下的 JSON 文件导入 SQLite。
//!
//! 约定「每年一个文件」，但以文件内容为准：文件里出现哪一年就写哪一年。
//! 文件按名称升序处理，同名日期后写入的覆盖先写入的。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Datelike;
use tracing::{info, warn};

use crate::db::{self, Store, WriteOutcome};
use crate::model::{YearFile, normalize_date, parse_value};

/// 单个文件的导入结果。
#[derive(Debug, Default)]
pub struct FileReport {
    pub file: String,
    /// 文件内容实际覆盖到的年份
    pub years: BTreeSet<i32>,
    pub outcome: WriteOutcome,
    /// 被跳过的条目，附带原因
    pub invalid: Vec<String>,
}

impl FileReport {
    pub fn year_range(&self) -> String {
        match (self.years.iter().next(), self.years.iter().next_back()) {
            (Some(min), Some(max)) if min == max => min.to_string(),
            (Some(min), Some(max)) => format!("{min}..{max}"),
            _ => "-".to_string(),
        }
    }
}

/// 一次 `import` 的整体结果。
#[derive(Debug, Default)]
pub struct ImportReport {
    pub files: Vec<FileReport>,
    /// 读取或解析失败的文件：(文件名, 原因)
    pub failures: Vec<(String, String)>,
    pub outcome: WriteOutcome,
    pub invalid: usize,
}

impl ImportReport {
    /// 是否存在需要运维关注的问题。
    pub fn has_problems(&self) -> bool {
        !self.failures.is_empty() || self.invalid > 0
    }
}

/// 扫描目录并导入。`year_filter` 非空时只处理文件名年份命中的文件。
pub fn import_dir(store: &Store, dir: &Path, year_filter: &[i32]) -> Result<ImportReport> {
    if !dir.is_dir() {
        anyhow::bail!("数据目录不存在或不是目录：{}", dir.display());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("读取目录失败：{}", dir.display()))? {
        let path = entry?.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        files.push(path);
    }

    if files.is_empty() {
        anyhow::bail!("目录 {} 下没有找到 .json 文件", dir.display());
    }

    // 按文件名升序，保证覆盖顺序稳定可复现
    files.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));

    let mut report = ImportReport::default();
    for path in files {
        let name = file_name(&path);

        let name_year = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i32>().ok());

        if year_filter.is_empty() {
            if name_year.is_none() {
                warn!(file = %name, "文件名不是 4 位年份，将按文件内容导入");
            }
        } else {
            // 过滤按文件名匹配，符合「每年一个文件」的约定
            match name_year {
                Some(y) if year_filter.contains(&y) => {}
                _ => continue,
            }
        }

        match import_file(store, &path, &name) {
            Ok(file_report) => {
                info!(
                    file = %name,
                    inserted = file_report.outcome.inserted,
                    updated = file_report.outcome.updated,
                    skipped = file_report.invalid.len(),
                    "导入完成"
                );
                report.outcome.inserted += file_report.outcome.inserted;
                report.outcome.updated += file_report.outcome.updated;
                report.invalid += file_report.invalid.len();
                if !file_report.invalid.is_empty() {
                    warn!(file = %name, "有 {} 条记录被跳过", file_report.invalid.len());
                }
                report.files.push(file_report);
            }
            Err(e) => {
                warn!(file = %name, error = %e, "导入失败");
                report.failures.push((name, format!("{e:#}")));
            }
        }
    }

    Ok(report)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 导入单个文件。解析或写入失败会返回 `Err`，此时该文件整体不留数据。
fn import_file(store: &Store, path: &Path, name: &str) -> Result<FileReport> {
    let text =
        fs::read_to_string(path).with_context(|| format!("读取文件失败：{}", path.display()))?;
    let parsed: YearFile = serde_json::from_str(&text)
        .with_context(|| format!("解析 JSON 失败：{}", path.display()))?;

    if parsed.is_empty() {
        warn!(file = %name, "文件中没有任何记录");
        return Ok(FileReport {
            file: name.to_string(),
            ..Default::default()
        });
    }

    let mut outcome = WriteOutcome::default();
    let mut invalid = Vec::new();
    let mut years = BTreeSet::new();

    store.with_transaction(|tx| {
        for entry in parsed.entries() {
            for (raw_date, raw_value) in entry.records {
                let (date, day) = match normalize_date(raw_date) {
                    Ok(v) => v,
                    Err(e) => {
                        invalid.push(format!("{raw_date}: {e}"));
                        continue;
                    }
                };
                let value = match parse_value(raw_value) {
                    Ok(v) => v,
                    Err(e) => {
                        invalid.push(format!("{raw_date}: {e}"));
                        continue;
                    }
                };

                let record = crate::model::Record {
                    date,
                    category: entry.category,
                    name_en: value.name_en,
                    name_zh: value.name_zh,
                    statutory_days: value.statutory_days,
                    source_file: Some(name.to_string()),
                };

                if db::upsert(tx, &record)? {
                    outcome.inserted += 1;
                } else {
                    outcome.updated += 1;
                }
                years.insert(day.year());
            }
        }

        db::record_import(tx, name, outcome.total() as i64)?;
        Ok(())
    })?;

    Ok(FileReport {
        file: name.to_string(),
        years,
        outcome,
        invalid,
    })
}

/// 列出数据目录里的年份文件，供 `--list-files` 之类的展示使用。
pub fn discover_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("读取目录失败：{}", dir.display()))? {
        let path = entry?.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn imports_directory_and_covers_conflicts_by_file_order() {
        let tmp = std::env::temp_dir().join(format!("holidays-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        write(
            &tmp,
            "2024.json",
            r#"{"holidays":{"2024-01-01":"New Year's Day,元旦,1"},
                "workdays":{},"inLieuDays":{}}"#,
        );
        write(
            &tmp,
            "2025.json",
            r#"{"holidays":{"2025-01-01":"New Year's Day,元旦,1"},
                "workdays":{"2025-01-26":"Spring Festival,春节,4"},
                "inLieuDays":{}}"#,
        );
        // 后写入的文件包含同名日期，旧值应被覆盖
        write(
            &tmp,
            "2026.json",
            r#"{"holidays":{"2024-01-01":"New Year's Day,元旦,2",
                            "2026-01-01":"New Year's Day,元旦,1"},
                "workdays":{},"inLieuDays":{}}"#,
        );
        // 非法条目应被跳过而不影响其他记录
        write(
            &tmp,
            "2027.json",
            r#"{"holidays":{"2027-02-30":"Bad,坏,1","2027-10-01":"National Day,国庆节,3"},
                "workdays":{},"inLieuDays":{}}"#,
        );

        let store = Store::open_in_memory().unwrap();
        let report = import_dir(&store, &tmp, &[]).unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.files.len(), 4);
        assert_eq!(report.invalid, 1, "2027-02-30 应被跳过");

        let stats = store.stats().unwrap();
        assert_eq!(stats.total, 5);
        assert_eq!(stats.holiday, 4);
        assert_eq!(stats.workday, 1);
        assert_eq!(stats.years, vec![2024, 2025, 2026, 2027]);

        // 2024-01-01 被 2026.json 覆盖为 2 天
        let day = store
            .query_day(normalize_date("2024-01-01").unwrap().1)
            .unwrap();
        let festival = day.festival.unwrap();
        assert_eq!(festival.statutory_days, 2);
        assert_eq!(
            store.year_records(2024).unwrap()[0].source_file.as_deref(),
            Some("2026.json")
        );

        // 重复导入是幂等的：文件里共 6 条有效记录（2024-01-01 在 2024/2026 各出现一次），
        // 全部命中已有主键 → 计为覆盖，且库内条数不变。
        let again = import_dir(&store, &tmp, &[]).unwrap();
        assert_eq!(again.outcome.inserted, 0);
        assert_eq!(again.outcome.updated, 6);
        let restats = store.stats().unwrap();
        assert_eq!(restats.total, 5, "覆盖不应新增记录");
        assert_eq!(restats.distinct_dates, 5);

        // 年份过滤
        let filtered = import_dir(&store, &tmp, &[2025]).unwrap();
        assert_eq!(filtered.files.len(), 1);
        assert_eq!(filtered.files[0].file, "2025.json");
        assert_eq!(
            store.year_records(2025).unwrap()[0].category,
            Category::Holiday
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_directory_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        let missing = std::env::temp_dir().join("holidays-definitely-not-here");
        assert!(import_dir(&store, &missing, &[]).is_err());
    }

    #[test]
    fn broken_json_is_reported_without_killing_other_files() {
        let tmp = std::env::temp_dir().join(format!("holidays-badjson-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "2024.json", "{ not json");
        write(
            &tmp,
            "2025.json",
            r#"{"holidays":{"2025-01-01":"New Year's Day,元旦,1"}}"#,
        );

        let store = Store::open_in_memory().unwrap();
        let report = import_dir(&store, &tmp, &[]).unwrap();

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, "2024.json");
        assert_eq!(report.files.len(), 1);
        assert_eq!(store.stats().unwrap().total, 1);

        let _ = fs::remove_dir_all(&tmp);
    }
}
