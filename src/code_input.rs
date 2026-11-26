// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 正则代码框:把正则源码切成词法片段,生成带颜色的文本装饰
//!
//! 设置页「匹配规则」的正则输入框用多行输入框(Textarea)承载,布局、光标、
//! 选择、滚动都由输入框自己负责;本模块只回答"哪个字节区间用什么颜色",
//! 以 `TextDecoration` 交给输入框渲染,因此不会干扰编辑行为。

use std::ops::Range;

use gpui_kit::base::input::TextDecoration;
use gpui_kit::HighlightStyle;

use crate::theme;

/// 正则词法类别(决定取哪一档颜色)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RxTok {
    /// 普通字面量(保持默认文本色)
    Literal,
    /// 字符集转义:\d \w \s \b \p{L} 等
    Escape,
    /// 转义字面量:\. \n \t \x41 \\ 等
    EscapeLit,
    /// 字符类 [...] (含方括号与内部内容)
    Class,
    /// 分组括号与 (?…) 前缀
    Group,
    /// 量词:* + ? {n,m} 及其惰性后缀
    Quant,
    /// 分支:|
    Alt,
    /// 锚点:^ $
    Anchor,
    /// 其他标点:.
    Punct,
}

/// 取当前字符在 UTF-8 里的字节长度(越界按 1 处理)
fn char_len(src: &str, i: usize) -> usize {
    src[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

/// 把正则源码切成首尾相接、互不重叠的字节区间
pub fn tokenize(src: &str) -> Vec<(Range<usize>, RxTok)> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out: Vec<(Range<usize>, RxTok)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let start = i;
        let tok = match b[i] {
            // 转义:按 Escaped 字符归类
            b'\\' => {
                i += 1;
                if i >= n {
                    RxTok::EscapeLit
                } else {
                    let e = b[i];
                    i += char_len(src, i);
                    if e == b'x' {
                        // \x41
                        for _ in 0..2 {
                            if i < n && b[i].is_ascii_hexdigit() {
                                i += 1;
                            }
                        }
                    } else if matches!(e, b'p' | b'P' | b'u' | b'N') && i < n && b[i] == b'{' {
                        // \p{Greek} \u{1F600} \N{...}
                        while i < n && b[i] != b'}' {
                            i += 1;
                        }
                        if i < n {
                            i += 1;
                        }
                    }
                    if matches!(
                        e,
                        b'd' | b'D'
                            | b'w'
                            | b'W'
                            | b's'
                            | b'S'
                            | b'b'
                            | b'B'
                            | b'A'
                            | b'z'
                            | b'Z'
                            | b'p'
                            | b'P'
                    ) {
                        RxTok::Escape
                    } else {
                        RxTok::EscapeLit
                    }
                }
            }
            // 字符类:吃到未转义的 ] 为止(首字符 ] 是字面量)
            b'[' => {
                i += 1;
                if i < n && b[i] == b'^' {
                    i += 1;
                }
                if i < n && b[i] == b']' {
                    i += 1;
                }
                while i < n {
                    match b[i] {
                        b'\\' => {
                            i += 1;
                            if i < n {
                                i += char_len(src, i);
                            }
                        }
                        b']' => {
                            i += 1;
                            break;
                        }
                        // POSIX 字符类 [:alpha:]:整体跳过,仍留在字符类内
                        b'[' if i + 1 < n && b[i + 1] == b':' => {
                            while i < n && b[i] != b']' {
                                i += 1;
                            }
                            if i < n {
                                i += 1;
                            }
                        }
                        _ => i += char_len(src, i),
                    }
                }
                i = i.min(n);
                RxTok::Class
            }
            // 分组:( 连同 (?: (?P<name> 等前缀一起着色
            b'(' => {
                i += 1;
                if i < n && b[i] == b'?' {
                    i += 1;
                    if i < n && b[i] == b'P' {
                        i += 1;
                    }
                    if i < n && b[i] == b'<' {
                        i += 1;
                    }
                    while i < n && !matches!(b[i], b'>' | b'=' | b'!' | b':' | b')' | b'#') {
                        if b[i].is_ascii_whitespace() {
                            break;
                        }
                        i += 1;
                    }
                    if i < n && b[i] == b'>' {
                        i += 1;
                    }
                }
                RxTok::Group
            }
            b')' => {
                i += 1;
                RxTok::Group
            }
            b'*' | b'+' => {
                i += 1;
                if i < n && b[i] == b'?' {
                    i += 1; // 惰性量词
                }
                RxTok::Quant
            }
            b'?' => {
                i += 1;
                if i < n && b[i] == b'?' {
                    i += 1;
                }
                RxTok::Quant
            }
            // 只有 {数字[,数字]} 形式才算量词,否则按字面量
            b'{' => {
                let mut j = i + 1;
                let digits = j;
                while j < n && b[j].is_ascii_digit() {
                    j += 1;
                }
                let mut ok = j > digits;
                if ok && j < n && b[j] == b',' {
                    j += 1;
                    while j < n && b[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                ok = ok && j < n && b[j] == b'}';
                if ok {
                    i = j + 1;
                    if i < n && b[i] == b'?' {
                        i += 1;
                    }
                    RxTok::Quant
                } else {
                    i += 1;
                    RxTok::Literal
                }
            }
            b'|' => {
                i += 1;
                RxTok::Alt
            }
            b'^' | b'$' => {
                i += 1;
                RxTok::Anchor
            }
            b'.' => {
                i += 1;
                RxTok::Punct
            }
            // 普通字面量:一直吃到下一个语法符号
            _ => {
                i += char_len(src, i);
                while i < n
                    && !matches!(
                        b[i],
                        b'\\'
                            | b'['
                            | b'('
                            | b')'
                            | b'*'
                            | b'+'
                            | b'?'
                            | b'{'
                            | b'}'
                            | b'|'
                            | b'^'
                            | b'$'
                            | b'.'
                            | b']'
                    )
                {
                    i += char_len(src, i);
                }
                RxTok::Literal
            }
        };
        out.push((start..i, tok));
    }
    out
}

/// 生成一条正则的着色装饰(逐段下发颜色,字面量用默认前景色)
pub fn decorations(src: &str, light: bool) -> Vec<TextDecoration> {
    let cc = theme::code_colors(light);
    let mut out = Vec::new();
    for (range, tok) in tokenize(src) {
        let hex = match tok {
            RxTok::Literal => cc.plain,
            RxTok::Escape => cc.escape,
            RxTok::EscapeLit => cc.escape_lit,
            RxTok::Class => cc.class,
            RxTok::Group => cc.group,
            RxTok::Quant => cc.quant,
            RxTok::Alt => cc.alt,
            RxTok::Anchor => cc.anchor,
            RxTok::Punct => cc.group,
        };
        out.push(TextDecoration::new(
            range,
            HighlightStyle {
                color: Some(theme::hex(hex)),
                ..Default::default()
            },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 词法片段必须首尾相接、覆盖全文,且不切断 UTF-8 字符
    fn check(src: &str) -> Vec<(Range<usize>, RxTok)> {
        let toks = tokenize(src);
        let mut at = 0;
        for (r, _) in &toks {
            assert_eq!(r.start, at, "区间不连续: {src:?}");
            assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end));
            at = r.end;
        }
        assert_eq!(at, src.len(), "未覆盖全文: {src:?}");
        toks
    }

    #[test]
    fn covers_text_without_gaps() {
        for src in [
            "",
            r"\b(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL)\b",
            r#"(?:^|[\s"'\[])(?:level|lvl|severity)[=:](\w+)"#,
            r"\[([TDIWEVF])\]",
            r"^a{2,4}?b+?c*?$",
            r"[^\d]+\.\d{3}",
            "中文[日志]级别",
        ] {
            check(src);
        }
    }

    #[test]
    fn classifies_common_tokens() {
        let toks = tokenize(r"\b(ERROR)\b");
        let kinds: Vec<RxTok> = toks.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                RxTok::Escape,  // \b
                RxTok::Group,   // (
                RxTok::Literal, // ERROR
                RxTok::Group,   // )
                RxTok::Escape,  // \b
            ]
        );
    }

    #[test]
    fn non_quantifier_braces_stay_literal() {
        let toks = tokenize("a{b}");
        assert!(toks.iter().all(|(_, k)| *k == RxTok::Literal));
        let toks = tokenize("a{2,3}");
        assert_eq!(toks.last().map(|(_, k)| *k), Some(RxTok::Quant));
    }

    #[test]
    fn character_class_swallows_escapes() {
        let toks = tokenize(r"[\]]\d");
        assert_eq!(toks[0].1, RxTok::Class);
        assert_eq!(toks[1].1, RxTok::Escape);
    }
}
