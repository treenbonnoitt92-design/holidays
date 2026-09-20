//! `holidays` 命令行入口。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use holidays::db::Store;
use holidays::model::normalize_date;
use holidays::{api, importer};
use tracing_subscriber::EnvFilter;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Parser)]
#[command(
    name = "holidays",
    version,
    about = "中国节假日数据服务：JSON 导入 SQLite + REST 查询",
    long_about = "从 data 目录的 JSON 文件导入节假日数据到 SQLite（每年一个文件，重复日期覆盖旧值），\
                  并可通过 REST 接口按日期查询。"
)]
struct Cli {
    /// SQLite 数据库文件路径
    #[arg(
        short,
        long,
        global = true,
        env = "HOLIDAYS_DB",
        default_value = "holidays.db",
        value_name = "FILE"
    )]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 从 data 目录导入节假日数据（重复日期会覆盖旧记录）
    Import {
        /// 数据目录（默认 ./data）
        #[arg(long, default_value = "data", value_name = "DIR")]
        dir: PathBuf,

        /// 只导入指定年份的文件（按文件名匹配），可重复指定
        #[arg(long, value_name = "YYYY")]
        year: Vec<i32>,
    },

    /// 启动 REST API 服务
    Serve {
        /// 监听地址
        #[arg(
            short,
            long,
            env = "HOLIDAYS_ADDR",
            default_value = "127.0.0.1:8080",
            value_name = "ADDR"
        )]
        addr: String,
    },

    /// 查询某个日期（输出 JSON）
    Get {
        /// 日期，格式 YYYY-MM-DD
        date: String,
    },

    /// 列出某一年的全部记录
    List {
        /// 年份，如 2026
        year: i32,

        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },

    /// 查看数据库概况与导入记录
    Info,

    /// 探测 HTTP 服务是否就绪（容器 HEALTHCHECK 用）
    Healthcheck {
        /// 服务地址 host:port
        #[arg(
            short,
            long,
            env = "HOLIDAYS_ADDR",
            default_value = "127.0.0.1:8080",
            value_name = "ADDR"
        )]
        addr: String,

        /// 探测路径
        #[arg(long, default_value = "/health", value_name = "PATH")]
        path: String,

        /// 超时秒数
        #[arg(long, default_value_t = 3, value_name = "SECS")]
        timeout: u64,
    },
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Import { dir, year } => cmd_import(&cli.db, &dir, &year),
        Command::Serve { addr } => cmd_serve(&cli.db, &addr),
        Command::Get { date } => cmd_get(&cli.db, &date),
        Command::List { year, json } => cmd_list(&cli.db, year, json),
        Command::Info => cmd_info(&cli.db),
        Command::Healthcheck {
            addr,
            path,
            timeout,
        } => cmd_healthcheck(&addr, &path, timeout),
    }
}

/// 日志写 stderr，保证 stdout 只放结构化结果，便于管道处理。
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------------------------------------------------------------- import

fn cmd_import(db: &Path, dir: &Path, years: &[i32]) -> Result<()> {
    let store = Store::open(db)?;
    let report = importer::import_dir(&store, dir, years)?;

    const W: [usize; 5] = [12, 8, 8, 8, 14];
    println!(
        "{}{}{}{}{}",
        pad("文件", W[0]),
        padr("新增", W[1]),
        padr("覆盖", W[2]),
        padr("跳过", W[3]),
        padr("覆盖年份", W[4])
    );
    println!("{}", "-".repeat(W.iter().sum()));
    for file in &report.files {
        println!(
            "{}{}{}{}{}",
            pad(&file.file, W[0]),
            padr(file.outcome.inserted.to_string(), W[1]),
            padr(file.outcome.updated.to_string(), W[2]),
            padr(file.invalid.len().to_string(), W[3]),
            padr(file.year_range(), W[4])
        );
        // 跳过明细只展示前几条，避免刷屏
        for reason in file.invalid.iter().take(5) {
            println!("    ↳ 跳过 {reason}");
        }
        if file.invalid.len() > 5 {
            println!("    ↳ 另有 {} 条跳过记录未展示", file.invalid.len() - 5);
        }
    }

    println!("{}", "-".repeat(W.iter().sum()));
    println!(
        "共 {} 个文件，新增 {}，覆盖 {}，跳过 {}",
        report.files.len(),
        report.outcome.inserted,
        report.outcome.updated,
        report.invalid
    );

    let stats = store.stats()?;
    println!();
    println!("数据库 {}", db.display());
    println!(
        "  记录 {} 条 / 日期 {} 个 / 年份 {} 个（{}）",
        stats.total,
        stats.distinct_dates,
        stats.years.len(),
        summarize_years(&stats.years)
    );
    println!(
        "  分类：放假 {}，调休上班 {}，调休补假 {}",
        stats.holiday, stats.workday, stats.in_lieu
    );

    if !report.failures.is_empty() {
        println!();
        for (file, reason) in &report.failures {
            eprintln!("导入失败：{file} —— {reason}");
        }
        anyhow::bail!("{} 个文件导入失败", report.failures.len());
    }

    if report.invalid > 0 {
        eprintln!(
            "注意：有 {} 条记录因日期或值非法被跳过，详见上方明细",
            report.invalid
        );
    }

    Ok(())
}

fn summarize_years(years: &[i32]) -> String {
    let Some((&first, rest)) = years.split_first() else {
        return "-".to_string();
    };
    // 折叠连续年份区间，例如 2004-2026
    let mut parts = Vec::new();
    let (mut start, mut prev) = (first, first);
    for &y in rest {
        if y == prev + 1 {
            prev = y;
            continue;
        }
        parts.push(format_range(start, prev));
        start = y;
        prev = y;
    }
    parts.push(format_range(start, prev));
    parts.join(", ")
}

fn format_range(start: i32, end: i32) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

// ---------------------------------------------------------------- 终端表格对齐
//
// 中文在终端占两列，直接按字符数补空格会让列错位，这里统一按显示宽度算。

fn pad(text: impl AsRef<str>, width: usize) -> String {
    let text = text.as_ref();
    let mut out = String::from(text);
    for _ in UnicodeWidthStr::width(text)..width {
        out.push(' ');
    }
    out
}

fn padr(text: impl AsRef<str>, width: usize) -> String {
    let text = text.as_ref();
    let mut out = String::new();
    for _ in UnicodeWidthStr::width(text)..width {
        out.push(' ');
    }
    out.push_str(text);
    out
}

/// 按显示宽度截断，中英文混排时不会切出半个字。
fn truncate(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max_width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------- serve

fn cmd_serve(db: &Path, addr: &str) -> Result<()> {
    let store = Arc::new(Store::open(db)?);
    let stats = store.stats()?;
    let addr = addr
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("监听地址无效：{addr}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("无法监听 {addr}（端口被占用或权限不足）"))?;
        let local = listener.local_addr()?;

        println!("holidays API 已启动：http://{local}");
        println!(
            "  数据库记录 {} 条，覆盖 {} 个年份",
            stats.total,
            stats.years.len()
        );
        println!("  示例：curl \"http://{local}/api/v1/holiday?date=2026-10-01\"");
        println!("  按 Ctrl+C 退出");

        axum::serve(listener, api::router(store))
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("HTTP 服务异常退出")
    })
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\n收到退出信号，正在关闭服务…");
}

// ---------------------------------------------------------------- get / list / info

fn cmd_get(db: &Path, raw: &str) -> Result<()> {
    let store = Store::open(db)?;
    let (_, day) = normalize_date(raw)?;
    let result = store.query_day(day)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn cmd_list(db: &Path, year: i32, as_json: bool) -> Result<()> {
    let store = Store::open(db)?;
    let records = store.year_records(year)?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }

    if records.is_empty() {
        println!("{year} 年没有数据，可用年份见 `holidays info`");
        return Ok(());
    }

    const W: [usize; 5] = [12, 12, 26, 12, 6];
    println!(
        "{}{}{}{}{}",
        pad("日期", W[0]),
        pad("类别", W[1]),
        pad("英文名", W[2]),
        pad("中文名", W[3]),
        padr("法定", W[4])
    );
    println!("{}", "-".repeat(W.iter().sum()));
    for r in &records {
        println!(
            "{}{}{}{}{}",
            pad(&r.date, W[0]),
            pad(r.category.label_cn(), W[1]),
            pad(truncate(&r.name_en, W[2]), W[2]),
            pad(truncate(&r.name_zh, W[3]), W[3]),
            padr(r.statutory_days.to_string(), W[4])
        );
    }
    println!("{}", "-".repeat(W.iter().sum()));
    println!("共 {} 条记录", records.len());
    Ok(())
}

fn cmd_info(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let stats = store.stats()?;

    println!("数据库 {}", db.display());
    println!(
        "  记录 {} 条 / 日期 {} 个",
        stats.total, stats.distinct_dates
    );
    println!(
        "  年份 {} 个：{}",
        stats.years.len(),
        summarize_years(&stats.years)
    );
    println!(
        "  分类：放假 {}，调休上班 {}，调休补假 {}",
        stats.holiday, stats.workday, stats.in_lieu
    );

    let log = store.import_log()?;
    println!();
    println!("导入记录：");
    if log.is_empty() {
        println!("  （尚未导入，请先执行 `holidays import`）");
    } else {
        const W: [usize; 3] = [14, 22, 8];
        println!(
            "{}{}{}",
            pad("文件", W[0]),
            pad("导入时间", W[1]),
            padr("条数", W[2])
        );
        println!("{}", "-".repeat(W.iter().sum()));
        for (file, at, count) in log {
            println!(
                "{}{}{}",
                pad(&file, W[0]),
                pad(&at, W[1]),
                padr(count.to_string(), W[2])
            );
        }
    }

    println!();
    println!("当前时间：{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    Ok(())
}

// ---------------------------------------------------------------- healthcheck

/// 极简 HTTP 探针：发一个 GET 只看状态行，避免在镜像里塞 curl。
///
/// 健康返回退出码 0，否则非 0 并把原因写到 stderr。
fn cmd_healthcheck(addr: &str, path: &str, timeout_secs: u64) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let target = addr
        .to_socket_addrs()
        .with_context(|| format!("地址无法解析：{addr}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("地址无法解析：{addr}"))?;

    let mut stream =
        TcpStream::connect_timeout(&target, timeout).with_context(|| format!("连接 {target} 失败"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;

    // 只取前 1KB 够看状态行了；用 lossy 转换，避免读一半的多字节字符报错
    let mut buf = Vec::new();
    let mut limited = stream.take(1024);
    limited.read_to_end(&mut buf)?;

    let head = String::from_utf8_lossy(&buf);
    let status = head.lines().next().unwrap_or_default().trim();

    if status.contains(" 200 ") {
        println!("ok: {status}");
        Ok(())
    } else if status.is_empty() {
        anyhow::bail!("探测 {addr}{path} 失败：没有收到任何 HTTP 响应")
    } else {
        anyhow::bail!("探测 {addr}{path} 失败，状态行：{status:?}")
    }
}
