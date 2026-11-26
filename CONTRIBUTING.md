# 贡献指南

感谢你考虑为 LogView 提交代码或文档。以下约定用于保持评审与发布流程稳定。

## 环境要求

- Rust **1.92+**（`rustup` 安装 stable 即可；依赖链中 keyring → zbus → oo7 要求 1.92）。
- macOS / Windows：无需额外系统依赖。
- Linux：尚未充分验证。构建/测试需要 X11 或 Wayland 开发库、字体库与 DBus，参考：

  ```bash
  sudo apt-get install -y build-essential pkg-config \
    libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev libxkbcommon-x11-dev \
    libfontconfig1-dev libfreetype6-dev libvulkan-dev libdbus-1-dev
  ```

  若构建失败，欢迎提交带完整报错的 issue。

## 常用命令

```bash
cargo build                       # 调试构建(依赖以 opt-level=2 编译)
cargo run -- sample/app.log       # 直接打开示例日志
cargo test --locked               # 单元测试
cargo fmt --all                   # 格式化(CI 会检查)
cargo clippy --all-targets --locked
```

演示自动追加（另开终端，会写入 `sample/app.log`）：

```bash
./sample/gen_log.sh sample/app.log 1
```

> `sample/app.log` 是受版本控制的示例数据。运行 `gen_log.sh` 后提交前请还原：
> `git checkout -- sample/app.log`。
>
> `dev.sh` / `package.sh` 是维护者本机工作流脚本，**不在版本库中**，也不要在 PR 里添加。

## 代码结构

模块职责见 [README 的「架构」一节](README.md#架构)。几条关键约束：

1. UI 主线程与每个标签的 tail worker 线程之间只通过 `crossbeam-channel` 通信，不共享锁。
2. 日志行在 worker 追加时完成级别判定；配置变更由 UI 分帧重判定（`relevel_step`）。
3. 依赖版本受 gpui-kit 约束，升级 UI 层前请先确认 API 变更。
4. 不要引入 WebView、异步运行时或重依赖；HTTP 源保持 blocking + 固定 IP 模型。

> `AGENTS.md` 记录的是 egui 时期的实现细节，部分内容已与当前 GPUI 实现不一致；
> 冲突时以 README 与源码为准。

## 贡献命令预设

命令源的能力全部由 `settings.json` 的 `command_presets` 驱动,不必写 Rust:

- `id` / `name`:稳定标识与显示名;`local` / `ssh`:是否可用于本地、SSH 目标。
- `argv`:命令模板,`${字段名}` 占位;值解析为空的参数会连同紧邻的以 `-` 开头的开关一起丢弃(可选参数).
- `fields`:表单字段(`key`/`label`/`placeholder`/`optional`),`optional` 为真时允许留空。
- `resolve`:执行前预解析,把命令输出填入某个占位符(例:`adb shell pidof -s ${package}` → `${pid}`)。
- `level_config_id`:产出的行用哪套级别配置分析。

内置预设定义在 `src/command.rs` 的 `builtin_presets()`,只放通用、无争议的命令;厂商专用或需要固定主机/路径的预设建议由用户自行添加(设置文件里直接加即可)。新增内置项时保留既有 id,`store.rs` 的迁移只按 id 追加缺失项,不会覆盖用户修改。改完请补 `command.rs` 的单元测试(模板展开、可选参数丢弃、必填校验)。

## 提交规范

提交信息使用中文，格式：

```
<类型>: <一句话摘要>

- 要点一
- 要点二
```

类型：`feat` / `fix` / `refactor` / `docs` / `chore` / `perf` / `test`。

## Pull Request 检查清单

- [ ] `cargo fmt --all` 后无改动
- [ ] `cargo clippy --all-targets --locked` 无新增告警
- [ ] `cargo test --locked` 通过
- [ ] 手工冒烟：打开 `sample/app.log`，检查级别着色与 chips 计数、检索跳转、自动追加跟随
- [ ] 涉及 SSH / HTTP / 凭据 / SSRF 的改动，在描述中说明安全影响
- [ ] 新增依赖在 README 与本 PR 中说明理由
- [ ] 不包含本机路径、真实主机名、凭据或隐私日志

## 贡献授权

本项目以 **AGPL-3.0-only** 发布。提交 PR 即表示你确认：

1. 你有权提交该内容，且不包含他人的受限代码；
2. 你同意以 AGPL-3.0-only 授权你的贡献，不附加额外条款；
3. 若贡献属于雇主或组织，你已获得相应授权。

## 报告问题

- 功能缺陷与建议：使用 issue 模板提交。
- 安全漏洞：请走 [SECURITY.md](SECURITY.md) 的私密渠道，不要公开 issue。