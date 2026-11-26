// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! tail worker:每标签一个线程,轮询增量读取,推送 Append/Replace/Truncated 消息

use crossbeam_channel::{Receiver, Sender};
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
        worker_stream(params, plan, cmd_rx, tx);
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

/// 命令源主循环:读线程 + 控制循环,支持暂停丢弃、清屏、退出提示与自动重连
fn worker_stream(
    params: SourceParams,
    plan: Option<CommandPlan>,
    cmd_rx: Receiver<Cmd>,
    tx: Sender<Msg>,
) {
    let restart = matches!(&params, SourceParams::Command { restart: true, .. });
    let mut attempt: u32 = 0;

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

        let mut paused = false;
        let mut received: u64 = 0;
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
                    // 命令没有历史可回放:从头加载不可用,忽略
                    Ok(Cmd::SeekStart) => {}
                    Ok(Cmd::SeekEnd) => {
                        received = 0;
                        let _ = tx.send(Msg::Replace {
                            data: Vec::new(),
                            offset: 0,
                            size: 0,
                        });
                    }
                },
                recv(rrx) -> m => match m {
                    Ok(ReaderMsg::Chunk(d)) => {
                        received += d.len() as u64;
                        // 暂停期间继续读(避免反压卡住子进程),但丢弃内容
                        if !paused
                            && tx
                                .send(Msg::Append {
                                    data: d,
                                    offset: received,
                                    size: 0,
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
                    Ok(ReaderMsg::Eof) => break,
                    Ok(ReaderMsg::Fail(e)) => {
                        let _ = tx.send(Msg::Error { msg: e, fatal: false });
                        break;
                    }
                    Err(_) => break,
                },
                default(Duration::from_millis(200)) => {}
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
        return;
    }
}
