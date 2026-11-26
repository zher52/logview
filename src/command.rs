// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 命令源:预设模板、占位符展开、本机 argv 拆分、远端命令引用
//!
//! 预设是「数据」:一条 argv 模板 + 若干字段。展开后交给 `source::open_command`
//! 执行;需要先取值的占位符(如 adb 的 PID)用 `resolve` 描述,每次打开/重连都重新求值,
//! 因此「最近打开」里存的是预设 id + 字段值,而不是当时解析出的 PID。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::i18n;

/// 预设里的一个字段(渲染成一行输入框)
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct PresetField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub placeholder: String,
    /// 可留空(如 adb 的设备序列号:留空 = 默认设备)
    #[serde(default)]
    pub optional: bool,
}

/// 取值命令:执行 `argv`,取 stdout 第一个词替换 `${key}`
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct ResolveSpec {
    pub key: String,
    pub argv: Vec<String>,
}

/// 一条命令预设
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct CommandPreset {
    pub id: String,
    pub name: String,
    /// 是否可用于本机命令
    #[serde(default = "yes")]
    pub local: bool,
    /// 是否可用于 SSH 命令
    #[serde(default = "yes")]
    pub ssh: bool,
    /// argv 模板:本机按项传参;SSH 时每项按 shell 规则引用后拼接成一条命令
    pub argv: Vec<String>,
    #[serde(default)]
    pub fields: Vec<PresetField>,
    /// 打开前需要先执行的取值命令(仅本机)
    #[serde(default)]
    pub resolve: Vec<ResolveSpec>,
    /// 打开后自动启用的级别配置 id(如 Android logcat 格式)
    #[serde(default)]
    pub level_config_id: Option<String>,
    #[serde(default)]
    pub builtin: bool,
}

fn yes() -> bool {
    true
}

/// 一次打开所需的执行计划:已展开的 argv + 待求值的占位符
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommandPlan {
    pub argv: Vec<String>,
    pub resolve: Vec<ResolveSpec>,
    /// 自定义命令的原文(SSH 时不加引号,允许管道/重定向)
    pub raw: bool,
}

/// 内置预设:本机 adb / tail,SSH journalctl / docker / kubectl / tail
pub fn builtin_presets() -> Vec<CommandPreset> {
    let mut out = Vec::with_capacity(8);

    // —— Android logcat ——
    out.push(CommandPreset {
        id: "adb-logcat-all".into(),
        name: "logcat 全部".into(),
        local: true,
        ssh: false,
        argv: vec![
            "adb".into(),
            "-s".into(),
            "${serial}".into(),
            "logcat".into(),
            "-v".into(),
            "threadtime".into(),
        ],
        fields: vec![PresetField {
            key: "serial".into(),
            label: "设备".into(),
            default: String::new(),
            placeholder: "留空 = 默认设备(多设备时需填写)".into(),
            optional: true,
        }],
        resolve: Vec::new(),
        level_config_id: Some("builtin-android-logcat".into()),
        builtin: true,
    });
    out.push(CommandPreset {
        id: "adb-logcat-package".into(),
        name: "logcat 包名".into(),
        local: true,
        ssh: false,
        argv: vec![
            "adb".into(),
            "-s".into(),
            "${serial}".into(),
            "logcat".into(),
            "-v".into(),
            "threadtime".into(),
            "--pid".into(),
            "${pid}".into(),
        ],
        fields: vec![
            PresetField {
                key: "package".into(),
                label: "包名".into(),
                default: String::new(),
                placeholder: "例如 com.example.app".into(),
                optional: false,
            },
            PresetField {
                key: "serial".into(),
                label: "设备".into(),
                default: String::new(),
                placeholder: "留空 = 默认设备".into(),
                optional: true,
            },
        ],
        resolve: vec![ResolveSpec {
            key: "pid".into(),
            argv: vec![
                "adb".into(),
                "-s".into(),
                "${serial}".into(),
                "shell".into(),
                "pidof".into(),
                "-s".into(),
                "${package}".into(),
            ],
        }],
        level_config_id: Some("builtin-android-logcat".into()),
        builtin: true,
    });
    out.push(CommandPreset {
        id: "adb-logcat-tag".into(),
        name: "logcat TAG".into(),
        local: true,
        ssh: false,
        argv: vec![
            "adb".into(),
            "-s".into(),
            "${serial}".into(),
            "logcat".into(),
            "-v".into(),
            "threadtime".into(),
            "-s".into(),
            "${tag}".into(),
        ],
        fields: vec![
            PresetField {
                key: "tag".into(),
                label: "TAG".into(),
                default: String::new(),
                placeholder: "例如 ActivityManager".into(),
                optional: false,
            },
            PresetField {
                key: "serial".into(),
                label: "设备".into(),
                default: String::new(),
                placeholder: "留空 = 默认设备".into(),
                optional: true,
            },
        ],
        resolve: Vec::new(),
        level_config_id: Some("builtin-android-logcat".into()),
        builtin: true,
    });

    // —— 通用:跟随文件(本机与远端都可用,比重连读取更贴近 tail 语义)——
    out.push(CommandPreset {
        id: "cmd-tail-f".into(),
        name: "tail -F".into(),
        local: true,
        ssh: true,
        argv: vec!["tail".into(), "-F".into(), "${path}".into()],
        fields: vec![PresetField {
            key: "path".into(),
            label: "路径".into(),
            default: String::new(),
            placeholder: "/var/log/app.log".into(),
            optional: false,
        }],
        resolve: Vec::new(),
        level_config_id: None,
        builtin: true,
    });

    // —— 服务端常见来源 ——
    out.push(CommandPreset {
        id: "cmd-journalctl".into(),
        name: "journalctl -f".into(),
        local: false,
        ssh: true,
        argv: vec![
            "journalctl".into(),
            "-u".into(),
            "${unit}".into(),
            "-f".into(),
            "--no-pager".into(),
            "--output=short-iso".into(),
        ],
        fields: vec![PresetField {
            key: "unit".into(),
            label: "服务".into(),
            default: String::new(),
            placeholder: "例如 nginx.service".into(),
            optional: false,
        }],
        resolve: Vec::new(),
        level_config_id: None,
        builtin: true,
    });
    out.push(CommandPreset {
        id: "cmd-docker-logs".into(),
        name: "docker logs".into(),
        local: false,
        ssh: true,
        argv: vec![
            "docker".into(),
            "logs".into(),
            "-f".into(),
            "--tail".into(),
            "200".into(),
            "${container}".into(),
        ],
        fields: vec![PresetField {
            key: "container".into(),
            label: "容器".into(),
            default: String::new(),
            placeholder: "容器名或 ID".into(),
            optional: false,
        }],
        resolve: Vec::new(),
        level_config_id: None,
        builtin: true,
    });
    out.push(CommandPreset {
        id: "cmd-kubectl-logs".into(),
        name: "kubectl logs".into(),
        local: false,
        ssh: true,
        argv: vec![
            "kubectl".into(),
            "logs".into(),
            "-f".into(),
            "--tail".into(),
            "200".into(),
            "${pod}".into(),
        ],
        fields: vec![PresetField {
            key: "pod".into(),
            label: "Pod".into(),
            default: String::new(),
            placeholder: "pod 名称,可带 -c 容器".into(),
            optional: false,
        }],
        resolve: Vec::new(),
        level_config_id: None,
        builtin: true,
    });

    out
}

/// 可用的预设(按目标过滤,保持内置顺序;用户自定义的排在后面)
pub fn presets_for(presets: &[CommandPreset], local: bool) -> Vec<&CommandPreset> {
    presets
        .iter()
        .filter(|p| if local { p.local } else { p.ssh })
        .collect()
}

pub fn find<'a>(presets: &'a [CommandPreset], id: &str) -> Option<&'a CommandPreset> {
    presets.iter().find(|p| p.id == id)
}

/// 展开模板:值为空的可选参数连同紧邻开关一起丢弃(如 ["-s", "${serial}"])
fn expand_argv(template: &[String], values: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in template {
        let s = subst(item, values);
        if s.trim().is_empty() {
            if out.last().is_some_and(|p: &String| p.starts_with('-')) {
                out.pop();
            }
            continue;
        }
        out.push(s);
    }
    out
}

/// 展开字段并生成执行计划
pub fn plan(
    preset: &CommandPreset,
    values: &BTreeMap<String, String>,
) -> Result<CommandPlan, String> {
    for f in &preset.fields {
        let v = values.get(&f.key).map(|s| s.trim()).unwrap_or("");
        if v.is_empty() && !f.optional {
            return Err(i18n::tf(
                "validation.field_required",
                &[("label", i18n::td(&f.label))],
            ));
        }
    }
    let argv = expand_argv(&preset.argv, values);
    if argv.is_empty() {
        return Err(i18n::t("error.command.expanded_empty").into());
    }
    let resolve = preset
        .resolve
        .iter()
        .map(|r| ResolveSpec {
            key: r.key.clone(),
            argv: expand_argv(&r.argv, values),
        })
        .collect();
    Ok(CommandPlan {
        argv,
        resolve,
        raw: false,
    })
}

/// 把模板里的 `${key}` 替换为字段值(未知占位符原样保留,交给 resolve)
fn subst(template: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        match tail.find('}') {
            Some(end) => {
                let key = &tail[..end];
                match values.get(key) {
                    Some(v) => out.push_str(v.trim()),
                    None => {
                        out.push_str("${");
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 拆分本机命令行:支持单/双引号与反斜杠转义,不展开变量与通配符
pub fn split_argv(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    // 双引号内允许 \" 与 \\,其余反斜杠按字面保留
                    match chars.peek() {
                        Some('"') | Some('\\') => {
                            cur.push(chars.next().unwrap());
                        }
                        _ => cur.push('\\'),
                    }
                } else {
                    cur.push(c);
                }
                has = true;
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                        has = true;
                    }
                }
                c if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if let Some(q) = quote {
        return Err(i18n::tf(
            "error.command.unclosed_quote",
            &[("q", q.to_string())],
        ));
    }
    if has {
        out.push(cur);
    }
    Ok(out)
}

/// 单引号引用(远端命令用):内部单引号按 POSIX 规则拼接
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,^~".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 展示用命令行:未求值的占位符显示为 …
pub fn display(argv: &[String]) -> String {
    let parts: Vec<String> = argv
        .iter()
        .map(|a| {
            if a.contains("${") {
                let mut s = a.clone();
                while let Some(b) = s.find("${") {
                    let Some(e) = s[b..].find('}') else { break };
                    s.replace_range(b..b + e + 1, "…");
                }
                s
            } else {
                a.clone()
            }
        })
        .collect();
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn adb_all_omits_device_flag_when_empty() {
        let presets = builtin_presets();
        let p = find(&presets, "adb-logcat-all").unwrap();
        let pl = plan(p, &values(&[("serial", "")])).unwrap();
        assert_eq!(
            pl.argv,
            vec!["adb", "logcat", "-v", "threadtime"],
            "{:?}",
            pl.argv
        );
        let pl = plan(p, &values(&[("serial", "emulator-5554")])).unwrap();
        assert_eq!(
            pl.argv,
            vec!["adb", "-s", "emulator-5554", "logcat", "-v", "threadtime"]
        );
    }

    #[test]
    fn adb_package_keeps_pid_placeholder_and_resolve() {
        let presets = builtin_presets();
        let p = find(&presets, "adb-logcat-package").unwrap();
        let vals = values(&[("package", "com.example.app"), ("serial", "")]);
        let pl = plan(p, &vals).unwrap();
        assert!(pl.argv.contains(&"${pid}".to_string()));
        assert_eq!(pl.resolve.len(), 1);
        assert_eq!(pl.resolve[0].key, "pid");
        assert_eq!(
            pl.resolve[0].argv,
            vec!["adb", "shell", "pidof", "-s", "com.example.app"]
        );
    }

    #[test]
    fn required_field_is_reported() {
        let presets = builtin_presets();
        let p = find(&presets, "adb-logcat-package").unwrap();
        let err = plan(p, &values(&[("package", "  ")])).unwrap_err();
        assert!(err.contains("包名"), "{err}");
    }

    #[test]
    fn split_argv_handles_quotes_and_escapes() {
        assert_eq!(
            split_argv("adb logcat -s 'My Tag'").unwrap(),
            vec!["adb", "logcat", "-s", "My Tag"]
        );
        assert_eq!(
            split_argv(r#"tail -F "/var/log/my app.log""#).unwrap(),
            vec!["tail", "-F", "/var/log/my app.log"]
        );
        assert_eq!(split_argv("a\\ b c").unwrap(), vec!["a b", "c"]);
        assert!(split_argv("echo 'unclosed").is_err());
        assert!(split_argv("   ").unwrap().is_empty());
    }

    #[test]
    fn remote_tokens_are_quoted() {
        assert_eq!(shell_quote("nginx.service"), "nginx.service");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn display_hides_unresolved_placeholders() {
        let argv = vec![
            "adb".to_string(),
            "logcat".to_string(),
            "--pid".to_string(),
            "${pid}".to_string(),
        ];
        assert_eq!(display(&argv), "adb logcat --pid …");
    }
}
