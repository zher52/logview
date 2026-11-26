// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 行存储:字节流 → 行(UTF-8 断字安全)、级别标注、级别计数、行数上限

use crate::level::{Engine, SEP};

#[derive(Clone)]
pub struct Line {
    pub text: String,
    pub level: u8,
    /// 该行的本地接收时间(Unix 毫秒);历史加载的行无法得知输出时间,为 None
    pub at: Option<i64>,
}

pub struct LineStore {
    pub lines: Vec<Line>,
    pub counts: [u64; 7],
    pub max_lines: usize,
    pub bytes_loaded: u64,
    pub total_size: u64,
    pub dropped: u64,
    buf: Vec<u8>,
}

impl LineStore {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: Vec::new(),
            counts: [0; 7],
            max_lines,
            bytes_loaded: 0,
            total_size: 0,
            dropped: 0,
            buf: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.counts = [0; 7];
        self.buf.clear();
        self.bytes_loaded = 0;
        self.dropped = 0;
    }

    /// 追加字节流。按 \n 分行(UTF-8 续字节 ≥0x80,不会与 0x0A 冲突,天然断字安全)
    ///
    /// `at`:这一批行的接收时间(Unix 毫秒)。跟随读取时由 worker 逐块打点;
    /// 历史加载(启动时头部预读、加载全部、跳到末尾)传 None —— 那些行的时间不可知,
    /// 显示成"当前时间"会误导。
    pub fn push_bytes(&mut self, data: &[u8], engine: &Engine, at: Option<i64>) -> usize {
        if data.is_empty() {
            return 0;
        }
        self.buf.extend_from_slice(data);
        let mut start = 0usize;
        let mut added = 0usize;
        for (i, &b) in self.buf.iter().enumerate() {
            if b == b'\n' {
                let mut text = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
                if text.ends_with('\r') {
                    text.pop();
                }
                let level = engine.eval(&text);
                self.lines.push(Line { text, level, at });
                self.counts[level as usize] += 1;
                added += 1;
                start = i + 1;
            }
        }
        self.buf.drain(..start);
        self.bytes_loaded += data.len() as u64;
        self.enforce_cap();
        added
    }

    /// 整体替换(跳到末尾 / 加载全部起点)
    pub fn reset_with(&mut self, data: &[u8], offset: u64, engine: &Engine) {
        self.clear();
        self.push_bytes(data, engine, None);
        self.bytes_loaded = offset;
    }

    fn enforce_cap(&mut self) {
        if self.lines.len() > self.max_lines {
            let excess = self.lines.len() - self.max_lines;
            for l in self.lines.drain(..excess) {
                if (l.level as usize) < 7 {
                    self.counts[l.level as usize] = self.counts[l.level as usize].saturating_sub(1);
                }
                self.dropped += 1;
            }
        }
    }

    pub fn set_total(&mut self, size: u64) {
        self.total_size = size;
    }

    /// 插入分隔行(轮换提示),level = SEP,不参与计数
    pub fn insert_separator(&mut self, text: &str) {
        self.lines.push(Line {
            text: text.to_owned(),
            level: SEP,
            at: None,
        });
        self.enforce_cap();
    }

    /// 全量重新判定级别(配置变更时,分帧调用由 App 驱动)
    pub fn relevel_range(&mut self, engine: &Engine, start: usize, end: usize) -> usize {
        let end = end.min(self.lines.len());
        for l in &mut self.lines[start..end] {
            if l.level == SEP {
                continue;
            }
            let nl = engine.eval(&l.text);
            if nl != l.level {
                self.counts[l.level as usize] = self.counts[l.level as usize].saturating_sub(1);
                self.counts[nl as usize] += 1;
                l.level = nl;
            }
        }
        end
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

/// 本地时区偏移(秒)。每帧取一次即可,避免逐行查询时区(DST 切换也能跟上)
pub fn local_offset_secs() -> i32 {
    use chrono::Offset as _;
    chrono::Local::now().offset().fix().local_minus_utc()
}

/// 把接收时间格式化为本地 `HH:MM:SS.mmm`;offset_secs 为 `local_offset_secs()` 的结果
pub fn fmt_local_time(ms: i64, offset_secs: i32) -> String {
    match chrono::DateTime::from_timestamp_millis(ms + offset_secs as i64 * 1000) {
        Some(dt) => dt.format("%H:%M:%S%.3f").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_only_the_batch_it_belongs_to() {
        // 跟随读到的行带上接收时间;历史加载(at=None)的行不编造时间
        let eng = crate::code::no_level_engine();
        let mut st = LineStore::new(100);
        st.push_bytes(b"a\nb\n", &eng, Some(1_700_000_000_000));
        st.push_bytes(b"c\n", &eng, None);
        assert_eq!(st.lines[0].at, Some(1_700_000_000_000));
        assert_eq!(st.lines[1].at, Some(1_700_000_000_000));
        assert_eq!(st.lines[2].at, None);
        // reset_with / 分隔行一律不带时间
        st.reset_with(b"d\n", 0, &eng);
        st.insert_separator(crate::i18n::t("── 轮换 ──"));
        assert_eq!(st.lines[0].at, None);
        assert_eq!(st.lines[1].at, None);
    }

    #[test]
    fn formats_local_time_with_millis() {
        let cst = 8 * 3600; // UTC+8
        assert_eq!(fmt_local_time(0, cst), "08:00:00.000");
        assert_eq!(fmt_local_time(1_700_000_000_123, cst), "06:13:20.123");
        assert_eq!(fmt_local_time(0, 0), "00:00:00.000");
    }
}
