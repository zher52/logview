//! 日志级别:内置/自定义多套正则配置、别名映射、编译求值引擎

use serde::{Deserialize, Serialize};

pub const TRACE: u8 = 0;
pub const DEBUG: u8 = 1;
pub const INFO: u8 = 2;
pub const WARN: u8 = 3;
pub const ERROR: u8 = 4;
pub const FATAL: u8 = 5;
pub const NONE: u8 = 6;
pub const LEVEL_COUNT: usize = 7;
/// 分隔行标记(轮换提示等),不参与计数与筛选
pub const SEP: u8 = 255;

pub fn level_name(l: u8) -> &'static str {
    match l {
        TRACE => "TRACE",
        DEBUG => "DEBUG",
        INFO => "INFO",
        WARN => "WARN",
        ERROR => "ERROR",
        FATAL => "FATAL",
        _ => "—",
    }
}

pub fn level_names() -> [&'static str; LEVEL_COUNT] {
    ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL", "无级别"]
}

/// 捕获组文本 → 级别(err → ERROR, warning → WARN, 单字母 d/i/w/e …)
fn alias_level(word: &str) -> Option<u8> {
    let w = word.trim().to_ascii_lowercase();
    Some(match w.as_str() {
        "trace" | "verbose" | "trc" | "t" | "v" => TRACE,
        "debug" | "dbg" | "fine" | "finer" | "finest" | "d" => DEBUG,
        "info" | "information" | "notice" | "i" => INFO,
        "warn" | "warning" | "caution" | "w" => WARN,
        "error" | "err" | "severe" | "fail" | "failure" | "failed" | "e" => ERROR,
        "fatal" | "critical" | "crit" | "panic" | "emerg" | "emergency" | "alert" | "f" => FATAL,
        _ => return None,
    })
}

/// 一条规则:auto=true 时取捕获组 1 的文本映射级别,否则用固定 level
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Rule {
    pub auto: bool,
    pub level: u8,
    pub regex: String,
    pub flags: String,
}

/// 一套命名配置(多套并存,按序判定,首中即停)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LevelConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub built_in: bool,
    pub rules: Vec<Rule>,
}

pub fn builtin_configs() -> Vec<LevelConfig> {
    vec![
        LevelConfig {
            id: "builtin-words".into(),
            name: "通用单词".into(),
            enabled: true,
            built_in: true,
            rules: vec![Rule {
                auto: true,
                level: INFO,
                regex: r"\b(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRITICAL|PANIC|SEVERE)\b".into(),
                flags: "i".into(),
            }],
        },
        LevelConfig {
            id: "builtin-logfmt".into(),
            name: "logfmt (level=xxx)".into(),
            enabled: true,
            built_in: true,
            rules: vec![Rule {
                auto: true,
                level: INFO,
                regex: r#"(?:^|[\s"'\[])(?:level|lvl|severity)[=:](\w+)"#.into(),
                flags: "i".into(),
            }],
        },
        LevelConfig {
            id: "builtin-bracket".into(),
            name: "方括号 [LEVEL]".into(),
            enabled: true,
            built_in: true,
            rules: vec![Rule {
                auto: true,
                level: INFO,
                regex: r"\[(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRITICAL|PANIC)\]".into(),
                flags: "i".into(),
            }],
        },
        // 默认关闭:单独的 I 会命中英文单词 "I",按需启用
        LevelConfig {
            id: "builtin-letters".into(),
            name: "单字母级别 (T/D/I/W/E/V/F)".into(),
            enabled: false,
            built_in: true,
            rules: vec![Rule {
                auto: true,
                level: INFO,
                regex: r"\b([TDIWEVF])\b".into(),
                flags: "".into(),
            }],
        },
        // 方括号包裹的单字母(如 [D] xxx),格式精确,默认启用
        LevelConfig {
            id: "builtin-bracket-letters".into(),
            name: "方括号单字母 [D]/[I]/[W]/[E]".into(),
            enabled: true,
            built_in: true,
            rules: vec![Rule {
                auto: true,
                level: INFO,
                regex: r"\[([TDIWEVF])\]".into(),
                flags: "".into(),
            }],
        },
    ]
}

pub struct CompiledRule {
    re: regex::Regex,
    auto: bool,
    level: u8,
}

/// 编译后的规则引擎(只包含 enabled 的配置,顺序 = 配置列表顺序)
#[derive(Default)]
pub struct Engine {
    rules: Vec<CompiledRule>,
}

impl Engine {
    /// 按标签页自己的启用集合编译(顺序仍按配置列表;不在 `enabled` 的跳过)
    pub fn compile_selected(configs: &[LevelConfig], enabled: &[bool]) -> Engine {
        let mut rules = Vec::new();
        for (i, c) in configs.iter().enumerate() {
            if !enabled.get(i).copied().unwrap_or(false) {
                continue;
            }
            for r in &c.rules {
                if r.regex.is_empty() {
                    continue;
                }
                if let Ok(re) = regex::RegexBuilder::new(&r.regex)
                    .case_insensitive(r.flags.contains('i'))
                    .multi_line(true)
                    .size_limit(16 * (1 << 20))
                    .build()
                {
                    rules.push(CompiledRule { re, auto: r.auto, level: r.level });
                }
            }
        }
        Engine { rules }
    }

    /// 按 id 集合编译(标签页记住的是配置 id,配置增删后仍可对齐)
    pub fn compile_ids(configs: &[LevelConfig], ids: &[String]) -> Engine {
        let enabled: Vec<bool> = configs
            .iter()
            .map(|c| ids.iter().any(|id| id == &c.id))
            .collect();
        Self::compile_selected(configs, &enabled)
    }

    /// 单行级别判定
    pub fn eval(&self, line: &str) -> u8 {
        for r in &self.rules {
            if let Some(caps) = r.re.captures(line) {
                return if r.auto {
                    caps.get(1).and_then(|m| alias_level(m.as_str())).unwrap_or(NONE)
                } else {
                    r.level
                };
            }
        }
        NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_words_detect_sample_levels() {
        // 与真实日志(时间戳+级别词在中间)一致:内置「通用单词」配置应逐行判定出级别
        let cfg = builtin_configs();
        let enabled: Vec<bool> = cfg.iter().map(|c| c.enabled).collect();
        let e = Engine::compile_selected(&cfg, &enabled);
        assert_eq!(
            e.eval("2026-11-26T09:00:00.000Z TRACE worker-0 request id=0"),
            TRACE
        );
        assert_eq!(e.eval("2026-11-26T09:01:00.001Z DEBUG worker-1"), DEBUG);
        assert_eq!(e.eval("2026-11-26T09:02:00.002Z INFO worker-2"), INFO);
        assert_eq!(e.eval("2026-11-26T09:03:00.003Z WARN worker-3"), WARN);
        assert_eq!(e.eval("2026-11-26T09:04:00.004Z ERROR worker-4"), ERROR);
        assert_eq!(e.eval("2026-11-26T09:05:00.005Z FATAL worker-5"), FATAL);
        assert_eq!(e.eval("plain text without level"), NONE);
    }
}
