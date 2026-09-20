# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概览

中国节假日查询服务(Rust 2024 edition):把 `data/*.json` 导入 SQLite,提供 CLI 与 REST API(axum 0.8)两种查询方式。单个二进制、无外部服务依赖(rusqlite bundled)。

## 常用命令

```bash
cargo build                              # 构建
cargo test                               # 全部测试(内联 #[cfg(test)] 模块,内存 SQLite,无需真实数据库)
cargo test lookup_classifies_days        # 运行单个测试(按测试名过滤)
cargo fmt && cargo clippy --all-targets  # 格式化 + lint

# 本地跑服务
cargo run -- import                      # 导入 data/*.json → holidays.db(幂等,可反复执行)
cargo run -- serve                       # http://127.0.0.1:8080
cargo run -- get 2026-10-01              # CLI 快查某天
cargo run -- list 2026                   # 列某年全部记录
cargo run -- info                        # 数据库概况与导入日志

# 部署:同步源码到远程 hsnut,在服务器上构建镜像并重启容器
./deploy.sh
VERSION=0.1.1 ./deploy.sh                # 指定镜像 tag
./deploy.sh --build-only                 # 只构建镜像,不动线上容器
```

## 架构

数据流:`data/*.json`(唯一事实来源)→ importer → SQLite → CLI / REST 查询。容器每次启动时 entrypoint(`docker/entrypoint.sh`)都会重新导入(幂等 upsert),因此永远不要手工改数据库——改数据就改 JSON,然后重启容器。

模块职责:

- `src/model.rs` — 领域模型与数据解析:日期校验 `normalize_date`(拒绝 2026-02-30 这类日期)、记录值解析 `parse_value`
- `src/importer.rs` — 扫描 data 目录导入;单文件一个事务,解析失败整体回滚;单条记录非法只跳过不报错
- `src/db.rs` — `Store`(单条 SQLite 连接 + Mutex,刻意不用连接池:查询是微秒级)、建表、`upsert`、`query_day` 的日期分类逻辑
- `src/api.rs` — axum 路由与统一错误信封
- `src/main.rs` — clap CLI 壳(import/serve/get/list/info/healthcheck 子命令);业务逻辑都在 lib 里,main 只做参数解析与终端展示(含中文对齐表格)

关键语义(需要跨文件才能看清的):

- 表主键是 `(date, category)`:同一天可以同时是放假日和调休补假日;同键重复导入即覆盖
- 文件按文件名升序处理,后导入的文件覆盖先导入的同键记录
- `data_available` 是**年份级**标志(该年是否有任何数据),用来区分「无数据」与「不是节假日」
- `day_type` 优先级:Holiday > MakeupWorkday > Weekend > Workday;`is_rest_day = 放假 || (周末 && 非调休上班)`
- API 错误响应为 `{"error":{"code":"...","message":"..."}}`,成功响应直接返回数据对象(无 code/data 包装)——遵循本项目现有约定,不要套用其他项目的响应格式
- CORS 全放开(纯只读查询服务,刻意为之)
- 容器 HEALTHCHECK 用二进制自带的 `holidays healthcheck` 子命令,镜像里不装 curl

## 约定

- **所有注释、文档、CLI 输出、错误与日志消息用中文**,新代码保持一致
- CLI 的 stdout 只放结构化结果,日志全部走 stderr(tracing writer 配置在 `main.rs` 的 `init_tracing`),保证输出可管道
- `data/YYYY.json` 的值格式为 `英文名,中文名,法定天数`:天数取最右逗号分段,中英文名以第一个逗号切分
- 时区恒为 Asia/Shanghai:运行镜像装 tzdata、SQLite 写入用 `datetime('now','localtime')`、展示用 chrono Local;日期逻辑保持 naive date,不做时区换算
- Windows 开发 → Linux 部署:`.sh` / `Dockerfile` / `.yml` 必须保持 LF 行尾(镜像里对 entrypoint.sh 有 CRLF 兜底,但不要依赖它)
- `deploy.sh` 用 tar-over-ssh 同步(Windows Git Bash 无 rsync),远端 `db/` 是持久化数据,同步时绝不触碰
