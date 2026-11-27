// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! tail worker:每标签一个线程,轮询增量读取,推送 Append/Replace/Truncated 消息

use crossbeam_channel::{Receiver, Sender};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::command::CommandPlan;
use crate::source::{self, Opened, SourceParams};

#[derive(Clone)]
pub enum Cmd {
    Stop,
    Pause(bool),
    SeekStart, // 清空并从头全量加载(加载全部)
    SeekEnd,   // 跳到末尾:取尾部 tail_cap 字节
    /// 清屏:命令源同时清空输出缓存(临时文件)
    Clear,
    /// 对未知 SSH 主机密钥的确认结果(TOFU)
    HostKeyDecision {
        accept: bool,
        remember: bool,
    },
}

pub enum Msg {
    /// `at`:该批数据的本地接收时间(Unix 毫秒);历史加载为 None(输出时间不可知)
    Append {
        data: Vec<u8>,
        offset: u64,
        size: u64,
        at: Option<i64>,
    },
    Replace {
        data: Vec<u8>,
        offset: u64,
        size: u64,
    },
    Partial {
        loaded: u64,
        total: u64,
    },
    Complete,
    Truncated {
        size: u64,
    },
    NoFollow,
    Error {
        msg: String,
        fatal: bool,
    },
    /// 命令/远端命令已退出;`retry_in` 为自动重试倒计时(秒)
    Exited {
        code: Option<i32>,
        retry_in: Option<u64>,
    },
    /// 首次连接未知主机:等用户核对指纹后回 `Cmd::HostKeyDecision`
    HostKeyPrompt(source::HostKeyInfo),
}

/// 询问 UI 是否信任未知主机;等待期间仍响应 Stop
fn ask_host_key(
    info: &source::HostKeyInfo,
    cmd_rx: &Receiver<Cmd>,
    tx: &Sender<Msg>,
) -> source::HostKeyDecision {
    if tx.send(Msg::HostKeyPrompt(info.clone())).is_err() {
        return source::HostKeyDecision::Reject;
    }
    loop {
        match cmd_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Cmd::HostKeyDecision { accept, remember }) => {
                return if accept {
                    source::HostKeyDecision::Accept { remember }
                } else {
                    source::HostKeyDecision::Reject
                };
            }
            Ok(Cmd::Stop) => return source::HostKeyDecision::Reject,
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return source::HostKeyDecision::Reject;
            }
        }
    }
}

/// 命令输出:读线程 → 控制循环
enum ReaderMsg {
    Chunk(Vec<u8>),
    Eof,
    Fail(String),
}

const CHUNK: usize = 1 << 20; // 单次读取 1MB
const TICK_BUDGET: u64 = 8 << 20; // 每个轮询周期最多读 8MB
const TAIL_ALIGN: u64 = 64 << 10; // 跳到末尾时行对齐窗口

/// 当前本地时间(Unix 毫秒),用于给跟随读到的行打接收时间戳
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 诊断日志开关(LOGVIEW_DEBUG=1)
pub fn debug_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("LOGVIEW_DEBUG")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

pub fn spawn(
    params: SourceParams,
    plan: Option<CommandPlan>,
    poll_ms: u64,
    initial_cap: u64,
    tail_cap: u64,
) -> (Sender<Cmd>, Receiver<Msg>) {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
    let _ = std::thread::Builder::new()
        .name("tail-worker".into())
        .spawn(move || worker(params, plan, poll_ms, initial_cap, tail_cap, cmd_rx, msg_tx));
    (cmd_tx, msg_rx)
}

fn worker(
    params: SourceParams,
    plan: Option<CommandPlan>,
    poll_ms: u64,
    initial_cap: u64,
    tail_cap: u64,
    cmd_rx: Receiver<Cmd>,
    tx: Sender<Msg>,
) {
    // 命令源是单向流,走独立循环(无 size/seek 概念)
    if matches!(params, SourceParams::Command { .. }) {
        worker_stream(params, plan, tail_cap, cmd_rx, tx);
        return;
    }
    let mut o = match source::open(&params, &mut |info| ask_host_key(info, &cmd_rx, &tx)) {
        Ok(o) => o,
        Err(e) => {
            let _ = tx.send(Msg::Error {
                msg: e,
                fatal: true,
            });
            return;
        }
    };
    let mut supports_range = match source::meta(&mut o) {
        Ok(m) => m.supports_range,
        Err(e) => {
            let _ = tx.send(Msg::Error {
                msg: e,
                fatal: true,
            });
            return;
        }
    };
    if let Opened::Http { .. } = &o {
        supports_range = match source::meta(&mut o) {
            Ok(m) => m.supports_range,
            Err(_) => supports_range,
        };
    }

    let mut paused = false;
    let mut offset: u64 = 0;
    let mut size: u64;
    let mut err_streak = 0u32;

    // ---- 初始加载:头部 initial_cap 字节 ----
    let mut stopped = false;
    loop {
        match cmd_rx.try_recv() {
            Ok(Cmd::Stop) => return,
            Ok(Cmd::Pause(p)) => paused = p,
            _ => {}
        }
        if paused {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        match source::meta(&mut o) {
            Err(e) => {
                err_streak += 1;
                let _ = tx.send(Msg::Error {
                    msg: e.to_string(),
                    fatal: false,
                });
                if err_streak >= 5 {
                    let _ = tx.send(Msg::Error {
                        msg: e.to_string(),
                        fatal: true,
                    });
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            Ok(m) => {
                err_streak = 0;
                size = m.size;
            }
        }
        let cap = initial_cap.min(size);
        while offset < cap {
            match source::read_at(&mut o, offset, CHUNK) {
                Ok((d, tot)) => {
                    if d.is_empty() {
                        break;
                    }
                    size = tot;
                    offset += d.len() as u64;
                    // 启动时的头部预读属于历史内容,时间不可知
                    if tx
                        .send(Msg::Append {
                            data: d,
                            offset,
                            size,
                            at: None,
                        })
                        .is_err()
                    {
                        stopped = true;
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error {
                        msg: e,
                        fatal: false,
                    });
                    break;
                }
            }
        }
        if stopped {
            return;
        }
        if offset < size {
            let _ = tx.send(Msg::Partial {
                loaded: offset,
                total: size,
            });
        }
        break;
    }

    // ---- HTTP 不支持 Range:无法跟随 ----
    if !supports_range {
        let _ = tx.send(Msg::NoFollow);
    }

    // ---- 跟随主循环 ----
    // 命令 100ms 内响应;轮询按 poll_ms 节拍
    let mut last_tick = std::time::Instant::now();
    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Cmd::Stop) => return,
            Ok(Cmd::Pause(p)) => paused = p,
            Ok(Cmd::HostKeyDecision { .. }) => {}
            // 文件源清屏只清视图,不需要动文件
            Ok(Cmd::Clear) => {}
            Ok(Cmd::SeekStart) => {
                offset = 0;
                let _ = tx.send(Msg::Replace {
                    data: Vec::new(),
                    offset: 0,
                    size,
                });
                while offset < size {
                    if let Ok((d, tot)) = source::read_at(&mut o, offset, CHUNK) {
                        if d.is_empty() {
                            break;
                        }
                        size = tot;
                        offset += d.len() as u64;
                        if tx
                            .send(Msg::Append {
                                data: d,
                                offset,
                                size,
                                at: None,
                            })
                            .is_err()
                        {
                            return;
                        }
                    } else {
                        break;
                    }
                }
                let _ = tx.send(Msg::Complete);
                continue;
            }
            Ok(Cmd::SeekEnd) => {
                if debug_on() {
                    eprintln!("[dbg] worker 收到 SeekEnd(size={size})");
                }
                if let Ok(m) = source::meta(&mut o) {
                    size = m.size;
                }
                offset = size.saturating_sub(tail_cap);
                if offset > 0 {
                    // 对齐到下一行首
                    if let Ok((d, _)) = source::read_at(&mut o, offset, TAIL_ALIGN as usize) {
                        if let Some(p) = d.iter().position(|&b| b == b'\n') {
                            offset += p as u64 + 1;
                        }
                    }
                }
                let mut data = Vec::new();
                while offset < size && data.len() < (8 << 20) {
                    match source::read_at(&mut o, offset, CHUNK) {
                        Ok((d, tot)) => {
                            if d.is_empty() {
                                break;
                            }
                            size = tot;
                            offset += d.len() as u64;
                            data.extend(d);
                        }
                        Err(_) => break,
                    }
                }
                if debug_on() {
                    eprintln!(
                        "[dbg] worker 发出 Replace(offset={offset}, size={size}, bytes={})",
                        data.len()
                    );
                }
                if tx.send(Msg::Replace { data, offset, size }).is_err() {
                    return;
                }
                continue;
            }
            Err(_) => {}
        }

        if paused {
            continue;
        }
        if !supports_range {
            continue; // 无跟随能力
        }
        if last_tick.elapsed() < Duration::from_millis(poll_ms.max(200)) {
            continue;
        }
        last_tick = std::time::Instant::now();

        match source::meta(&mut o) {
            Err(e) => {
                err_streak += 1;
                let _ = tx.send(Msg::Error {
                    msg: crate::i18n::tf(
                        "error.command.retry_count",
                        &[("e", e.clone()), ("err_count", err_streak.to_string())],
                    ),
                    fatal: false,
                });
                if err_streak >= 5 {
                    let _ = tx.send(Msg::Error {
                        msg: e,
                        fatal: true,
                    });
                    return;
                }
            }
            Ok(m) => {
                err_streak = 0;
                if m.size < offset {
                    // 截断/轮换:回到 0 重新跟随
                    let _ = tx.send(Msg::Truncated { size: m.size });
                    offset = 0;
                    size = m.size;
                } else {
                    size = m.size;
                }
                let mut budget = TICK_BUDGET;
                while offset < size && budget > 0 {
                    match source::read_at(&mut o, offset, CHUNK) {
                        Ok((d, tot)) => {
                            if d.is_empty() {
                                break;
                            }
                            size = tot;
                            offset += d.len() as u64;
                            budget = budget.saturating_sub(d.len() as u64);
                            // 跟随读取:这就是该批日志"输出"(写入文件)后我们观测到的时刻
                            let at = Some(now_ms());
                            if tx
                                .send(Msg::Append {
                                    data: d,
                                    offset,
                                    size,
                                    at,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Error {
                                msg: e,
                                fatal: false,
                            });
                            break;
                        }
                    }
                }
            }
        }
    }
}

// ---------- 命令流 worker ----------

/// 退出后重试的退避秒数:1,2,4,8,16,30…
fn backoff(attempt: u32) -> u64 {
    (1u64 << attempt.min(5)).min(30)
}

/// 可被 Stop 打断的等待;返回 false 表示收到 Stop
fn interruptible_sleep(secs: u64, cmd_rx: &Receiver<Cmd>) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        match cmd_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Stop) => return false,
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(_) => return false,
        }
    }
    true
}

/// 回收子进程:最多等 2s,超时后 kill 再等
fn reap(mut c: std::process::Child) -> Option<i32> {
    for _ in 0..100 {
        match c.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    let _ = c.kill();
    c.wait().ok().and_then(|s| s.code())
}

// ---------- 命令输出缓存(spool) ----------

/// 每个命令标签一份的临时缓存:读线程读到的字节先落盘,worker 再从缓存补发。
/// 因此暂停不会丢日志,“加载全部 / 跳到末尾”与文件源使用同一套字节偏移语义。
const SPOOL_MAX: u64 = 128 << 20;
/// 每次控制循环最多向 UI 补发的字节数,保证 Stop/Pause 仍能快速响应
const PUMP_BUDGET: u64 = 4 << 20;

fn spool_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("logview")
        .join("spool")
}

/// 启动时清理崩溃/强杀留下的缓存(正常退出由 `Spool::drop` 删除)
pub fn cleanup_stale_spools() {
    let Ok(rd) = std::fs::read_dir(spool_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(24 * 3600))
        .unwrap_or(std::time::UNIX_EPOCH);
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(p);
        }
    }
}

struct Spool {
    file: File,
    path: PathBuf,
    len: u64,
    max: u64,
}

impl Spool {
    fn create() -> io::Result<Self> {
        Self::create_in(&spool_dir(), SPOOL_MAX)
    }

    fn create_in(dir: &std::path::Path, max: u64) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(dir)?;

        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(
            "cmd-{}-{}-{}.log",
            std::process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        Ok(Self {
            file: opts.open(&path)?,
            path,
            len: 0,
            max,
        })
    }

    fn append(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(self.len))?;
        self.file.write_all(data)?;
        self.len += data.len() as u64;
        Ok(())
    }

    fn read_at(&mut self, off: u64, max: usize) -> io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; max];
        let n = self.file.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.len = 0;
        Ok(())
    }

    /// 超过上限时丢掉前半段,保留尾部并返回被丢弃的字节数(偏移基准随之前移)。
    /// 逐块前移改写,避免一次分配几十 MB。
    fn compact_if_needed(&mut self) -> io::Result<u64> {
        if self.len <= self.max {
            return Ok(0);
        }
        let keep = (self.max / 2).max(1).min(self.len);
        let mut start = self.len - keep;
        // 对齐到下一行首,避免留下半行
        if start > 0 {
            if let Ok(d) = self.read_at(start, TAIL_ALIGN as usize) {
                if let Some(p) = d.iter().position(|&b| b == b'\n') {
                    start += p as u64 + 1;
                }
            }
        }
        let dropped = start;
        if dropped == 0 {
            return Ok(0);
        }
        let mut off = start;
        let mut write = 0u64;
        let mut buf = vec![0u8; CHUNK];
        while off < self.len {
            self.file.seek(SeekFrom::Start(off))?;
            let n = self.file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            off += n as u64;
            self.file.seek(SeekFrom::Start(write))?;
            self.file.write_all(&buf[..n])?;
            write += n as u64;
        }
        self.file.set_len(write)?;
        self.len = write;
        Ok(dropped)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 把缓存里尚未送给 UI 的部分按块补发;返回 false 表示 UI 端已关闭
fn pump(spool: &mut Spool, sent: &mut u64, tx: &Sender<Msg>) -> bool {
    let mut budget = PUMP_BUDGET;
    while *sent < spool.len && budget > 0 {
        let want = (spool.len - *sent).min(CHUNK as u64).min(budget) as usize;
        match spool.read_at(*sent, want) {
            Ok(d) if d.is_empty() => break,
            Ok(d) => {
                budget = budget.saturating_sub(d.len() as u64);
                *sent += d.len() as u64;
                let msg = Msg::Append {
                    data: d,
                    offset: *sent,
                    size: spool.len,
                    at: Some(now_ms()),
                };
                if tx.send(msg).is_err() {
                    return false;
                }
            }
            Err(_) => break,
        }
    }
    true
}

/// 取缓存尾部 tail_cap 字节,并向前对齐到行首
fn read_tail(spool: &mut Spool, tail_cap: u64) -> (Vec<u8>, u64) {
    let len = spool.len;
    let mut start = len.saturating_sub(tail_cap);
    if start > 0 {
        if let Ok(d) = spool.read_at(start, TAIL_ALIGN as usize) {
            if let Some(p) = d.iter().position(|&b| b == b'\n') {
                start += p as u64 + 1;
            }
        }
    }
    // 单次最多送 16MB,避免用户把 tail_mb 设得很大时一次分配过多
    let want = (len - start).min(16 << 20) as usize;
    let data = spool.read_at(start, want).unwrap_or_default();
    (data, start)
}

/// 命令结束后继续服务缓存:加载全部 / 跳出尾 / 清屏仍然可用,直到标签关闭。
/// 返回 false 表示收到 Stop 或 UI 端已关闭。
fn idle_stream(
    spool: &mut Spool,
    sent: &mut u64,
    tail_cap: u64,
    cmd_rx: &Receiver<Cmd>,
    tx: &Sender<Msg>,
) -> bool {
    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Stop) => return false,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return false,
            Ok(Cmd::Pause(_)) | Ok(Cmd::HostKeyDecision { .. }) => {}
            Ok(Cmd::SeekStart) => {
                *sent = 0;
                let size = spool.len;
                let _ = tx.send(Msg::Replace {
                    data: Vec::new(),
                    offset: 0,
                    size,
                });
            }
            Ok(Cmd::SeekEnd) => {
                let (data, start) = read_tail(spool, tail_cap);
                let size = spool.len;
                *sent = size;
                let _ = tx.send(Msg::Replace {
                    data,
                    offset: start,
                    size,
                });
            }
            Ok(Cmd::Clear) => {
                let _ = spool.clear();
                *sent = 0;
                let _ = tx.send(Msg::Replace {
                    data: Vec::new(),
                    offset: 0,
                    size: 0,
                });
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        if !pump(spool, sent, tx) {
            return false;
        }
    }
}

/// 命令源主循环:读线程 → spool → UI。
/// 暂停时继续落盘但不再补发,恢复后从上次位置补发,日志不会丢。
fn worker_stream(
    params: SourceParams,
    plan: Option<CommandPlan>,
    tail_cap: u64,
    cmd_rx: Receiver<Cmd>,
    tx: Sender<Msg>,
) {
    let restart = matches!(&params, SourceParams::Command { restart: true, .. });
    let mut attempt: u32 = 0;
    // 暂停状态跨重连保留:设备重连不应把“暂停”悄悄变回“继续”
    let mut paused = false;
    // 已送给 UI 的缓存偏移(degraded 模式下退化为累计字节数)
    let mut sent: u64 = 0;
    // “加载全部”补发中:期间即使暂停也要把历史加载完
    let mut replay = false;
    let mut spool = match Spool::create() {
        Ok(s) => Some(s),
        Err(e) => {
            let _ = tx.send(Msg::Error {
                msg: crate::i18n::tf("error.command.spool", &[("e", e.to_string())]),
                fatal: false,
            });
            None
        }
    };

    loop {
        let Some(plan) = plan.clone() else {
            let _ = tx.send(Msg::Error {
                msg: "命令源缺少执行计划".into(),
                fatal: true,
            });
            return;
        };
        let started = std::time::Instant::now();
        let mut handle = match source::open_command(&params, &plan, &mut |info| {
            ask_host_key(info, &cmd_rx, &tx)
        }) {
            Ok(Opened::Stream(h)) => h,
            Ok(_) => {
                let _ = tx.send(Msg::Error {
                    msg: "命令源返回了非流式句柄".into(),
                    fatal: true,
                });
                return;
            }
            Err(e) => {
                if restart && attempt < 6 {
                    let secs = backoff(attempt);
                    let _ = tx.send(Msg::Exited {
                        code: None,
                        retry_in: Some(secs),
                    });
                    let _ = tx.send(Msg::Error {
                        msg: e,
                        fatal: false,
                    });
                    attempt += 1;
                    if !interruptible_sleep(secs, &cmd_rx) {
                        return;
                    }
                    continue;
                }
                let _ = tx.send(Msg::Error {
                    msg: e,
                    fatal: true,
                });
                return;
            }
        };

        let exit_slot = handle.exit.clone();
        let stop = handle.stop.clone();
        let mut child = handle.child.take();
        let reader = std::mem::replace(&mut handle.reader, Box::new(std::io::empty()));

        // 读线程:阻塞读由 MergedReader/非阻塞 SSH 读端负责,这里只管攒批转发
        let (rtx, rrx) = crossbeam_channel::bounded::<ReaderMsg>(64);
        {
            let stop = stop.clone();
            let _ = std::thread::Builder::new()
                .name("cmd-read".into())
                .spawn(move || {
                    let mut r = reader;
                    let mut buf = vec![0u8; 64 << 10];
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match r.read(&mut buf) {
                            Ok(0) => {
                                let _ = rtx.send(ReaderMsg::Eof);
                                return;
                            }
                            Ok(n) => {
                                if rtx.send(ReaderMsg::Chunk(buf[..n].to_vec())).is_err() {
                                    return;
                                }
                            }
                            Err(e) if source::is_retryable(&e) => {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                            Err(e) => {
                                let _ = rtx.send(ReaderMsg::Fail(e.to_string()));
                                return;
                            }
                        }
                    }
                });
        }

        let mut eof = false;
        loop {
            crossbeam_channel::select! {
                recv(cmd_rx) -> m => match m {
                    Ok(Cmd::Stop) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        if let Some(c) = child.take() {
                            let _ = reap(c);
                        }
                        return;
                    }
                    Ok(Cmd::Pause(p)) => paused = p,
                    Ok(Cmd::HostKeyDecision { .. }) => {}
                    Ok(Cmd::SeekStart) => {
                        // 从头加载:清空视图后由 pump 按块补发,期间不受暂停影响
                        if let Some(sp) = spool.as_ref() {
                            sent = 0;
                            replay = true;
                            let size = sp.len;
                            let _ = tx.send(Msg::Replace {
                                data: Vec::new(),
                                offset: 0,
                                size,
                            });
                        }
                    }
                    Ok(Cmd::SeekEnd) => {
                        if let Some(sp) = spool.as_mut() {
                            let (data, start) = read_tail(sp, tail_cap);
                            let size = sp.len;
                            sent = size;
                            replay = false;
                            let _ = tx.send(Msg::Replace { data, offset: start, size });
                        }
                    }
                    Ok(Cmd::Clear) => {
                        let mut size = 0;
                        if let Some(sp) = spool.as_mut() {
                            let _ = sp.clear();
                            size = sp.len;
                        }
                        sent = 0;
                        replay = false;
                        let _ = tx.send(Msg::Replace {
                            data: Vec::new(),
                            offset: 0,
                            size,
                        });
                    }
                },
                recv(rrx) -> m => match m {
                    Ok(ReaderMsg::Chunk(d)) => {
                        let mut fallback: Option<Vec<u8>> = None;
                        if let Some(sp) = spool.as_mut() {
                            match sp.append(&d) {
                                Ok(()) => match sp.compact_if_needed() {
                                    Ok(0) => {}
                                    Ok(dropped) => {
                                        if sent >= dropped {
                                            sent -= dropped;
                                        } else {
                                            // UI 还停在已被裁掉的历史上:重置视图,避免行内容与偏移错位
                                            sent = 0;
                                            let size = sp.len;
                                            let _ = tx.send(Msg::Replace {
                                                data: Vec::new(),
                                                offset: 0,
                                                size,
                                            });
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Msg::Error {
                                            msg: crate::i18n::tf(
                                                "error.command.spool_write",
                                                &[("e", e.to_string())],
                                            ),
                                            fatal: false,
                                        });
                                    }
                                },
                                Err(e) => {
                                    let _ = tx.send(Msg::Error {
                                        msg: crate::i18n::tf(
                                            "error.command.spool_write",
                                            &[("e", e.to_string())],
                                        ),
                                        fatal: false,
                                    });
                                    fallback = Some(d);
                                }
                            }
                        } else {
                            fallback = Some(d);
                        }
                        // 缓存不可用:退回直通模式(暂停仍会丢弃,但预览继续)
                        if let Some(d) = fallback {
                            spool = None;
                            if !paused {
                                sent += d.len() as u64;
                                if tx
                                    .send(Msg::Append {
                                        data: d,
                                        offset: sent,
                                        // 退化模式没有缓存长度,用累计字节数充当“已接收”
                                        size: sent,
                                        at: Some(now_ms()),
                                    })
                                    .is_err()
                                {
                                    stop.store(true, Ordering::Relaxed);
                                    if let Some(c) = child.take() {
                                        let _ = reap(c);
                                    }
                                    return;
                                }
                            }
                        }
                    }
                    Ok(ReaderMsg::Eof) => eof = true,
                    Ok(ReaderMsg::Fail(e)) => {
                        let _ = tx.send(Msg::Error { msg: e, fatal: false });
                        eof = true;
                    }
                    Err(_) => eof = true,
                },
                default(Duration::from_millis(20)) => {}
            }

            if eof {
                break;
            }
            if let Some(sp) = spool.as_mut() {
                // 暂停时停止补发;SeekStart 的补发不受暂停影响
                if (!paused || replay) && !pump(sp, &mut sent, &tx) {
                    stop.store(true, Ordering::Relaxed);
                    if let Some(c) = child.take() {
                        let _ = reap(c);
                    }
                    return;
                }
                if replay && sent >= sp.len {
                    replay = false;
                    let _ = tx.send(Msg::Complete);
                }
            }
        }

        // 进程结束后把缓存里剩余内容全部送出(暂停也送出:worker 即将退出,没人再补发)
        if let Some(sp) = spool.as_mut() {
            let _ = pump(sp, &mut sent, &tx);
            if replay {
                replay = false;
                let _ = tx.send(Msg::Complete);
            }
        }

        let code = match child.take() {
            Some(c) => reap(c),
            None => exit_slot.lock().ok().and_then(|v| *v),
        };
        if started.elapsed().as_secs() > 30 {
            attempt = 0;
        }

        if restart && attempt < 6 {
            let secs = backoff(attempt);
            let _ = tx.send(Msg::Exited {
                code,
                retry_in: Some(secs),
            });
            attempt += 1;
            if !interruptible_sleep(secs, &cmd_rx) {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            continue;
        }
        let _ = tx.send(Msg::Exited {
            code,
            retry_in: None,
        });
        // 命令已结束,但缓存还在:继续响应“加载全部 / 跳到末尾 / 清屏”,
        // 直到用户关闭标签(通道断开)或显式 Stop。
        if let Some(sp) = spool.as_mut() {
            let _ = idle_stream(sp, &mut sent, tail_cap, &cmd_rx, &tx);
            return;
        }
        loop {
            match cmd_rx.recv() {
                Ok(Cmd::Stop) | Err(_) => return,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "logview-spool-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        dir
    }

    #[test]
    fn spool_append_read_clear() {
        let dir = temp_dir("basic");
        let mut sp = Spool::create_in(&dir, 1 << 20).unwrap();
        sp.append(b"a\nbb\n").unwrap();
        assert_eq!(sp.len, 5);
        assert_eq!(sp.read_at(2, 2).unwrap(), b"bb");
        // 偏移越界时读到空
        assert!(sp.read_at(99, 8).unwrap().is_empty());
        sp.clear().unwrap();
        assert_eq!(sp.len, 0);
        assert!(sp.read_at(0, 8).unwrap().is_empty());
        let path = sp.path.clone();
        drop(sp);
        assert!(!path.exists(), "缓存文件应在 drop 时删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spool_compaction_keeps_tail_aligned() {
        let dir = temp_dir("compact");
        // 上限 32 字节:反复追加,压缩后必须保留尾部完整行
        let mut sp = Spool::create_in(&dir, 32).unwrap();
        for i in 0..80 {
            sp.append(format!("line-{i:03}\n").as_bytes()).unwrap();
            sp.compact_if_needed().unwrap();
            assert!(sp.len <= 32, "压缩后仍超上限: {}", sp.len);
        }
        let all = sp.read_at(0, sp.len as usize).unwrap();
        let text = String::from_utf8_lossy(&all);
        assert!(text.starts_with("line-"), "尾部对齐失败: {text:?}");
        assert!(text.ends_with('\n'));
        drop(sp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pump_sends_appends_with_offsets() {
        let dir = temp_dir("pump");
        let mut sp = Spool::create_in(&dir, 1 << 20).unwrap();
        sp.append(b"one\ntwo\n").unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut sent = 0;
        assert!(pump(&mut sp, &mut sent, &tx));
        assert_eq!(sent, 8);
        match rx.try_recv().unwrap() {
            Msg::Append {
                data,
                offset,
                size,
                at,
            } => {
                assert_eq!(data, b"one\ntwo\n");
                assert_eq!(offset, 8);
                assert_eq!(size, 8);
                assert!(at.is_some());
            }
            _ => panic!("应收到 Append"),
        }
        assert!(rx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn stream_pause_seek_clear_keeps_history() {
        use crate::source::{ExecTarget, SourceParams};
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'one\\ntwo\\n'; sleep 0.5; printf 'three\\n'; sleep 0.1".to_string(),
        ];
        let params = SourceParams::Command {
            target: ExecTarget::Local,
            argv: argv.clone(),
            preset: None,
            values: Default::default(),
            restart: false,
            raw: true,
        };
        let plan = Some(CommandPlan {
            argv,
            resolve: Vec::new(),
            raw: true,
        });
        let (cmd_tx, msg_rx) = spawn(params, plan, 1000, 1 << 20, 1 << 20);

        // 先暂停:此后的输出只落盘不补发,恢复后必须全部拿回来
        let _ = cmd_tx.send(Cmd::Pause(true));
        std::thread::sleep(Duration::from_millis(700));
        let _ = cmd_tx.send(Cmd::Pause(false));

        let mut text = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !text.contains("three") {
            if let Ok(Msg::Append { data, .. }) = msg_rx.recv_timeout(Duration::from_millis(100)) {
                text.push_str(&String::from_utf8_lossy(&data));
            }
        }
        assert!(text.contains("one"), "暂停期间的输出丢失: {text:?}");
        assert!(text.contains("three"), "恢复后未补发: {text:?}");

        // 命令结束后 worker 仍保留缓存:从头加载可用
        let _ = cmd_tx.send(Cmd::SeekStart);
        let mut replaced = false;
        let mut replayed = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match msg_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Replace { size, .. }) => {
                    replaced = true;
                    assert!(size > 0, "缓存不应为空");
                }
                Ok(Msg::Append { data, .. }) => {
                    replayed.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(_) => {}
                Err(_) => {}
            }
            if replaced && replayed.contains("three") {
                break;
            }
        }
        assert!(replaced, "应收到 Replace");
        assert!(
            replayed.contains("one") && replayed.contains("three"),
            "回放不完整: {replayed:?}"
        );

        // 清屏:缓存截断
        let _ = cmd_tx.send(Cmd::Clear);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut cleared = false;
        while std::time::Instant::now() < deadline {
            if let Ok(Msg::Replace { size, .. }) = msg_rx.recv_timeout(Duration::from_millis(100)) {
                assert_eq!(size, 0);
                cleared = true;
                break;
            }
        }
        assert!(cleared, "清屏应收到空 Replace");
        let _ = cmd_tx.send(Cmd::Stop);
    }
}
