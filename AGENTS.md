# AGENTS.md — 面向 AI 代理的项目说明

本文件供 AI 编码代理(以及新加入的开发者)快速了解本仓库的结构、约定与注意事项。改动代码前请先阅读。

> ⚠️ **部分内容已过期**：以下关于 egui / eframe、`app.rs` / `ui_viewer.rs` / `ui_open.rs` / `macos_open.rs`、
> Phosphor 图标等的描述属于迁移前的实现；当前代码已改为 GPUI(`app_view.rs` + `gpui-kit`)。
> 两者冲突时以 [README](README.md) 与源码为准；线程模型、安全约束、Git 约定仍然适用。

## 项目概述

**LogView**:原生桌面日志查看器,Rust + egui 0.32 自绘 UI(**无 WebView**),交互参考 Chrome DevTools 控制台。支持本地 / SSH / HTTP 三种日志源、多套正则级别配置与着色筛选、高亮/过滤检索、自动追加(tail -f)、行内 URL/路径/JSON 识别并在新标签页打开。

- 语言/运行时:Rust(edition 2021,建议 ≥1.80)
- UI:egui 0.32 + eframe 0.32(glow 渲染)
- 关键依赖:`ssh2`(libssh2,SFTP)、`reqwest`(blocking + rustls,Range)、`keyring`(系统钥匙串,macOS/Windows/Linux 原生后端)、`rfd`(原生文件对话框)、`serde/serde_json`(持久化)、`regex`、`crossbeam-channel`
- 语言约定:代码注释、文档、提交信息均为**中文**;标识符英文

## 常用命令

```bash
cargo build                     # 构建(依赖以 opt-level=2 编译,保证调试运行流畅)
cargo run                       # 运行,弹出打开对话框
cargo run -- sample/app.log     # CLI 直达打开文件
./sample/gen_log.sh sample/app.log 1   # 演示自动追加(每秒 1 行,Ctrl+C 停止)
cargo build --release           # 发布构建 → target/release/logview
```

验证改动的基本流程:`cargo build` 零 error → `cargo run -- sample/app.log` 打开示例日志 → 目视检查级别着色/chips 计数/链接可点击;`gen_log.sh` 验证自动追加与粘底跟随。

## 目录结构

```
src/
├── main.rs          eframe 入口、窗口参数、CLI args
├── app.rs           App:标签管理、工具栏(搜索/级别 chips/配置选择器)、消息轮询、设置同步、快捷键
├── ui_viewer.rs     日志区:虚拟滚动、分段着色(级别色+搜索高亮+链接合并)、跟随暂停、JSON 视图
├── ui_open.rs       打开源对话框(本地/SSH/HTTP + 最近打开)
├── ui_settings.rs   设置对话框(多套配置规则编辑、级别颜色、导入导出)
├── widgets.rs       antd 风格组件库:按钮(primary/default/text)、级别 Tag、Segmented、Switch、图标按钮(Phosphor)
├── theme.rs         antd v5 设计令牌(暗/亮双配色 Palette)+ 双样式注册(跟随系统)+ 字号/字体系统 + 中文字体加载
├── level.rs         级别常量、别名映射、LevelConfig/Rule、Engine(正则编译+逐行判定)
├── lines.rs         LineStore:字节流→行(按 \n 切分,天然 UTF-8 断字安全)、计数、环形上限
├── search.rs        检索状态与匹配器(文本/正则、高亮区间)
├── links.rs         URL/路径识别、重叠消解、JSON 行探测
├── ssrf.rs          HTTP 源 SSRF 安全校验(网段表)
├── source.rs        三种源 open/meta/read_at、SSH 连接、HTTP 固定 IP client
├── tail.rs          worker 线程:轮询增量读、初始/尾部/全量加载、轮换检测
├── store.rs         settings.json 读写(dirs::config_dir()/logview/)
├── macos_open.rs    (仅 macOS)接收 Finder「打开方式」的 odoc Apple Event
└── scripts/make_app.sh  生成 target/日志查看器.app(bundle + Info.plist 文档类型 + lsregister)
sample/              示例日志 + 追加演示脚本
DESIGN.md            设计文档(架构、数据模型、SSRF 策略、里程碑)
prototype.figcraft   figcraft 原型源文件(旧 egui 版设计稿);icon.png 为应用图标,打包脚本据此生成 AppIcon.icns
```

## 架构要点(改动前必读)

1. **线程模型**:UI 主线程 + 每个日志标签一个 tail worker 线程。通信只有两条 channel:
   - UI→worker `Cmd { Stop, Pause(bool), SeekStart, SeekEnd }`
   - worker→UI `Msg { Append{data,offset,size}, Replace{…}, Partial, Complete, Truncated{size}, NoFollow{size}, Error{msg,fatal} }`
   UI 在 `poll_msgs` 中 drain 消息;**不要**在 worker 与 UI 间共享锁。
2. **行模型**:`LineStore::push_bytes` 按 `\n` 切分字节再 decode(UTF-8 续字节 ≥0x80 不含 0x0A,天然断字安全);级别在追加时由 `Engine::eval` 判定;配置变更后由 UI 分帧 `relevel_range`(10 万行/帧)重判定,期间 chips 显示"重新分析级别中…"。
3. **虚拟滚动**:`ScrollArea::both().show_rows(ui, ROW_H=20.0, total, …)`,只渲染可见行;行宽 = 段落 galley 宽之和与可用宽取 max(支持横向滚动)。检索跳转通过 `scroll_request → 写回 ScrollArea State.offset`(egui 0.32 无 `scroll_to_row`)。
4. **视图管线**:过滤模式/隐藏级别 → `Tab.view: Vec<u32>`(真实行索引);高亮模式 → `Tab.matched`;两者在追加时增量维护,在搜索/级别/配置变化时全量重建(`rebuild_view` / `rebuild_matched`)。
5. **配置同步**:`App.settings_rev` 版本号驱动;变更后每标签重编译 `Engine` 并触发重判定。主页配置选择器与设置对话框共享同一份 `settings.configs`(enabled 字段),任何修改都要 `settings_rev += 1` 并 `store::save`。
6. **HTTP 源与 SSRF**:见下方安全约束;重定向逐跳重新 `validate_url` 并重建固定 IP 的 client(`http_get`)。
7. **文件打开三入口**:(a) CLI argv(支持多文件,`App::new` 遍历);(b) 拖入窗口(`ctx.input raw.dropped_files`/`hovered_files`,`handle_dnd` 带覆盖提示);(c) macOS「打开方式」odoc Apple Event。三者最终都走 `App::open_source(Local…)`。
8. **macOS odoc 接收**(`macos_open.rs`):winit 0.30 不处理 Apple Event 且其 delegate `WinitApplicationDelegate` 未实现 `application:openFile(s):`,故启动时用 libobjc `class_addMethod` 给该类补方法。**坑**:
   - delegate 方法 selector 带两个参数,IMP 签名必须是 4 参 `(self, _cmd, app: NSApplication, files)`,types 编码 `c@:@@` / `v@:@@` —— 少写一个参数会指针错位,收到 `-[NSApplication count]` 之类的崩溃;
   - `WinitApplicationDelegate` 是 objc2 `declare_class!` 惰性注册的类,`main()` 里 `objc_getClass` 拿不到 —— `App::new` 试一次、`update` 每帧重试直到成功(`App::odoc_hooked`);
   - 收到的文件入全局队列,UI 每帧 `take_opened()` 取用(勿跨线程直接碰 UI)。
9. **滚动控制**:egui 0.32 无 `scroll_to_row`;程序化滚动 = 修改 `ScrollAreaOutput.state.offset` 后 `ctx.data_mut(|d| d.insert_temp(out.id, state))` 写回。「跳到末尾/恢复跟随」用 `Tab.force_bottom_once` 走同一机制(仅 stick_to_bottom 不保证切换时立即吸附)。
10. **融合标题栏**:macOS 用 `ViewportBuilder::with_fullsize_content_view(true)+with_titlebar_shown(true)+with_titlebar_buttons_shown(true)+with_title_shown(false)`;标签栏左侧预留 74px 给红绿灯,`ui.interact(Sense::drag)` + `ViewportCommand::StartDrag` 实现拖动窗口。Windows/Linux 仍用原生标题栏(cfg 区分)。
11. **worker 命令延迟**:tail 主循环 `recv_timeout(100ms)` + `last_tick` 节拍控制轮询 —— 命令(SeekEnd 等)100ms 内被处理,不要改回"recv_timeout(poll_ms)"(会让按钮感觉无响应)。
11b. **【关键】egui 不会因 worker 消息而出帧**:`poll_msgs` 只在 update 帧里运行;没有用户交互时 egui 不重绘,channel 里的 Append/Replace 会一直积压,表现为"点了按钮没反应/日志不刷新"。因此 `update()` 开头必须 `ctx.request_repaint_after(150ms)`(诊断开关:LOGVIEW_DEBUG=1 + LOGVIEW_SEEK_DEBUG=1 自动触发一次跳底并输出全链路 [dbg] 日志)。
12. **主题(亮/暗跟随系统)**:egui-winit 会把 `ThemeChanged` 转成 `raw.system_theme`;`theme::apply_themes` 设 `ThemePreference::System` 并用 `set_style_of(Theme, style)` 注册暗/亮两套样式(egui 自动切换);自定义配色表 `Palette` 由 `theme::sync(ctx)` 每帧跟随 `ctx.theme()` 刷新,调用点用 `theme::BG0()` 这类**函数访问器**(不是常量,注意别写成裸标识符)。级别颜色**暗/亮主题各一套配置**(`settings.colors` / `colors_light`,`settings.level_colors_of(light)` 是 chips 与日志行的唯一来源,二者天然一致——不要引入任何自动变换,否则会偏离配置值);搜索高亮默认随主题取 `highlight_dark/light`(设置里 `highlight_use_light` 可强制)。
12b. **字号系统**:设置 `ui_font_size`/`log_font_size`(f32,默认 13)→ `theme::set_font_sizes` 变化时重建双主题样式;新增 UI 文本一律用 `theme::scaled(原字号)`(按基准 13pt 百分比),日志区文本用 `theme::log_scaled(原字号)`,行高用 `ui_viewer::ROW_H()`(= `theme::row_h()`,随日志字号)——**不要硬编码字号**;`build_style` 的 text_styles 从这些静态值动态读取。
12c. **字体家族**:egui 只渲染 `FontDefinitions` 里加载的字体,不使用系统字体 —— `theme::apply_fonts(ctx, ui_family, log_family)` 按候选表(`ui_font_candidates`/`log_font_candidates`)读取系统字体文件(注意 dfont 等格式 ab_glyph 不支持,候选表只收录 ttf/ttc),插入到对应家族**首位**;界面字体未显式选择时加载**系统默认 UI 字体**(`system_ui_font()`:macOS=SFNS.ttf,Windows=segoeui.ttf,Linux=NotoSans),并把中文字体回退追加到两个家族末尾(否则 CJK 显示方块)。字体解析失败 egui 会警告并跳过该字体,不会 panic。App 用 `fonts_applied` 字段做变更检测,设置变化才重载。
12d. **设计系统(antd v5 令牌)**:`theme::Palette` 为语义令牌(primary/success/warning/error/bg_layout/bg_container/bg_elevated/text1-3/border/split/fill/行底色…),暗/亮两套常量 DARK/LIGHT;`theme::sync(ctx, primary_override)` 每帧跟随主题 + 应用自定义主题色(`settings.primary_color`,空 = 默认);**新界面元素一律用访问器取色(theme::PRIMARY() 等)+ `theme::scaled()` 字号,禁止硬编码**。组件库在 widgets.rs:button_primary/button_default/button_text/icon_btn(Phosphor 字形)/level_chip(antd Tag)/segmented/switch/tab_strip/text_field(antd 描边)。图标用 `egui_phosphor::fill::*` 常量(fill feature 已启用),通过 `add_to_fonts` 注册为 Proportional 回退,PUA 字符可混入普通文本。
12e. **依赖版本强约束**:`egui-phosphor` 必须 0.10(绑定 egui 0.32)—— 0.11+ 绑定 egui 0.33+,会出现两份 epaint 导致链接失败("multiple different versions of crate epaint")。升级 egui 时需同步升级 egui-phosphor。

## 硬性安全约束(不得弱化)

- HTTP 日志源仅允许 `http/https`;请求前 `ssrf::validate_url` 校验 DNS **全部**地址,拒绝 localhost、环回、私有与保留网段(网段表在 `ssrf.rs`);连接固定到已校验 IP(防 DNS rebinding);重定向每一跳复检。
- 凭据(密码/口令/token/私钥正文)默认只存内存;`SourceParams::sanitized()` 用于"最近打开"持久化,设置文件永远不含凭据。仅当用户显式勾选「记住连接」时,由 `secrets.rs` 写入系统钥匙串(服务名 `logview`);钥匙串不可用时退回仅内存 + 手动输入,不得回退到明文落盘。旧版明文凭据在 `store::load()` 中一次性迁移,迁移失败也要清除明文。SSH 私钥正文不写入钥匙串(Windows 单条 2560 字节上限)。
- 修改 `ssrf.rs` 或 `source.rs` 的请求路径时,保持以上性质并说明理由。

## egui 0.32 API 注意(易踩坑)

- `Button`/`Frame` 用 `corner_radius`(不是 `rounding`);`CornerRadius` 字段是 `nw/ne/sw/se`(不是 tl/tr/bl/br)
- `Frame::new()`(不是 `Frame::default()`);`Margin` 字段为 **i8**(`Margin::symmetric(8, 3)`)
- `RichText::size(12.0)` 需要**带小数的字面量**;`Stroke::new(1.0_f32, …)` 需 f32 后缀
- `Button` 没有 `.size()`/`.inner_margin()`(后者用文本两侧加空格或包 Frame)
- 程序化滚动:`out.state.offset` 公开,show 之后 `ctx.data_mut(|d| d.insert_temp(out.id, state))` 写回
- `eframe::App::on_exit(&mut self, _gl: Option<&eframe::glow::Context>)` 是**两个参数**
- 菜单内关闭:`ui.close()`;颜色编辑:`ui.color_edit_button_rgb(&mut [f32;3])`(无 color32 版、无 color-picker feature)
- `reqwest::blocking::Response` 无 `chunk()`,限流读取用 `std::io::Read::take().read_to_end()`
- 中文字体:egui 不带 CJK,`theme::load_fonts` 按候选列表加载系统字体作为 fallback,**别删**

## 代码约定

- 错误处理:面向用户的错误用 `String` 消息(中文、可操作);模块内部可用 `anyhow`(当前源/worker 层用 String 直传 `Msg::Error`)
- 注释密度:每个模块顶部 `//!` 一句话职责;关键函数/非直观逻辑处行内中文注释
- 命名:类型/文件与职责对应;UI 控件文案中文,日志字段(协议)英文
- 不要引入 WebView/浏览器依赖;不要添加重运行时(如 tokio 全家桶)——保持 blocking 模型
- 新增依赖需在 README 与本文件中说明理由

## 已知限制(勿当 bug 修)

- 长行不折行(横向滚动);ANSI 色码未解析(原样显示)
- HTTP 无 Range 支持的源:整页加载、无法跟随(发送 `NoFollow`)
- 行数上限默认 20 万,超出环形丢弃最旧行(状态栏计数即为当前保留行数)
- 内网 HTTP 地址被 SSRF 策略拒绝是有意行为

## Git 约定

- 提交信息中文,格式:`<类型>: <一句话摘要>` + 空行 + 要点列表;类型用 feat/fix/refactor/docs/chore
- `Cargo.lock` 入库(应用型仓库)
- `target/`、`.DS_Store` 已在 `.gitignore`
