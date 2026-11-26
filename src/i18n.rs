// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 极简 i18n：语义 ID + 编译期语言包。
//!
//! 译文放在 `locales/{zh,en,ru}.toml`，由 `build.rs` 生成 `&'static str` 查表：
//! 调用方只写 ID（`i18n::t("common.cancel")`），改译文不必动代码，运行时也没有解析开销。
//! 回退链：当前语言 → 英文 → 中文（源语言）；ID 不存在时原样返回，等测试暴露。
//!
//! 复数用 [`tn`]：按语言规则拼 `{id}.one` / `{id}.few`，缺失时回退到基础 ID。

use std::sync::atomic::{AtomicU8, Ordering};

mod tables {
    include!(concat!(env!("OUT_DIR"), "/locales.rs"));
}

#[cfg(test)]
pub use tables::IDS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
    Ru,
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::Zh as u8);

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Zh, Lang::En, Lang::Ru];

    pub fn from_code(code: &str) -> Self {
        if code.eq_ignore_ascii_case("en") {
            Lang::En
        } else if code.eq_ignore_ascii_case("ru") {
            Lang::Ru
        } else {
            Lang::Zh
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    /// 语言名称使用各自的文字，便于在任意当前语言下识别
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
            Lang::Ru => "Русский",
        }
    }

    /// gpui-kit 组件库使用的 locale 标识
    pub fn locale(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }
}

fn table(lang: Lang) -> &'static [(&'static str, &'static str)] {
    match lang {
        Lang::Zh => tables::ZH,
        Lang::En => tables::EN,
        Lang::Ru => tables::RU,
    }
}

fn lookup(lang: Lang, id: &str) -> Option<&'static str> {
    let t = table(lang);
    t.binary_search_by(|(k, _)| (*k).cmp(id))
        .ok()
        .map(|i| t[i].1)
}

fn zh(id: &str) -> Option<&'static str> {
    lookup(Lang::Zh, id)
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        x if x == Lang::En as u8 => Lang::En,
        x if x == Lang::Ru as u8 => Lang::Ru,
        _ => Lang::Zh,
    }
}

/// 取当前语言的文案；缺失时依次回退英文、中文，仍缺失则原样返回 ID
pub fn t(id: &'static str) -> &'static str {
    let cur = lang();
    if cur == Lang::Zh {
        return zh(id).unwrap_or(id);
    }
    lookup(cur, id)
        .or_else(|| lookup(Lang::En, id))
        .or_else(|| zh(id))
        .unwrap_or(id)
}

/// 运行时文案（worker 上报的消息、历史数据里的中文名称）翻译。
/// 按中文原文反查 ID；已经是目标语言或查不到时原样返回。
pub fn td(text: &str) -> String {
    if lang() == Lang::Zh {
        return text.to_string();
    }
    let idx = tables::ZH_INDEX;
    match idx.binary_search_by(|(k, _)| (*k).cmp(text)) {
        Ok(i) => t(idx[i].1).to_string(),
        Err(_) => text.to_string(),
    }
}

/// 带 `{name}` 占位符的模板翻译：先查表，再替换占位符
pub fn tf(template: &'static str, args: &[(&str, String)]) -> String {
    let mut out = t(template).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// 复数文案：`{n}` 替换为数量，词形按语言规则选择
/// （`{id}.one` / `{id}.few`，缺失时用基础 ID）。
pub fn tn(n: u64, id: &'static str) -> String {
    tnf(n, id, &[])
}

/// 带其它占位符的复数文案
pub fn tnf(n: u64, id: &'static str, args: &[(&str, String)]) -> String {
    let cur = lang();
    let form = plural_form(cur, n);
    let text = plural_lookup(cur, id, form)
        .or_else(|| {
            if cur == Lang::En {
                None
            } else {
                plural_lookup(Lang::En, id, form)
            }
        })
        .or_else(|| zh(id))
        .unwrap_or(id);
    let mut out = text.replace("{n}", &n.to_string());
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn plural_lookup(lang: Lang, id: &str, form: &str) -> Option<&'static str> {
    lookup(lang, &format!("{id}.{form}")).or_else(|| lookup(lang, id))
}

/// 语言对应的复数词形：中文不区分，英文 one/other，俄语 one/few/other
pub fn plural_form(lang: Lang, n: u64) -> &'static str {
    match lang {
        Lang::Zh => "other",
        Lang::En => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
        Lang::Ru => {
            let m10 = n % 10;
            let m100 = n % 100;
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "other"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(id: &str) -> bool {
        IDS.binary_search(&id).is_ok()
    }

    /// 语言是进程级全局状态，改语言的用例必须串行执行
    fn lang_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn translates_and_falls_back() {
        let _g = lang_lock();
        set(Lang::En);
        assert_eq!(t("common.cancel"), "Cancel");
        assert_eq!(t("toolbar.jump_to_end"), "Jump to end");
        // 未知 ID 原样返回，便于测试发现拼写错误
        assert_eq!(t("no.such.id"), "no.such.id");
        assert_eq!(td("取消"), "Cancel");
        assert_eq!(td("Cancel"), "Cancel");
        assert_eq!(
            tf(
                "command.retry.after_exit",
                &[("code", "1".into()), ("secs", "3".into())]
            ),
            "Command exited (code=1); retrying in 3s"
        );
        set(Lang::Zh);
        assert_eq!(t("common.cancel"), "取消");
        assert_eq!(td("取消"), "取消");
    }

    #[test]
    fn russian_translates_and_plurals() {
        let _g = lang_lock();
        set(Lang::Ru);
        assert_eq!(t("common.cancel"), "Отмена");
        assert_eq!(td("取消"), "Отмена");
        assert_eq!(tn(1, "status.rows"), "1 строка");
        assert_eq!(tn(3, "status.rows"), "3 строки");
        assert_eq!(tn(5, "status.rows"), "5 строк");
        assert_eq!(tn(21, "status.rows"), "21 строка");
        assert_eq!(tn(11, "status.rows"), "11 строк");
        set(Lang::Zh);
        assert_eq!(tn(3, "status.rows"), "3 行");
    }

    #[test]
    fn english_plurals() {
        let _g = lang_lock();
        set(Lang::En);
        assert_eq!(tn(1, "status.rows"), "1 line");
        assert_eq!(tn(2, "status.rows"), "2 lines");
        set(Lang::Zh);
    }

    #[test]
    fn every_plural_id_has_required_forms() {
        // 中文不区分词形；英文需要 one；俄语需要 one 与 few（other 用基础 ID 承担）
        for (lang, forms) in [
            (Lang::Zh, &[][..]),
            (Lang::En, &["one"][..]),
            (Lang::Ru, &["one", "few"][..]),
        ] {
            for id in ["status.rows", "tab.title_rows", "configs.rules_count"] {
                for form in forms {
                    assert!(
                        lookup(lang, &format!("{id}.{form}")).is_some(),
                        "{} 缺少 {id}.{form}",
                        lang.code()
                    );
                }
            }
        }
    }

    #[test]
    fn every_id_has_all_languages() {
        for id in IDS {
            for lang in Lang::ALL {
                assert!(lookup(lang, id).is_some(), "{id} 缺少 {}", lang.code());
            }
        }
    }

    #[test]
    fn every_source_literal_is_a_known_id() {
        // 扫描源码里 t("…") / tf("…") 的首个字面量，防止 ID 拼写错误静默回退
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for entry in walk(&root) {
            // 本文件自身含扫描用的前缀字面量，跳过
            if entry.file_name().is_some_and(|n| n == "i18n.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&entry).unwrap();
            for prefix in ["i18n::t(", "i18n::tf("] {
                let mut rest = text.as_str();
                while let Some(pos) = rest.find(prefix) {
                    rest = &rest[pos + prefix.len()..];
                    let Some(body) = rest.trim_start().strip_prefix('"') else {
                        continue;
                    };
                    let id = &body[..body.find('"').unwrap_or(0)];
                    assert!(has(id), "{}: 未知 ID {id:?}", entry.display());
                    checked += 1;
                }
            }
        }
        assert!(checked > 300, "扫描到的调用点过少: {checked}");
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn td_translates_data_strings() {
        let _g = lang_lock();
        set(Lang::En);
        assert_eq!(td("通用单词"), "Common words");
        assert_eq!(td("logcat 全部"), "logcat (all)");
        set(Lang::Ru);
        assert_eq!(td("通用单词"), "Обычные слова");
        set(Lang::Zh);
        assert_eq!(td("通用单词"), "通用单词");
    }

    #[test]
    fn language_codes_roundtrip() {
        for lang in Lang::ALL {
            assert_eq!(Lang::from_code(lang.code()), lang);
            assert!(!lang.label().is_empty());
            assert!(!lang.locale().is_empty());
        }
        assert_eq!(Lang::from_code("RU"), Lang::Ru);
        assert_eq!(Lang::from_code("fr"), Lang::Zh);
    }
}
