//! 中国节假日数据服务。
//!
//! - [`importer`]：把 `data/*.json` 导入 SQLite，重复日期覆盖旧值
//! - [`db`]：SQLite 存储与查询
//! - [`api`]：REST 查询接口
//! - [`model`]：领域模型与数据解析

pub mod api;
pub mod db;
pub mod importer;
pub mod model;
