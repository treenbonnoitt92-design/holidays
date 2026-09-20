# syntax=docker/dockerfile:1
#
# 多阶段构建：rust 环境里编译，只把二进制搬进极简运行镜像。
#
# 三个设计要点：
#   1. 依赖单独一层缓存 —— 改业务代码不会重编 axum/tokio/rusqlite
#   2. 全流程走国内镜像源（Debian 换清华、crates 换 rsproxy），否则构建会卡在拉包
#   3. 运行镜像不带编译器，只装 tzdata，保证 SQLite 的 localtime 是东八区

# ================================================================ 构建阶段
FROM rust:1-slim-bookworm AS builder

# 换官方源可传 --build-arg CARGO_MIRROR=sparse+https://index.crates.io/
ARG CARGO_MIRROR=sparse+https://rsproxy.cn/index/

ENV CARGO_HOME=/usr/local/cargo \
    CARGO_TERM_COLOR=never

# rusqlite 用的是 bundled 特性，sqlite3 源码要现编，必须有 C 工具链
RUN set -eux; \
    sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
        /etc/apt/sources.list.d/debian.sources 2>/dev/null || true; \
    apt-get update; \
    apt-get install -y --no-install-recommends gcc libc6-dev pkg-config; \
    rm -rf /var/lib/apt/lists/*

RUN mkdir -p "$CARGO_HOME" && { \
        echo '[source.crates-io]'; \
        echo 'replace-with = "mirror"'; \
        echo '[source.mirror]'; \
        echo "registry = \"${CARGO_MIRROR}\""; \
    } > "$CARGO_HOME/config.toml"

WORKDIR /app

# ---- 依赖层：先用占位源码编一遍，之后只改 src/ 时这一层仍命中缓存
COPY Cargo.toml Cargo.lock ./
RUN set -eux; \
    mkdir -p src; \
    echo 'fn main() {}' > src/main.rs; \
    : > src/lib.rs; \
    cargo build --release --locked; \
    rm -rf src

# ---- 业务层
COPY src ./src
RUN set -eux; \
    touch src/main.rs src/lib.rs; \
    cargo build --release --locked; \
    test -x target/release/holidays

# ================================================================ 运行阶段
FROM debian:bookworm-slim AS runtime

ENV TZ=Asia/Shanghai \
    HOLIDAYS_DB=/data/db/holidays.db \
    HOLIDAYS_DATA_DIR=/data/json \
    HOLIDAYS_ADDR=0.0.0.0:8080 \
    RUST_LOG=info

# tzdata：/etc/share/zoneinfo 让 TZ=Asia/Shanghai 生效，SQLite 的
# datetime('now','localtime') 与 `holidays info` 才会显示东八区时间
RUN set -eux; \
    sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
        /etc/apt/sources.list.d/debian.sources 2>/dev/null || true; \
    apt-get update; \
    apt-get install -y --no-install-recommends tzdata ca-certificates; \
    rm -rf /var/lib/apt/lists/*; \
    mkdir -p /data/db /data/json

COPY --from=builder /app/target/release/holidays /usr/local/bin/holidays
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

# 从 Windows 同步过来可能带 CRLF 或丢失执行位，这里统一修正
RUN sed -i 's/\r$//' /usr/local/bin/entrypoint.sh \
    && chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 8080

# 用二进制自带的探针，镜像里不必额外装 curl
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["holidays", "healthcheck", "--addr", "127.0.0.1:8080", "--path", "/health"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
