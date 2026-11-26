#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! LogView 入口:解析 CLI 参数,创建主窗口(融合标题栏),交给 AppView 渲染

mod app_view;
mod assets;
mod code;
mod code_input;
mod level;
mod lines;
mod links;
mod search;
mod secrets;
mod source;
mod ssh_config;
mod ssrf;
mod store;
mod tail;
mod theme;

use app_view::AppView;
use gpui_kit::{px, size, AppContext as _, WindowOptions};

/// 应用展示名(Bundle / 菜单栏 / 窗口标题 / 任务栏一致使用)
const APP_NAME: &str = "日志查看器";

fn main() {
    let paths: Vec<std::path::PathBuf> = std::env::args()
        .skip(1)
        .filter(|p| std::path::Path::new(p).is_file())
        .map(std::path::PathBuf::from)
        .collect();

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx: &mut gpui_kit::App| {
            gpui_kit::init(cx);
            // 围栏代码块着色:给 base 的 Text View 注入全局 highlighter
            code::install_code_block_highlighter(cx);
            // 组件库文案(颜色选择器等)跟随中文界面
            gpui_kit::component::set_locale("zh-CN");
            cx.activate(true);

            let settings = store::load();
            // 窗口标题(任务栏 / Mission Control / 窗口菜单):不设置时为空
            let mut titlebar = gpui_kit::component::TitleBar::title_bar_options();
            titlebar.title = Some(APP_NAME.into());
            let options = WindowOptions {
                window_min_size: Some(size(px(960.0), px(600.0))),
                titlebar: Some(titlebar),
                ..gpui_kit::component::TitleBar::window_options()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| AppView::new(window, cx, paths, settings))
            })
            .expect("打开主窗口失败");

            // 亮/暗跟随系统
            gpui_kit::component::Theme::sync_system_appearance(None, cx);
        });
}
