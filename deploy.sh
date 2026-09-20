#!/usr/bin/env bash
#
# 同步源码到部署目标服务器，在服务器上构建镜像并重启容器。
#
#   ./deploy.sh                        构建 + 部署
#   ./deploy.sh --build-only           只构建镜像，不动线上容器
#   ./deploy.sh --no-cache             丢弃构建缓存重来一遍
#   REMOTE=user@host ./deploy.sh       指定部署目标（也可写进 .deploy.local）
#   VERSION=0.1.1 ./deploy.sh          指定镜像 tag（默认 0.1.0）
#   EXTERNAL_PORT=13480 ./deploy.sh    指定对外端口
#
# 私有参数（远程主机等）放本机 .deploy.local，该文件不进版本库，也不会同步到远端。
#
# 用 tar over ssh 而不是 rsync：Windows 的 Git Bash 不带 rsync，服务器上却有，
# 统一走 tar 就只有一个代码路径。上传先落到 .staging/，传完才切换，
# 中途断线不会把手上的部署搞坏。只动下面列出的子路径，绝不碰远端 db/。
set -euo pipefail

REMOTE_DIR="${REMOTE_DIR:-/data/holidays}"
IMAGE="${IMAGE:-holidays}"
VERSION="${VERSION:-0.1.0}"
EXTERNAL_PORT="${EXTERNAL_PORT:-13480}"

# 这些是部署产物，远端做镜像同步（本地删掉的文件远端也会删）
MIRRORED=(src docker data)
# 这些是单文件，覆盖即可
PLAIN=(Dockerfile .dockerignore docker-compose.yml Cargo.toml Cargo.lock)

BUILD_ONLY=0
NO_CACHE=""
for arg in "$@"; do
    case "$arg" in
        --build-only) BUILD_ONLY=1 ;;
        --no-cache) NO_CACHE="--no-cache" ;;
        -h | --help)
            sed -n '3,12p' "$0"
            exit 0
            ;;
        *)
            echo "未知参数：$arg" >&2
            exit 2
            ;;
    esac
done

cd "$(dirname "$0")"

# 私有部署参数只留在本机：.deploy.local 已被 .gitignore 忽略，也不会同步到远端。
#   REMOTE=user@host
#   EXTERNAL_PORT=13480
if [ -f .deploy.local ]; then
    # shellcheck disable=SC1091
    . ./.deploy.local
fi

: "${REMOTE:?未指定部署目标：在 .deploy.local 里写 REMOTE=user@host，或 REMOTE=user@host ./deploy.sh}"

# 兼容用记事本写出的 CRLF 配置
REMOTE="${REMOTE//$'\r'/}"
EXTERNAL_PORT="${EXTERNAL_PORT//$'\r'/}"

for tool in ssh tar gzip; do
    command -v "$tool" >/dev/null || {
        echo "缺少命令：$tool" >&2
        exit 1
    }
done

echo "==> 1/4 同步源码 → $REMOTE:$REMOTE_DIR"
ssh "$REMOTE" "set -e; rm -rf '$REMOTE_DIR/.staging'; mkdir -p '$REMOTE_DIR/db' '$REMOTE_DIR/.staging'"
tar czf - "${PLAIN[@]}" "${MIRRORED[@]}" | ssh "$REMOTE" "tar xzf - -C '$REMOTE_DIR/.staging'"

# 暂存内容到位后再切换，切换过程中不会有半份源码被编译。
# 用 cp -a .staging/. 而不是 mv .staging/*，否则 .dockerignore 这类点文件会被漏掉。
ssh "$REMOTE" "set -e; cd '$REMOTE_DIR'; \
    rm -rf ${MIRRORED[*]}; \
    cp -a .staging/. .; \
    rm -rf .staging; \
    printf 'HOLIDAYS_VERSION=%s\nHOLIDAYS_EXTERNAL_PORT=%s\n' '$VERSION' '$EXTERNAL_PORT' > .env"

echo "    远端现有：$(ssh "$REMOTE" "ls -1 '$REMOTE_DIR/data' | tr '\n' ' '")"

echo "==> 2/4 构建镜像 $IMAGE:$VERSION"
ssh "$REMOTE" "cd '$REMOTE_DIR' && docker build $NO_CACHE -t '$IMAGE:$VERSION' ."

if [ "$BUILD_ONLY" = "1" ]; then
    echo "==> 已按 --build-only 结束，容器未改动"
    exit 0
fi

echo "==> 3/4 重新拉起容器"
ssh "$REMOTE" "cd '$REMOTE_DIR' && docker compose up -d"

echo "==> 4/4 健康检查"
sleep 3
ssh "$REMOTE" "cd '$REMOTE_DIR' && docker compose ps"
# 用容器自带的探针自测，不依赖宿主机有没有 curl
ssh "$REMOTE" "docker exec holidays holidays healthcheck --addr 127.0.0.1:8080 --path /health"

HOST_IP="$(ssh "$REMOTE" "hostname -I | cut -d' ' -f1")"
echo
echo "部署完成：http://$HOST_IP:$EXTERNAL_PORT/api/v1/holiday?date=2026-10-01"
