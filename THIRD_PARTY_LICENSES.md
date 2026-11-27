# 第三方许可与素材声明

本项目以 **GNU AGPL-3.0**(`AGPL-3.0-only`)发布（见 [LICENSE](LICENSE)）。以下内容为随源码或二进制分发的第三方素材与依赖，
其许可条款独立于本项目，分发时需一并保留。

## 1. 内置图标素材

`assets/icons/*.svg` 取自 **Ionicons 5.0.0** 的子集（chevron-down / clear / configs / edit /
jump-end / load-all / save / settings / tail-off / tail-on / wrap / logo-github），经单色化处理后在
`src/assets.rs` 中通过 `include_bytes!` 内嵌。

```
Ionicons — Copyright (c) 2015-present Ionic (http://ionic.io/)
Licensed under the MIT License.
https://github.com/ionic-team/ionicons
```

其余图标回退到依赖 `gpui-kit` 自带的图标资源，遵循其 Apache-2.0 许可。

## 2. 应用图标

`icon.png` 为项目自有素材，文件内嵌 XMP 中的 AIGC 内容标识（TC260）。该标识不属于第三方版权素材，
如需在发布流程中统一元数据，可用 `exiftool -all= icon.png` 处理（会改变文件哈希）。

## 3. 直接依赖

| 依赖 | 版本 | 许可 |
|---|---|---|
| gpui-kit | 0.11 | Apache-2.0 |
| serde / serde_json | 1 | MIT OR Apache-2.0 |
| regex | 1 | MIT OR Apache-2.0 |
| dirs | 5 | MIT OR Apache-2.0 |
| open | 5 | MIT |
| anyhow | 1 | MIT OR Apache-2.0 |
| crossbeam-channel | 0.5 | MIT OR Apache-2.0 |
| ssh2 | 0.9 | MIT OR Apache-2.0 |
| reqwest | 0.12 | MIT OR Apache-2.0 |
| ropey | 2.0.0-beta.1 | MIT OR Apache-2.0 |
| keyring | 3 | MIT OR Apache-2.0 |
| chrono | 0.4 | MIT OR Apache-2.0 |

完整传递依赖清单可由以下任一方式生成（建议在发布流程中执行）：

```bash
cargo install cargo-about
cargo about generate about.hbs > THIRD_PARTY_LICENSES.generated.md
# 或
cargo install cargo-deny
cargo deny check licenses
```

## 4. 许可兼容性说明

- 本项目为 AGPL-3.0-only；MIT / Apache-2.0 依赖均可与 AGPL-3.0 组合分发，分发时需保留其许可与版权声明。
- 注意：AGPL 的传染性适用于本项目自身代码；第三方依赖仍按其原许可使用，不得移除其声明。
- 仓库内未内嵌字体文件：界面与日志字体均从操作系统读取，不涉及字体再分发。