// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

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

/// 级别显示文案的键：前六个是固定的级别缩写，末项使用语言包里的 `level.none`
pub fn level_names() -> [&'static str; LEVEL_COUNT] {
    [
        "TRACE",
        "DEBUG",
        "INFO",
        "WARN",
        "ERROR",
        "FATAL",
        "level.none",
    ]
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
        "fatal" | "critical" | "crit" | "panic" | "emerg" | "emergency" | "alert" | "f" | "a" => {
            FATAL
        }
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
        // Android logcat:默认关闭,只在命令源确实是 logcat(预设或 argv 含 logcat)时
        // 由标签页自动启用;规则按格式精确锚定,可安全排在通用词规则之前。
        //   threadtime: MM-DD HH:MM:SS.mmm  PID  TID L Tag: msg
        //   time:       MM-DD HH:MM:SS.mmm L/Tag( PID): msg
        //   brief:      L/Tag( PID): msg
        //   process:    L( PID) msg
        LevelConfig {
            id: "builtin-android-logcat".into(),
            name: "Android logcat (threadtime / brief / time)".into(),
            enabled: false,
            built_in: true,
            rules: vec![
                Rule {
                    auto: true,
                    level: INFO,
                    regex: r"^\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}\s+\d+\s+\d+\s+([VDIWEFA])\s".into(),
                    flags: "".into(),
                },
                Rule {
                    auto: true,
                    level: INFO,
                    regex: r"^\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}\s+([VDIWEFA])\s".into(),
                    flags: "".into(),
                },
                Rule {
                    auto: true,
                    level: INFO,
                    regex: r"^\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}\s+([VDIWEFA])/".into(),
                    flags: "".into(),
                },
                Rule {
                    auto: true,
                    level: INFO,
                    regex: r"^([VDIWEFA])/[^(]+(?:\(\s*\d+\))?:".into(),
                    flags: "".into(),
                },
                Rule {
                    auto: true,
                    level: INFO,
                    regex: r"^([VDIWEFA])\(\s*\d+\)\s".into(),
                    flags: "".into(),
                },
            ],
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
        let order: Vec<usize> = (0..configs.len()).collect();
        Self::compile_in_order(configs, enabled, &order)
    }

    /// 按 id 集合编译(标签页记住的是配置 id,配置增删后仍可对齐)
    pub fn compile_ids(configs: &[LevelConfig], ids: &[String]) -> Engine {
        Self::compile_ids_preferring(configs, ids, &[])
    }

    /// 按 id 编译,并把 `first` 里的配置排到最前面。
    /// 用于格式类规则(如 logcat):行首级别必须压过正文里的通用级别词,
    /// 否则 `I/xxx: error ...` 会被「通用单词」先判成 ERROR。
    pub fn compile_ids_preferring(
        configs: &[LevelConfig],
        ids: &[String],
        first: &[&str],
    ) -> Engine {
        let enabled: Vec<bool> = configs
            .iter()
            .map(|c| ids.iter().any(|id| id == &c.id))
            .collect();
        let mut order: Vec<usize> = Vec::with_capacity(configs.len());
        for id in first {
            if let Some(i) = configs.iter().position(|c| c.id == *id) {
                if !order.contains(&i) {
                    order.push(i);
                }
            }
        }
        for i in 0..configs.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        Self::compile_in_order(configs, &enabled, &order)
    }

    fn compile_in_order(configs: &[LevelConfig], enabled: &[bool], order: &[usize]) -> Engine {
        let mut rules = Vec::new();
        for &i in order {
            let Some(c) = configs.get(i) else { continue };
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
                    rules.push(CompiledRule {
                        re,
                        auto: r.auto,
                        level: r.level,
                    });
                }
            }
        }
        Engine { rules }
    }

    /// 单行级别判定
    pub fn eval(&self, line: &str) -> u8 {
        for r in &self.rules {
            if let Some(caps) = r.re.captures(line) {
                return if r.auto {
                    caps.get(1)
                        .and_then(|m| alias_level(m.as_str()))
                        .unwrap_or(NONE)
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

    #[test]
    fn android_logcat_is_not_enabled_by_default() {
        // logcat 规则只在命令源确实是 logcat 时自动启用,不再污染普通文件的默认集合
        let cfg = builtin_configs();
        let logcat = cfg
            .iter()
            .find(|c| c.id == "builtin-android-logcat")
            .unwrap();
        assert!(!logcat.enabled);
    }

    #[test]
    fn android_logcat_formats_are_detected() {
        let cfg = builtin_configs();
        let ids = vec!["builtin-android-logcat".to_string()];
        let e = Engine::compile_ids(&cfg, &ids);
        assert_eq!(
            e.eval("11-27 10:00:00.123  1234  5678 I ActivityManager: start"),
            INFO
        );
        assert_eq!(e.eval("11-27 10:00:00.123  1234  5678 E Tag: boom"), ERROR);
        assert_eq!(e.eval("11-27 10:00:00.123 E/Tag( 1234): boom"), ERROR);
        assert_eq!(e.eval("W/Tag( 1234): slow"), WARN);
        assert_eq!(e.eval("D( 1234) msg"), DEBUG);
        assert_eq!(e.eval("A/Tag( 1234): assert"), FATAL);
        assert_eq!(e.eval("--------- beginning of main"), NONE);
    }

    #[test]
    fn logcat_rule_precedes_generic_words() {
        // 行首级别必须胜过正文里的 "error",否则 logcat 的级别会判错
        let cfg = builtin_configs();
        let ids = vec![
            "builtin-words".to_string(),
            "builtin-android-logcat".to_string(),
        ];
        let line = "11-27 10:00:00.123  1234  5678 I Tag: error while loading";
        assert_eq!(Engine::compile_ids(&cfg, &ids).eval(line), ERROR);
        let e = Engine::compile_ids_preferring(&cfg, &ids, &["builtin-android-logcat"]);
        assert_eq!(e.eval(line), INFO);
    }
}
