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
