# holidays — 中国节假日查询服务

`data/*.json` 是唯一事实来源:导入 SQLite 后,同一个二进制提供 **CLI** 与 **REST API**(axum)两种查询方式。单二进制、零外部服务依赖(rusqlite bundled SQLite),自带 Docker 部署。

## 特性

- 单日查询直接给出归类结果:`holiday` / `makeup_workday` / `weekend` / `workday`,不用自己拼「放假 + 调休 + 周末」的判断逻辑
- 区分「该年无数据」与「不是节假日」(`data_available` 字段)
- 导入幂等:主键 `(date, category)`,重复导入覆盖旧值,容器可随意重启
- 同一天可同时挂多个类别(如既是放假日又是调休补假日)

## 快速开始

```bash
cargo run -- import      # 导入 data/*.json → holidays.db
cargo run -- serve       # 启动 http://127.0.0.1:8080
```

CLI 子命令:

```bash
holidays get 2026-10-01        # 查某天,输出 JSON
holidays list 2026             # 列某年全部记录(表格);--json 输出 JSON
holidays import --year 2026    # 只导入指定年份的文件
holidays info                  # 数据库概况与导入日志
holidays healthcheck           # HTTP 探针(容器 HEALTHCHECK 用)
```

## REST API

| 方法 | 路径 | 说明 |
|------|------|------|
| GET  | `/health` | 健康检查,附带记录数 |
| GET  | `/api/v1/holiday?date=YYYY-MM-DD` | 查询单个日期 |
| GET  | `/api/v1/holiday/{date}` | 查询单个日期(路径写法) |
| GET  | `/api/v1/holidays?year=YYYY` | 列出某年全部记录 |
| GET  | `/api/v1/year/{year}` | 列出某年全部记录(路径写法) |
| GET  | `/api/v1/years` | 列出有数据的年份 |
| POST | `/api/v1/holidays/batch` | 批量查询,body `{"dates":["..."]}`,上限 1000 |
| GET  | `/docs` | Swagger UI 接口文档(可查看字段说明并单独调试) |
| GET  | `/api-docs/openapi.json` | OpenAPI 3 文档(JSON,可导入 Postman / Apifox / 代码生成器) |

### 查询单日

```bash
curl "http://127.0.0.1:8080/api/v1/holiday?date=2026-10-01"
```

```json
{
  "date": "2026-10-01",
  "year": 2026,
  "month": 10,
  "day": 1,
  "day_of_week": 4,
  "day_of_week_cn": "周四",
  "is_weekend": false,
  "data_available": true,
  "day_type": "holiday",
  "is_holiday": true,
  "is_makeup_workday": false,
  "is_in_lieu": false,
  "is_rest_day": true,
  "festival": {
    "name_en": "National Day",
    "name_zh": "国庆节",
    "statutory_days": 3
  }
}
```

调休上班日示例 —— 2026-02-14 周六,春节补班:

```json
{
  "date": "2026-02-14",
  "day_of_week_cn": "周六",
  "is_weekend": true,
  "day_type": "makeup_workday",
  "is_rest_day": false,
  "festival": { "name_en": "Spring Festival", "name_zh": "春节", "statutory_days": 4 }
}
```

`is_rest_day` 是最终答案:法定假日,或未被调休占用的周末。

错误统一为 `{"error":{"code":"...","message":"..."}}`:

```bash
curl "http://127.0.0.1:8080/api/v1/holiday?date=2026-02-30"
# 400 {"error":{"code":"invalid_date","message":"日期 \"2026-02-30\" 不合法:input is out of range"}}
```

## 接口文档

服务自带 **OpenAPI 3** 文档,随代码生成,不需要额外部署:

| 入口 | 说明 |
|------|------|
| `/docs` | Swagger UI,每个响应字段都有中文说明,可直接 Try it out 单独调试 |
| `/api-docs/openapi.json` | 原始文档,可导入 Postman / Apifox / 客户端代码生成器 |

```bash
cargo run -- serve
# 浏览器打开 http://127.0.0.1:8080/docs
curl -s http://127.0.0.1:8080/api-docs/openapi.json -o openapi.json
```

字段说明直接来自 Rust 结构体字段上的文档注释(`src/model.rs`、`src/api.rs`),改注释即改文档。
新增或改动接口后,要同步 `src/api/docs.rs` 里的 `paths` / `components` —— 漏了测试会失败。

## 数据格式

`data/` 每年一个文件(如 `2026.json`),三个集合按日期为键:

```json
{
  "holidays":   { "2026-01-01": "New Year's Day,元旦,1" },
  "workdays":   { "2026-01-04": "New Year's Day,元旦,1" },
  "inLieuDays": { "2026-01-02": "New Year's Day,元旦,1" }
}
```

| 集合 | 含义 |
|------|------|
| `holidays` | 放假的日子 |
| `workdays` | 调休上班的日子(多为周末补班) |
| `inLieuDays` | 放假日中由调休换来的那些天 |

值为 `英文名,中文名,法定天数`(天数取最右逗号分段)。新增年份只需放入 `data/YYYY.json` 并重新导入/重启容器。

## Docker 部署

```bash
docker build -t holidays:0.1.0 .
docker compose up -d          # 默认对外端口 13480 → 容器 8080
curl http://localhost:13480/health
```

容器启动时 entrypoint 自动把挂载的 JSON 重新导入 SQLite(幂等),数据库落在宿主机 `./db/`,重建容器不丢数据。

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HOLIDAYS_DB` | `/data/db/holidays.db` | SQLite 文件路径 |
| `HOLIDAYS_DATA_DIR` | `/data/json` | 源数据目录 |
| `HOLIDAYS_AUTO_IMPORT` | `1` | 启动时是否自动导入 |
| `HOLIDAYS_ADDR` | `0.0.0.0:8080` | 监听地址 |
| `RUST_LOG` | `info` | 日志级别 |
| `HOLIDAYS_VERSION` | `0.1.0` | compose 使用的镜像 tag |
| `HOLIDAYS_EXTERNAL_PORT` | `13480` | 对外端口 |

远程部署用 `deploy.sh`(tar-over-ssh 同步源码到服务器构建,详见脚本头部注释),回滚只需改 `HOLIDAYS_VERSION` 重新 `docker compose up -d`。

部署目标主机不写进仓库,放在本机 `.deploy.local`(已被 `.gitignore` 忽略,也不会同步到远端):

```bash
# .deploy.local
REMOTE=user@example.com
EXTERNAL_PORT=13480
```

```bash
./deploy.sh                  # 读取 .deploy.local
REMOTE=user@host ./deploy.sh # 或临时用环境变量覆盖
```

## 开发

```bash
cargo build                    # 构建
cargo test                     # 全部测试(内存 SQLite,无需真实数据库)
cargo test lookup_classifies_days   # 单个测试
cargo fmt && cargo clippy --all-targets
```

约定与架构说明见 [CLAUDE.md](CLAUDE.md)。
