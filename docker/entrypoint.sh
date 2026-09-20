#!/bin/sh
# 容器入口：先把 data 目录的 JSON 导入 SQLite，再拉起 HTTP 服务。
#
# 这样 data/*.json 就是唯一事实来源 —— 换数据只需更新挂载的 JSON 并重启容器，
# 不用手工碰数据库。导入是幂等的（同一天同类别覆盖旧值），反复重启不会产生脏数据。
set -eu

DB="${HOLIDAYS_DB:-/data/db/holidays.db}"
DATA_DIR="${HOLIDAYS_DATA_DIR:-/data/json}"
ADDR="${HOLIDAYS_ADDR:-0.0.0.0:8080}"
AUTO_IMPORT="${HOLIDAYS_AUTO_IMPORT:-1}"

mkdir -p "$(dirname "$DB")"

if [ "$AUTO_IMPORT" = "1" ]; then
    if [ -d "$DATA_DIR" ] && find "$DATA_DIR" -maxdepth 1 -name '*.json' -print -quit | grep -q .; then
        echo "[entrypoint] 导入 $DATA_DIR 的节假日数据 → $DB"
        holidays import --db "$DB" --dir "$DATA_DIR"
    else
        echo "[entrypoint] 警告：$DATA_DIR 下没有 JSON 文件，跳过导入"
    fi
else
    echo "[entrypoint] HOLIDAYS_AUTO_IMPORT=$AUTO_IMPORT，跳过导入"
fi

# 没有数据就直接失败退出，别起一个查谁都是「非节假日」的空服务
if [ ! -f "$DB" ]; then
    echo "[entrypoint] 错误：$DB 不存在，且没有可导入的数据，拒绝启动"
    exit 1
fi

echo "[entrypoint] 启动 API：$ADDR"
exec holidays serve --db "$DB" --addr "$ADDR"
