//! SQLite 存储层：建表、覆盖式写入、按日期/年份查询。

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::model::{
    Category, DayQuery, DayType, Festival, Record, is_weekend, weekday_cn,
};

/// 表结构。`(date, category)` 作为主键，因此同一天重新导入时会覆盖旧记录。
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS holiday_records (
    date           TEXT    NOT NULL,
    category       TEXT    NOT NULL,
    name_en        TEXT    NOT NULL,
    name_zh        TEXT    NOT NULL,
    statutory_days INTEGER NOT NULL,
    source_file    TEXT,
    updated_at     TEXT    NOT NULL,
    PRIMARY KEY (date, category)
);

CREATE INDEX IF NOT EXISTS idx_holiday_records_year
    ON holiday_records (substr(date, 1, 4));

CREATE TABLE IF NOT EXISTS import_log (
    source_file  TEXT    PRIMARY KEY,
    imported_at  TEXT    NOT NULL,
    record_count INTEGER NOT NULL
);
"#;

/// 数据库整体概况。
#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    /// 记录总条数
    pub total: usize,
    /// 涉及的不同日期数
    pub distinct_dates: usize,
    /// 有数据的年份，升序
    pub years: Vec<i32>,
    pub holiday: usize,
    pub workday: usize,
    pub in_lieu: usize,
}

/// 一次导入操作的结果统计。
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOutcome {
    pub inserted: usize,
    pub updated: usize,
}

impl WriteOutcome {
    pub fn total(&self) -> usize {
        self.inserted + self.updated
    }
}

/// SQLite 连接句柄。
///
/// 内部只有一条连接，用 `Mutex` 串行化访问。节假日查询是内存命中级别的
/// 极短操作，单连接完全够用，也省掉了连接池依赖。
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// 打开（不存在则创建）数据库，并初始化表结构。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("打开 SQLite 失败：{}", path.display()))?;
        Self::from_conn(conn)
    }

    /// 内存数据库，供测试使用。
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        // WAL 下读写互不阻塞；journal_mode 会返回一行结果，故用 query_row。
        let _mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .context("设置 journal_mode 失败")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("初始化表结构失败")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // 即便某次查询触发了 panic，也继续复用这条连接，而不是让服务整体不可用。
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 在一个事务里执行 `f`，返回其返回值；`f` 出错则整体回滚。
    pub fn with_transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// 查询单个日期的节假日信息。`day` 需为已校验的日期。
    pub fn query_day(&self, day: NaiveDate) -> Result<DayQuery> {
        let key = day.format("%Y-%m-%d").to_string();
        let year = day.year();
        let conn = self.lock();

        let hits: Vec<(String, String, String, i64)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT category, name_en, name_zh, statutory_days
                 FROM holiday_records WHERE date = ?1",
            )?;
            stmt.query_map([&key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut holiday = None;
        let mut in_lieu = None;
        let mut workday = None;
        for (category, name_en, name_zh, statutory_days) in hits {
            let festival = Festival {
                name_en,
                name_zh,
                statutory_days,
            };
            match Category::parse(&category) {
                Some(Category::Holiday) => holiday = Some(festival),
                Some(Category::InLieu) => in_lieu = Some(festival),
                Some(Category::Workday) => workday = Some(festival),
                None => {}
            }
        }

        let data_available: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM holiday_records
                           WHERE date >= ?1 AND date <= ?2)",
            params![format!("{year}-01-01"), format!("{year}-12-31")],
            |r| r.get(0),
        )?;

        let weekend = is_weekend(day.weekday());
        let is_holiday = holiday.is_some();
        let is_makeup_workday = workday.is_some();

        let day_type = if is_holiday {
            DayType::Holiday
        } else if is_makeup_workday {
            DayType::MakeupWorkday
        } else if weekend {
            DayType::Weekend
        } else {
            DayType::Workday
        };

        Ok(DayQuery {
            date: key,
            year,
            month: day.month(),
            day: day.day(),
            day_of_week: day.weekday().number_from_monday(),
            day_of_week_cn: weekday_cn(day.weekday()),
            is_weekend: weekend,
            data_available,
            day_type,
            is_holiday,
            is_makeup_workday,
            is_in_lieu: in_lieu.is_some(),
            is_rest_day: is_holiday || (weekend && !is_makeup_workday),
            // 放假记录优先；调休补假日与调休上班日作为兜底，保证总能给出节日名
            festival: holiday.or(in_lieu).or(workday),
        })
    }

    /// 取某一年的全部记录，按日期、类别排序。
    pub fn year_records(&self, year: i32) -> Result<Vec<Record>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT date, category, name_en, name_zh, statutory_days, source_file
             FROM holiday_records
             WHERE substr(date, 1, 4) = ?1
             ORDER BY date, category",
        )?;
        let rows = stmt
            .query_map([year.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(rows
            .into_iter()
            .filter_map(|(date, cat, name_en, name_zh, days, src)| {
                Some(Record {
                    date,
                    category: Category::parse(&cat)?,
                    name_en,
                    name_zh,
                    statutory_days: days,
                    source_file: src,
                })
            })
            .collect())
    }

    /// 数据库里有数据的年份，升序。
    pub fn available_years(&self) -> Result<Vec<i32>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT substr(date, 1, 4) AS y FROM holiday_records ORDER BY y",
        )?;
        let years = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();
        Ok(years)
    }

    /// 整体统计。
    pub fn stats(&self) -> Result<StoreStats> {
        let conn = self.lock();
        let count = |category: &str| -> Result<usize> {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM holiday_records WHERE category = ?1",
                [category],
                |r| r.get(0),
            )?;
            Ok(n as usize)
        };

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM holiday_records", [], |r| r.get(0))?;
        let distinct_dates: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT date) FROM holiday_records",
            [],
            |r| r.get(0),
        )?;
        let years = {
            let mut stmt =
                conn.prepare_cached("SELECT DISTINCT substr(date,1,4) AS y FROM holiday_records")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|s| s.parse::<i32>().ok())
                .collect::<Vec<_>>()
        };

        Ok(StoreStats {
            total: total as usize,
            distinct_dates: distinct_dates as usize,
            years,
            holiday: count(Category::Holiday.as_str())?,
            workday: count(Category::Workday.as_str())?,
            in_lieu: count(Category::InLieu.as_str())?,
        })
    }

    /// 已导入过的文件记录，供排查「哪次导入覆盖了什么」。
    pub fn import_log(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT source_file, imported_at, record_count FROM import_log ORDER BY source_file",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// 写入一条记录。返回 `true` 表示新增，`false` 表示覆盖了已有记录。
///
/// 冲突键是 `(date, category)`：同一天同类别重复导入即为覆盖。
pub fn upsert(tx: &rusqlite::Transaction<'_>, record: &Record) -> Result<bool> {
    let existed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM holiday_records WHERE date = ?1 AND category = ?2)",
        params![record.date, record.category.as_str()],
        |r| r.get(0),
    )?;

    tx.execute(
        "INSERT INTO holiday_records
             (date, category, name_en, name_zh, statutory_days, source_file, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now', 'localtime'))
         ON CONFLICT(date, category) DO UPDATE SET
             name_en        = excluded.name_en,
             name_zh        = excluded.name_zh,
             statutory_days = excluded.statutory_days,
             source_file    = excluded.source_file,
             updated_at     = datetime('now', 'localtime')",
        params![
            record.date,
            record.category.as_str(),
            record.name_en,
            record.name_zh,
            record.statutory_days,
            record.source_file,
        ],
    )?;

    Ok(!existed)
}

/// 记录一次文件导入。
pub fn record_import(
    tx: &rusqlite::Transaction<'_>,
    source_file: &str,
    record_count: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO import_log (source_file, imported_at, record_count)
         VALUES (?1, datetime('now', 'localtime'), ?2)
         ON CONFLICT(source_file) DO UPDATE SET
             imported_at  = excluded.imported_at,
             record_count = excluded.record_count",
        params![source_file, record_count],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::normalize_date;

    fn rec(date: &str, category: Category, name_zh: &str, days: i64) -> Record {
        Record {
            date: date.to_string(),
            category,
            name_en: "Test".to_string(),
            name_zh: name_zh.to_string(),
            statutory_days: days,
            source_file: Some("test.json".to_string()),
        }
    }

    fn store_with(records: &[Record]) -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .with_transaction(|tx| {
                for r in records {
                    upsert(tx, r)?;
                }
                Ok(())
            })
            .unwrap();
        store
    }

    #[test]
    fn importing_same_date_twice_overwrites() {
        let store = store_with(&[rec("2026-01-01", Category::Holiday, "元旦", 1)]);

        store
            .with_transaction(|tx| {
                // 同一天同类别，第二次应判定为覆盖
                assert!(!upsert(tx, &rec("2026-01-01", Category::Holiday, "元旦", 1))?);
                // 新增一个「调休上班」，属于不同类别，应判定为新增
                assert!(upsert(tx, &rec("2026-01-04", Category::Workday, "元旦", 1))?);
                Ok(())
            })
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.total, 2);

        let day = store
            .query_day(normalize_date("2026-01-01").unwrap().1)
            .unwrap();
        assert!(day.is_holiday);
        assert!(day.data_available);
    }

    #[test]
    fn overwrite_updates_fields() {
        let store = store_with(&[rec("2026-05-01", Category::Holiday, "劳动节", 1)]);
        store
            .with_transaction(|tx| upsert(tx, &rec("2026-05-01", Category::Holiday, "劳动节", 2)))
            .unwrap();

        let day = store
            .query_day(normalize_date("2026-05-01").unwrap().1)
            .unwrap();
        assert_eq!(day.festival.unwrap().statutory_days, 2);
        assert_eq!(store.stats().unwrap().total, 1, "覆盖不应产生第二条记录");
    }

    #[test]
    fn classifies_holiday_makeup_and_plain_days() {
        let store = store_with(&[
            rec("2026-02-17", Category::Holiday, "春节", 4),
            rec("2026-02-14", Category::Workday, "春节", 4),
            rec("2026-02-20", Category::Holiday, "春节", 4),
            rec("2026-02-20", Category::InLieu, "春节", 4),
        ]);

        // 2026-02-17 是周二，放假
        let holiday = store
            .query_day(normalize_date("2026-02-17").unwrap().1)
            .unwrap();
        assert_eq!(holiday.day_type, DayType::Holiday);
        assert!(holiday.is_holiday && holiday.is_rest_day);
        assert_eq!(holiday.day_of_week_cn, "周二");

        // 2026-02-14 是周六，但要上班
        let makeup = store
            .query_day(normalize_date("2026-02-14").unwrap().1)
            .unwrap();
        assert_eq!(makeup.day_type, DayType::MakeupWorkday);
        assert!(makeup.is_weekend && makeup.is_makeup_workday);
        assert!(!makeup.is_rest_day);

        // 2026-05-06 是周三，无任何记录
        let plain = store
            .query_day(normalize_date("2026-05-06").unwrap().1)
            .unwrap();
        assert_eq!(plain.day_type, DayType::Workday);
        assert!(!plain.is_holiday && !plain.is_rest_day);
        assert!(plain.data_available, "同一年有数据，应标记为已有数据");
        assert!(plain.festival.is_none());

        // 1999 年完全没有数据
        let unknown = store
            .query_day(normalize_date("1999-05-06").unwrap().1)
            .unwrap();
        assert!(!unknown.data_available);
    }

    #[test]
    fn in_lieu_day_is_flagged() {
        let store = store_with(&[
            rec("2026-02-20", Category::Holiday, "春节", 4),
            rec("2026-02-20", Category::InLieu, "春节", 4),
        ]);
        let day = store
            .query_day(normalize_date("2026-02-20").unwrap().1)
            .unwrap();
        assert!(day.is_holiday && day.is_in_lieu);
        assert_eq!(day.festival.unwrap().name_zh, "春节");
    }

    #[test]
    fn year_records_and_available_years() {
        let store = store_with(&[
            rec("2024-01-01", Category::Holiday, "元旦", 1),
            rec("2026-01-01", Category::Holiday, "元旦", 1),
        ]);
        assert_eq!(store.year_records(2024).unwrap().len(), 1);
        assert_eq!(store.year_records(2026).unwrap().len(), 1);
        assert!(store.year_records(2025).unwrap().is_empty());
        assert_eq!(store.available_years().unwrap(), vec![2024, 2026]);

        let stats = store.stats().unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.distinct_dates, 2);
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let store = Store::open_in_memory().unwrap();
        let result: Result<()> = store.with_transaction(|tx| {
            upsert(tx, &rec("2026-01-01", Category::Holiday, "元旦", 1))?;
            anyhow::bail!("模拟中途失败");
        });
        assert!(result.is_err());
        assert_eq!(store.stats().unwrap().total, 0, "失败的事务不应留下数据");
    }
}
