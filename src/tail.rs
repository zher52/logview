//! tail worker:每标签一个线程,轮询增量读取,推送 Append/Replace/Truncated 消息

use crossbeam_channel::{Receiver, Sender};
use std::time::Duration;

use crate::source::{self, Opened, SourceParams};

#[derive(Clone)]
pub enum Cmd {
    Stop,
    Pause(bool),
    SeekStart, // 清空并从头全量加载(加载全部)
    SeekEnd,   // 跳到末尾:取尾部 tail_cap 字节
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
    NoFollow {
        size: u64,
    },
    Error {
        msg: String,
        fatal: bool,
    },
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
    poll_ms: u64,
    initial_cap: u64,
    tail_cap: u64,
) -> (Sender<Cmd>, Receiver<Msg>) {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
    let _ = std::thread::Builder::new()
        .name("tail-worker".into())
        .spawn(move || worker(params, poll_ms, initial_cap, tail_cap, cmd_rx, msg_tx));
    (cmd_tx, msg_rx)
}

fn worker(
    params: SourceParams,
    poll_ms: u64,
    initial_cap: u64,
    tail_cap: u64,
    cmd_rx: Receiver<Cmd>,
    tx: Sender<Msg>,
) {
    let mut o = match source::open(&params) {
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
    let mut size: u64 = 0;
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
                    msg: format!("{e}"),
                    fatal: false,
                });
                if err_streak >= 5 {
                    let _ = tx.send(Msg::Error {
                        msg: format!("{e}"),
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
        let _ = tx.send(Msg::NoFollow { size });
    }

    // ---- 跟随主循环 ----
    // 命令 100ms 内响应;轮询按 poll_ms 节拍
    let mut last_tick = std::time::Instant::now();
    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Cmd::Stop) => return,
            Ok(Cmd::Pause(p)) => paused = p,
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
                    msg: format!("{e}(重试 {err_streak}/5)"),
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
