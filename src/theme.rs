// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! GPUI 主题令牌:antd v5 风格暗/亮双配色 + 字号/行高系统
//!
//! 颜色以 antd v5 设计令牌为准,存 hex 字符串;`palette(light)` 按当前主题取整套。
//! 界面元素一律经 `theme::hex(令牌)` 取色,禁止散落硬编码色值。
//! 字号经 `scaled()/log_scaled()` 换算,行高用 `row_h()`;设置项接入后改这里即可。

use gpui_kit::Hsla;

/// hex(#rrggbb) → Hsla(非法输入返回中性灰,便于容错)
pub fn hex(s: &str) -> Hsla {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 {
        return gpui_kit::hsla(0.0, 0.0, 0.7, 1.0);
    }
    let r = u8::from_str_radix(&t[0..2], 16).unwrap_or(255) as f32 / 255.0;
    let g = u8::from_str_radix(&t[2..4], 16).unwrap_or(255) as f32 / 255.0;
    let b = u8::from_str_radix(&t[4..6], 16).unwrap_or(255) as f32 / 255.0;
    rgba_to_hsla(r, g, b, 1.0)
}

/// hex + alpha(0.0–1.0)→ Hsla
pub fn hexa(s: &str, a: f32) -> Hsla {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 {
        return gpui_kit::hsla(0.0, 0.0, 0.7, a);
    }
    let r = u8::from_str_radix(&t[0..2], 16).unwrap_or(255) as f32 / 255.0;
    let g = u8::from_str_radix(&t[2..4], 16).unwrap_or(255) as f32 / 255.0;
    let b = u8::from_str_radix(&t[4..6], 16).unwrap_or(255) as f32 / 255.0;
    rgba_to_hsla(r, g, b, a.clamp(0.0, 1.0))
}

fn rgba_to_hsla(r: f32, g: f32, b: f32, a: f32) -> Hsla {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return gpui_kit::hsla(0.0, 0.0, l, a);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    gpui_kit::hsla(h / 6.0, s, l, a)
}

/// antd v5 语义令牌(hex 字符串,渲染时再转 Hsla)
pub struct Palette {
    /// 页面底色(日志区背景)
    pub bg_layout: &'static str,
    /// 标签栏/工具栏/状态栏底色
    pub bg_bar: &'static str,
    /// 浮层/弹窗底色
    pub bg_elevated: &'static str,
    /// 输入框底色
    pub bg_input: &'static str,
    /// 选中标签/高亮条目底色
    pub bg_selected: &'static str,
    /// 主文本
    pub text1: &'static str,
    /// 次级文本
    pub text2: &'static str,
    /// 弱化文本(行号/状态栏)
    pub text3: &'static str,
    /// 控件描边
    pub border: &'static str,
    /// 分割线
    pub split: &'static str,
    /// 控件填充(按钮/悬停底)
    pub fill: &'static str,
    /// 控件填充-悬停
    pub fill_hover: &'static str,
    /// 主题色(antd 暗色 #1668dc / 亮色 #1677ff)
    pub primary: &'static str,
    /// 主题色上的文字
    pub on_primary: &'static str,
    /// 成功/警告/危险(antd 暗亮两套)
    pub success: &'static str,
    pub warning: &'static str,
    pub error: &'static str,
    /// 日志行底色:WARN / ERROR / FATAL
    pub row_warn: &'static str,
    pub row_error: &'static str,
    pub row_fatal: &'static str,
    /// 代码视图:关键字 / 字符串 / 数字 / 键名 / 注释
    pub code_kw: &'static str,
    pub code_str: &'static str,
    pub code_num: &'static str,
    pub code_key: &'static str,
    pub code_comment: &'static str,
}

/// antd v5 暗色算法令牌
pub const DARK: Palette = Palette {
    bg_layout: "#141414",
    bg_bar: "#1d1d1d",
    bg_elevated: "#1f1f1f",
    bg_input: "#1d1d1d",
    bg_selected: "#232323",
    text1: "#d9d9d9",
    text2: "#a6a6a6",
    text3: "#737373",
    border: "#303030",
    split: "#1e1e1e",
    fill: "#232323",
    fill_hover: "#2e2e2e",
    primary: "#1668dc",
    on_primary: "#ffffff",
    success: "#95de64",
    warning: "#ffc53d",
    error: "#ff7875",
    row_warn: "#2b2111",
    row_error: "#2c1618",
    row_fatal: "#451d1f",
    code_kw: "#c586c0",
    code_str: "#ce9178",
    code_num: "#b5cea8",
    code_key: "#9cdcfe",
    code_comment: "#6a9955",
};

/// antd v5 亮色算法令牌
pub const LIGHT: Palette = Palette {
    bg_layout: "#f5f5f5",
    bg_bar: "#ffffff",
    bg_elevated: "#ffffff",
    bg_input: "#ffffff",
    bg_selected: "#f0f0f0",
    text1: "#262626",
    text2: "#595959",
    text3: "#8c8c8c",
    border: "#d9d9d9",
    split: "#f0f0f0",
    fill: "#f5f5f5",
    fill_hover: "#e8e8e8",
    primary: "#1677ff",
    on_primary: "#ffffff",
    success: "#52c41a",
    warning: "#d48806",
    error: "#cf1322",
    row_warn: "#fffbe6",
    row_error: "#fff1f0",
    row_fatal: "#ffccc7",
    code_kw: "#af00db",
    code_str: "#a31515",
    code_num: "#098658",
    code_key: "#0451a5",
    code_comment: "#008000",
};

/// 按当前主题取整套令牌
pub fn palette(light: bool) -> &'static Palette {
    if light {
        &LIGHT
    } else {
        &DARK
    }
}

// ---- 正则代码框配色(设置页正则输入框的语法着色) ----

/// 正则语法着色令牌(与日志级别色无关,取编辑器风格配色)
pub struct CodeColors {
    /// 普通字面量(输入框默认前景色)
    pub plain: &'static str,
    /// 字符集转义:\d \w \s \b \p{L}
    pub escape: &'static str,
    /// 转义字面量:\. \n \t \x41 \\
    pub escape_lit: &'static str,
    /// 字符类 [ ... ]
    pub class: &'static str,
    /// 分组括号与 (?…) 前缀
    pub group: &'static str,
    /// 量词 * + ? {n,m}
    pub quant: &'static str,
    /// 分支 |
    pub alt: &'static str,
    /// 锚点 ^ $
    pub anchor: &'static str,
}

/// 暗色(One Dark 系)
const CODE_DARK: CodeColors = CodeColors {
    plain: "#d9d9d9",
    escape: "#e5c07b",
    escape_lit: "#98c379",
    class: "#56b6c2",
    group: "#61afef",
    quant: "#c678dd",
    alt: "#e06c75",
    anchor: "#d19a66",
};

/// 亮色(One Light 系)
const CODE_LIGHT: CodeColors = CodeColors {
    plain: "#262626",
    escape: "#986801",
    escape_lit: "#50a14f",
    class: "#0184bc",
    group: "#4078f2",
    quant: "#a626a4",
    alt: "#e45649",
    anchor: "#b76b01",
};

/// 按当前主题取正则代码框配色
pub fn code_colors(light: bool) -> &'static CodeColors {
    if light {
        &CODE_LIGHT
    } else {
        &CODE_DARK
    }
}

/// 代码框等宽字体族(用户未指定日志字体时按平台取系统自带等宽字体)
pub fn mono_family() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "DejaVu Sans Mono"
    }
}

// ---- 字号/行高系统(运行时由设置驱动,基准 13pt) ----

/// 字号以 f32 位模式存入原子量:渲染与测量都在 UI 线程,读取无锁
static UI_FONT_SIZE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(13.0f32.to_bits());
static LOG_FONT_SIZE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(13.0f32.to_bits());

/// 字号下限/上限(pt),防止输入极端值导致布局不可用
pub const FONT_SIZE_MIN: f32 = 9.0;
pub const FONT_SIZE_MAX: f32 = 24.0;

/// 界面字号(pt)
pub fn ui_font_size() -> f32 {
    f32::from_bits(UI_FONT_SIZE.load(std::sync::atomic::Ordering::Relaxed))
}

/// 日志字号(pt)
pub fn log_font_size() -> f32 {
    f32::from_bits(LOG_FONT_SIZE.load(std::sync::atomic::Ordering::Relaxed))
}

/// 应用设置中的字号;返回是否发生变化(变化时调用方需失效行高缓存并重绘)
pub fn set_font_sizes(ui: f32, log: f32) -> bool {
    let ui = ui.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
    let log = log.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
    let changed = (ui_font_size() - ui).abs() > 0.01 || (log_font_size() - log).abs() > 0.01;
    UI_FONT_SIZE.store(ui.to_bits(), std::sync::atomic::Ordering::Relaxed);
    LOG_FONT_SIZE.store(log.to_bits(), std::sync::atomic::Ordering::Relaxed);
    changed
}

/// UI 元素字号缩放(按基准 13pt 百分比)
pub fn scaled(x: f32) -> f32 {
    x * (ui_font_size() / 13.0)
}

/// 日志区文本字号缩放
pub fn log_scaled(x: f32) -> f32 {
    x * (log_font_size() / 13.0)
}

/// 日志行高:随日志字号等比换算(13pt → 20px)
pub fn row_h() -> f32 {
    (log_font_size() * 20.0 / 13.0).round()
}
