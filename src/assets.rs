// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 应用资源:内置图标 SVG,其余路径回退到 gpui-kit 自带图标库
//!
//! 图标来源:ionicons 5.0.0(512×512 viewBox,单色渲染,颜色由 `svg().text_color()` 决定)。
//! 新增图标 = 把 svg 放进 `assets/icons/` 并在 `ICONS` 里加一行。

use std::borrow::Cow;

use gpui_kit::assets::Assets;
use gpui_kit::{AssetSource, Result, SharedString};

/// 内置图标表:`(资源路径, 文件内容)`
const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/settings.svg",
        include_bytes!("../assets/icons/settings.svg"),
    ),
    // 跟随中 / 已暂停
    (
        "icons/tail-on.svg",
        include_bytes!("../assets/icons/tail-on.svg"),
    ),
    (
        "icons/tail-off.svg",
        include_bytes!("../assets/icons/tail-off.svg"),
    ),
    (
        "icons/jump-end.svg",
        include_bytes!("../assets/icons/jump-end.svg"),
    ),
    (
        "icons/clear.svg",
        include_bytes!("../assets/icons/clear.svg"),
    ),
    (
        "icons/load-all.svg",
        include_bytes!("../assets/icons/load-all.svg"),
    ),
    ("icons/wrap.svg", include_bytes!("../assets/icons/wrap.svg")),
    (
        "icons/configs.svg",
        include_bytes!("../assets/icons/configs.svg"),
    ),
    ("icons/edit.svg", include_bytes!("../assets/icons/edit.svg")),
    ("icons/save.svg", include_bytes!("../assets/icons/save.svg")),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    // 设置页「分析配置」右上角动作:新建 / 导入 / 导出 / 恢复内置默认
    (
        "icons/config-new.svg",
        include_bytes!("../assets/icons/config-new.svg"),
    ),
    (
        "icons/config-import.svg",
        include_bytes!("../assets/icons/config-import.svg"),
    ),
    (
        "icons/config-export.svg",
        include_bytes!("../assets/icons/config-export.svg"),
    ),
    (
        "icons/config-restore.svg",
        include_bytes!("../assets/icons/config-restore.svg"),
    ),
    // 连接页「已保存的 SSH 主机」动作 / 快捷键行的图标按钮
    (
        "icons/host-add.svg",
        include_bytes!("../assets/icons/host-add.svg"),
    ),
    (
        "icons/host-import-recent.svg",
        include_bytes!("../assets/icons/host-import-recent.svg"),
    ),
    (
        "icons/host-import-ssh.svg",
        include_bytes!("../assets/icons/host-import-ssh.svg"),
    ),
    (
        "icons/edit-outline.svg",
        include_bytes!("../assets/icons/edit-outline.svg"),
    ),
    (
        "icons/reset.svg",
        include_bytes!("../assets/icons/reset.svg"),
    ),
    (
        "icons/delete.svg",
        include_bytes!("../assets/icons/delete.svg"),
    ),
    // 分析配置规则块:新增规则 / 只读锁定
    (
        "icons/rule-add.svg",
        include_bytes!("../assets/icons/rule-add.svg"),
    ),
    ("icons/lock.svg", include_bytes!("../assets/icons/lock.svg")),
    // 「关于」页:GitHub 仓库入口
    (
        "icons/github.svg",
        include_bytes!("../assets/icons/github.svg"),
    ),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = Assets.list(path)?;
        for (p, _) in ICONS {
            if p.starts_with(path) && !items.iter().any(|i| i.as_ref() == *p) {
                items.push((*p).into());
            }
        }
        Ok(items)
    }
}
