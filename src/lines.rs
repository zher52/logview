// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 行存储:字节流 → 行(UTF-8 断字安全)、级别标注、级别计数、行数上限

use crate::ansi::{self, AnsiSpan};
use crate::level::{Engine, NONE, SEP};

#[derive(Clone)]
pub struct Line {
    /// 已剥离 ANSI 控制序列的纯文本
    pub text: String,
    /// 行内 ANSI 样式区间(相对 `text` 的字节偏移);无样式时为空
    pub ansi: Box<[AnsiSpan]>,
    pub level: u8,
    /// 「无级别跟随上一条已知级别」的有效级别:无级别行取前一条已知级别,
    /// 分隔行(SEP)与文件开头的无级别行保持 NONE。展示/筛选/计数按此项取值。
    pub inherit: u8,
    /// 该行的本地接收时间(Unix 毫秒);历史加载的行无法得知输出时间,为 None
    pub at: Option<i64>,
}

pub struct LineStore {
    pub lines: Vec<Line>,
    /// 按原始级别计数(关闭「跟随上一条已知级别」时使用)
    pub counts: [u64; 7],
    /// 按有效级别计数(开启跟随时使用,无级别行计入前一条已知级别)
    pub follow_counts: [u64; 7],
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
            follow_counts: [0; 7],
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
        self.follow_counts = [0; 7];
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
        // 上一条已知级别:取末行的有效级别(无级别行已带着继承结果,分隔行为 NONE)
        let mut prev = self
            .lines
            .last()
            .map(|l| match l.level {
                SEP => NONE,
                NONE => l.inherit,
                l => l,
            })
            .unwrap_or(NONE);
        let mut start = 0usize;
        let mut added = 0usize;
        for (i, &b) in self.buf.iter().enumerate() {
            if b == b'\n' {
                let mut text = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
                if text.ends_with('\r') {
                    text.pop();
                }
                let (text, ansi) = match ansi::parse(&text) {
                    Some(parsed) => (parsed.text, parsed.spans.into_boxed_slice()),
                    None => (text, Box::default()),
                };
                let level = engine.eval(&text);
                let inherit = if level == SEP {
                    prev = NONE;
                    SEP
                } else if level == NONE {
                    prev
                } else {
                    prev = level;
                    level
                };
                self.lines.push(Line {
                    text,
                    ansi,
                    level,
                    inherit,
                    at,
                });
                self.counts[level as usize] += 1;
                self.follow_counts[inherit as usize] += 1;
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
                if (l.inherit as usize) < 7 {
                    self.follow_counts[l.inherit as usize] =
                        self.follow_counts[l.inherit as usize].saturating_sub(1);
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
            ansi: Box::default(),
            level: SEP,
            inherit: SEP,
            at: None,
        });
        self.enforce_cap();
    }

    /// 全量重新判定级别(配置变更时,分帧调用由 App 驱动)
    pub fn relevel_range(&mut self, engine: &Engine, start: usize, end: usize) -> usize {
        let end = end.min(self.lines.len());
        // 从区间前一行取已知级别(分隔行之后重新开始,与 push_bytes 一致)
        let mut prev = start
            .checked_sub(1)
            .and_then(|i| self.lines.get(i))
            .map(|l| match l.level {
                SEP => NONE,
                NONE => l.inherit,
                l => l,
            })
            .unwrap_or(NONE);
        for l in &mut self.lines[start..end] {
            if l.level == SEP {
                l.inherit = SEP;
                prev = NONE;
                continue;
            }
            let nl = engine.eval(&l.text);
            if nl != l.level {
                self.counts[l.level as usize] = self.counts[l.level as usize].saturating_sub(1);
                self.counts[nl as usize] += 1;
                l.level = nl;
            }
            let inherit = if nl == NONE {
                prev
            } else {
                prev = nl;
                nl
            };
            if inherit != l.inherit {
                if (l.inherit as usize) < 7 {
                    self.follow_counts[l.inherit as usize] =
                        self.follow_counts[l.inherit as usize].saturating_sub(1);
                }
                if (inherit as usize) < 7 {
                    self.follow_counts[inherit as usize] += 1;
                }
                l.inherit = inherit;
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
        st.insert_separator(crate::i18n::t("banner.rotation_short"));
        assert_eq!(st.lines[0].at, None);
        assert_eq!(st.lines[1].at, None);
    }

    #[test]
    fn unlevelled_lines_inherit_previous_level() {
        // 无级别续行沿用上一条已知级别;分隔行之后重新开始;文件开头没有可跟随的级别
        let cfg = crate::level::builtin_configs();
        let eng = crate::level::Engine::compile_ids(&cfg, &["builtin-words".to_string()]);
        let mut st = LineStore::new(100);
        st.push_bytes(
            b"ERROR boom\n  at a\n  at b\nINFO ok\n  detail\n",
            &eng,
            None,
        );
        st.insert_separator("rotated");
        st.push_bytes(b"  orphan\n", &eng, None);

        let inh: Vec<u8> = st.lines.iter().map(|l| l.inherit).collect();
        assert_eq!(
            inh,
            vec![
                crate::level::ERROR,
                crate::level::ERROR,
                crate::level::ERROR,
                crate::level::INFO,
                crate::level::INFO,
                crate::level::SEP,
                crate::level::NONE,
            ]
        );
        assert_eq!(st.counts[crate::level::NONE as usize], 4);
        assert_eq!(st.follow_counts[crate::level::ERROR as usize], 3);
        assert_eq!(st.follow_counts[crate::level::INFO as usize], 2);

        // 重判定后继承链同步重算:只保留 INFO 规则,原 ERROR 行变为无级别
        let only_info = crate::level::LevelConfig {
            id: "test-info".into(),
            name: "test-info".into(),
            enabled: true,
            built_in: false,
            rules: vec![crate::level::Rule {
                auto: true,
                level: crate::level::INFO,
                regex: r"\b(INFO)\b".into(),
                flags: "".into(),
            }],
        };
        let eng2 = crate::level::Engine::compile_ids(&[only_info], &["test-info".to_string()]);
        let n = st.lines.len();
        st.relevel_range(&eng2, 0, n);
        assert_eq!(st.follow_counts[crate::level::ERROR as usize], 0);
        assert_eq!(st.follow_counts[crate::level::INFO as usize], 2);
        assert_eq!(
            st.lines[0].inherit,
            crate::level::NONE,
            "开头没有已知级别可跟随"
        );
    }

    #[test]
    fn ansi_sequences_are_stripped_before_level_detection() {
        let cfg = crate::level::builtin_configs();
        let eng = crate::level::Engine::compile_ids(&cfg, &["builtin-words".to_string()]);
        let mut st = LineStore::new(10);
        st.push_bytes(b"\x1b[31mERROR\x1b[0m boom\n", &eng, None);

        assert_eq!(st.lines[0].text, "ERROR boom");
        assert_eq!(st.lines[0].level, crate::level::ERROR);
        assert_eq!(st.lines[0].ansi.len(), 1);
        assert_eq!(st.lines[0].ansi[0].start, 0);
        assert_eq!(st.lines[0].ansi[0].end, 5);
        assert_eq!(st.counts[crate::level::ERROR as usize], 1);
    }

    #[test]
    fn formats_local_time_with_millis() {
        let cst = 8 * 3600; // UTC+8
        assert_eq!(fmt_local_time(0, cst), "08:00:00.000");
        assert_eq!(fmt_local_time(1_700_000_000_123, cst), "06:13:20.123");
        assert_eq!(fmt_local_time(0, 0), "00:00:00.000");
    }
}
