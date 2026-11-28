#!/bin/bash
# 把 target/日志查看器.app 打成 macOS 安装镜像 dmg(内含拖入「应用程序」的快捷方式)。
# 用法: ./scripts/make_dmg.sh
#
# 环境变量:
#   APP     指定 .app 路径(默认 target/日志查看器.app)
#   ARCH    产物架构名 arm64 / x86_64(默认读二进制,取不到时用 uname -m)
#   VERSION 覆盖版本号(默认取 Cargo.toml)
#   OUT     输出路径(默认 logview-macos-<arch>.dmg,位于仓库根目录)
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="日志查看器"
APP="${APP:-target/$APP_NAME.app}"
VERSION="${VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)}"
VERSION="${VERSION:-0.0.0}"

if [ ! -d "$APP" ]; then
    echo "错误: 未找到 $APP(先运行 scripts/make_app.sh)" >&2
    exit 1
fi

if [ -z "${ARCH:-}" ]; then
    ARCH="$(lipo -archs "$APP/Contents/MacOS/logview" 2>/dev/null | awk '{print $1}')" || ARCH=""
fi
if [ -z "${ARCH:-}" ]; then
    ARCH="$(uname -m)"
fi
case "$ARCH" in
    arm64|aarch64) ARCH=arm64 ;;
    x86_64|amd64) ARCH=x86_64 ;;
esac

OUT="${OUT:-logview-macos-$ARCH.dmg}"
STAGE="target/dmg-stage-$ARCH"

echo "==> 组装 dmg 内容 $STAGE"
rm -rf "$STAGE"
mkdir -p "$STAGE"
# ditto 保留 .app 内的符号链接、权限与扩展属性,避免签名被破坏
ditto "$APP" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"

echo "==> 生成 $OUT"
rm -f "$OUT"
hdiutil create -quiet -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
rm -rf "$STAGE"
echo "    已生成 $OUT ($(du -h "$OUT" | cut -f1),$ARCH)"