// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 行内 ANSI 解析:剥离 CSI/OSC 控制序列,记录可应用到文本区间的 SGR 样式。
//!
//! 行文本入库前调用 [`parse`],这样检索、复制、链接识别与级别判定都只看纯文本;
//! 解析出的 [`AnsiSpan`] 用干净的字节区间保存,渲染时再按当前主题解析颜色。

const ESC: u8 = 0x1b;

/// ANSI 颜色:0–15 为基础色/亮色,16–255 为 xterm 256 色,`Rgb` 为 24 位真彩色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnsiColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// 一条 SGR 序列累积出的样式。颜色用索引保存,渲染时按暗/亮主题取实际色值。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnsiStyle {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
}

impl AnsiStyle {
    fn is_plain(self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && !self.bold
            && !self.dim
            && !self.italic
            && !self.underline
            && !self.strikethrough
            && !self.inverse
    }
}

/// 纯文本上的样式区间,`start..end` 为 UTF-8 字节偏移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnsiSpan {
    pub start: usize,
    pub end: usize,
    pub style: AnsiStyle,
}

/// 剥离控制序列后的文本与样式区间。
pub struct Parsed {
    pub text: String,
    pub spans: Vec<AnsiSpan>,
}

/// 解析一行中的 ANSI 序列。没有 `ESC` 时返回 `None`,避免无 ANSI 的日志多一次分配。
///
/// 只处理 SGR(`ESC[…m`)的颜色与常见字形;光标移动、清行、OSC 标题/超链接等
/// 控制序列会被剥离但不产生样式。每行从默认样式开始,与日志按行处理的语义一致。
pub fn parse(text: &str) -> Option<Parsed> {
    if !text.as_bytes().contains(&ESC) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut style = AnsiStyle::default();
    let mut seg_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == ESC {
            let (next, sgr) = scan_escape(bytes, i);
            if let Some(params) = sgr {
                push_span(&mut spans, seg_start, out.len(), style);
                apply_sgr(params, &mut style);
                seg_start = out.len();
            }
            i = next.max(i + 1);
            continue;
        }
        let next = text[i..]
            .find('\x1b')
            .map(|offset| i + offset)
            .unwrap_or(bytes.len());
        out.push_str(&text[i..next]);
        i = next;
    }

    push_span(&mut spans, seg_start, out.len(), style);
    Some(Parsed { text: out, spans })
}

fn push_span(spans: &mut Vec<AnsiSpan>, start: usize, end: usize, style: AnsiStyle) {
    if start >= end || style.is_plain() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.end == start && last.style == style {
            last.end = end;
            return;
        }
    }
    spans.push(AnsiSpan { start, end, style });
}

/// 返回控制序列结束后的下标,以及 SGR 参数字节(非 `m` 结尾时为 None)。
fn scan_escape(bytes: &[u8], esc: usize) -> (usize, Option<&[u8]>) {
    let Some(&kind) = bytes.get(esc + 1) else {
        return (bytes.len(), None);
    };
    match kind {
        // CSI: 参数/中间字节之后是 0x40..=0x7e 的终止字节
        b'[' => {
            let start = esc + 2;
            let mut j = start;
            while j < bytes.len() {
                let b = bytes[j];
                if (0x40..=0x7e).contains(&b) {
                    let params = (b == b'm').then_some(&bytes[start..j]);
                    return (j + 1, params);
                }
                j += 1;
            }
            (bytes.len(), None)
        }
        // OSC / DCS / SOS / PM / APC: 跳过整段字符串
        b']' | b'P' | b'X' | b'^' | b'_' => (skip_string(bytes, esc + 2), None),
        // 字符集选择等两字节参数序列
        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => ((esc + 3).min(bytes.len()), None),
        // 其余单字符 ESC 序列
        _ => ((esc + 2).min(bytes.len()), None),
    }
}

/// 跳过以 BEL 或 ST(`ESC \`)结尾的控制字符串。
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < bytes.len() {
        if bytes[j] == 0x07 {
            return j + 1;
        }
        if bytes[j] == ESC && bytes.get(j + 1) == Some(&b'\\') {
            return j + 2;
        }
        j += 1;
    }
    bytes.len()
}

fn apply_sgr(params: &[u8], style: &mut AnsiStyle) {
    let raw = std::str::from_utf8(params).unwrap_or_default();
    let values: Vec<Option<u16>> = raw
        .split(';')
        .map(|part| {
            if part.is_empty() {
                Some(0)
            } else {
                part.parse::<u16>().ok()
            }
        })
        .collect();
    let mut i = 0usize;
    while i < values.len() {
        let code = values[i].unwrap_or(0);
        match code {
            0 => *style = AnsiStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 | 21 => style.underline = true,
            7 => style.inverse = true,
            9 => style.strikethrough = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.inverse = false,
            29 => style.strikethrough = false,
            30..=37 => style.fg = Some(AnsiColor::Indexed((code - 30) as u8)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(AnsiColor::Indexed((code - 40) as u8)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(AnsiColor::Indexed((code - 90 + 8) as u8)),
            100..=107 => style.bg = Some(AnsiColor::Indexed((code - 100 + 8) as u8)),
            38 | 48 | 58 => {
                // 38/48 = 前景/背景色,58 = 下划线色(解析但忽略)
                match values.get(i + 1).copied().flatten() {
                    Some(5) => {
                        if let Some(n) = values.get(i + 2).copied().flatten() {
                            set_extended_color(style, code, AnsiColor::Indexed(n.min(255) as u8));
                        }
                        i += 3;
                        continue;
                    }
                    Some(2) => {
                        let rgb = (
                            values.get(i + 2).copied().flatten(),
                            values.get(i + 3).copied().flatten(),
                            values.get(i + 4).copied().flatten(),
                        );
                        if let (Some(r), Some(g), Some(b)) = rgb {
                            set_extended_color(
                                style,
                                code,
                                AnsiColor::Rgb(
                                    r.min(255) as u8,
                                    g.min(255) as u8,
                                    b.min(255) as u8,
                                ),
                            );
                        }
                        i += 5;
                        continue;
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn set_extended_color(style: &mut AnsiStyle, code: u16, color: AnsiColor) {
    match code {
        38 => style.fg = Some(color),
        48 => style.bg = Some(color),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> (String, Vec<AnsiSpan>) {
        let parsed = parse(text).expect("应当识别到 ANSI 序列");
        (parsed.text, parsed.spans)
    }

    #[test]
    fn strips_basic_colors_and_resets() {
        let (text, spans) = spans("\x1b[31mred\x1b[0m plain");
        assert_eq!(text, "red plain");
        assert_eq!(
            spans,
            vec![AnsiSpan {
                start: 0,
                end: 3,
                style: AnsiStyle {
                    fg: Some(AnsiColor::Indexed(1)),
                    ..Default::default()
                },
            }]
        );
    }

    #[test]
    fn parses_256_and_truecolor() {
        let (text, spans) = spans("\x1b[38;5;196mA\x1b[48;2;1;2;3mB");
        assert_eq!(text, "AB");
        let first = AnsiStyle {
            fg: Some(AnsiColor::Indexed(196)),
            ..Default::default()
        };
        let second = AnsiStyle {
            fg: Some(AnsiColor::Indexed(196)),
            bg: Some(AnsiColor::Rgb(1, 2, 3)),
            ..Default::default()
        };
        assert_eq!(
            spans[0],
            AnsiSpan {
                start: 0,
                end: 1,
                style: first,
            }
        );
        assert_eq!(
            spans[1],
            AnsiSpan {
                start: 1,
                end: 2,
                style: second,
            }
        );
    }

    #[test]
    fn parses_styles_and_resets() {
        let (text, spans) = spans("\x1b[1;2;3;4;7;9;31mX\x1b[22;23;24;27;29;39mY");
        assert_eq!(text, "XY");
        let style = AnsiStyle {
            fg: Some(AnsiColor::Indexed(1)),
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            strikethrough: true,
            inverse: true,
            ..Default::default()
        };
        assert_eq!(
            spans,
            vec![AnsiSpan {
                start: 0,
                end: 1,
                style,
            }]
        );
    }

    #[test]
    fn strips_non_sgr_control_sequences() {
        let (text, spans) = spans("a\x1b[2Kb\x1b]0;title\x07c\x1b(Bd");
        assert_eq!(text, "abcd");
        assert!(spans.is_empty());
    }

    #[test]
    fn keeps_utf8_offsets() {
        let (text, spans) = spans("前\x1b[32m中\x1b[0m后");
        assert_eq!(text, "前中后");
        assert_eq!(
            spans,
            vec![AnsiSpan {
                start: 3,
                end: 6,
                style: AnsiStyle {
                    fg: Some(AnsiColor::Indexed(2)),
                    ..Default::default()
                },
            }]
        );
    }

    #[test]
    fn ignores_malformed_sequences() {
        let (text, spans) = spans("ok\x1b[38;5m!");
        assert_eq!(text, "ok!");
        assert!(spans.is_empty());
        assert!(parse("plain log line").is_none());
    }
}
