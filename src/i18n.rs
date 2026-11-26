// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 极简 i18n:以中文原文为 key,英文缺失时回退中文。
//!
//! 这样界面代码可以原地 `i18n::t("取消")` 渐进迁移,新增文案不写英文也能正常显示。
//! 切换语言只需 `set` + 重新渲染;持久化字段是 `Settings::language`。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::Zh as u8);

impl Lang {
    pub fn from_code(code: &str) -> Self {
        if code.eq_ignore_ascii_case("en") {
            Lang::En
        } else {
            Lang::Zh
        }
    }

    /// gpui-kit 组件库使用的 locale 标识
    pub fn locale(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en",
        }
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::En as u8 {
        Lang::En
    } else {
        Lang::Zh
    }
}

/// 取当前语言的文案;英文表未收录时回退中文原文
pub fn t(key: &'static str) -> &'static str {
    if lang() == Lang::Zh {
        return key;
    }
    EN.iter()
        .find(|(zh, _)| *zh == key)
        .map(|(_, en)| *en)
        .unwrap_or(key)
}

/// 中文原文 → 英文;逐步补齐,未收录的条目自动回退中文
const EN: &[(&str, &str)] = &[
    ("取消", "Cancel"),
    ("连接", "Connect"),
    ("运行", "Run"),
    ("打开", "Open"),
    ("仅本次信任", "Trust once"),
    ("信任并记住", "Trust and remember"),
    ("SSH 连接", "SSH connection"),
    ("运行命令", "Run command"),
    ("确认主机密钥", "Confirm host key"),
    ("打开 URL", "Open URL"),
    ("＋ 新建配置", "＋ New config"),
    ("导入 JSON", "Import JSON"),
    ("导出 JSON", "Export JSON"),
    ("恢复内置默认", "Restore built-in defaults"),
    ("删除配置", "Delete config"),
    ("恢复默认", "Restore default"),
    ("＋ 添加规则", "＋ Add rule"),
    ("外观与显示", "Appearance"),
    ("日志与性能", "Logging & performance"),
    ("分析配置", "Analysis rules"),
    ("快捷键", "Shortcuts"),
    ("界面语言", "Language"),
    (
        "切换中文 / English(部分文案仍在迁移)",
        "Switch 中文 / English (some text is still being migrated)",
    ),
];
