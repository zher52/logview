#!/bin/bash
# 构建 Linux 便携包:可执行文件 + .desktop + 图标 + 安装脚本,可选生成 AppImage。
#
# 用法:
#   ./scripts/make_linux.sh                 # release 构建,出 tar.gz(有 appimagetool 时同时出 AppImage)
#   ./scripts/make_linux.sh --no-appimage   # 只出 tar.gz
#   SKIP_BUILD=1 ./scripts/make_linux.sh    # 复用已有二进制(打包脚本自检)
#
# 环境变量:
#   VERSION      覆盖版本号(默认取 Cargo.toml)
#   ARCH         覆盖架构名 x86_64 / aarch64(默认按 uname -m 映射)
#   BIN          指定要打包的可执行文件(默认 target/release/logview)
#   APPIMAGETOOL 指定 appimagetool 路径(默认在 PATH 中查找;找不到则跳过 AppImage)
#
# 产物(target/ 下):
#   logview-linux-<arch>/           解包后的便携目录
#   logview-linux-<arch>.tar.gz     便携包(解压后执行 ./install.sh)
#   logview-<arch>.AppImage         单文件可执行(需 appimagetool)
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="日志查看器"
BIN_NAME="logview"

MAKE_APPIMAGE=1
while [ $# -gt 0 ]; do
    case "$1" in
        --no-appimage) MAKE_APPIMAGE=0 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1(用 --help 查看用法)" >&2; exit 2 ;;
    esac
    shift
done

VERSION="${VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)}"
VERSION="${VERSION:-0.0.0}"

if [ -z "${ARCH:-}" ]; then
    case "$(uname -m)" in
        x86_64|amd64) ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *) ARCH="$(uname -m)" ;;
    esac
fi

BIN="${BIN:-target/release/$BIN_NAME}"
if [ "${SKIP_BUILD:-0}" = "1" ]; then
    echo "==> 跳过构建,复用 $BIN"
else
    echo "==> 构建 release (v$VERSION)"
    cargo build --release --locked
fi
if [ ! -x "$BIN" ]; then
    echo "错误: 未找到可执行文件 $BIN" >&2
    exit 1
fi

STAGE="target/logview-linux-$ARCH"
echo "==> 组装便携包 $STAGE"
rm -rf "$STAGE"
mkdir -p "$STAGE"
install -m 0755 "$BIN" "$STAGE/$BIN_NAME"
install -m 0644 assets/linux/logview.desktop "$STAGE/logview.desktop"
install -m 0644 assets/linux/logview.png "$STAGE/logview.png"
install -m 0755 assets/linux/install.sh "$STAGE/install.sh"
[ -f LICENSE ] && install -m 0644 LICENSE "$STAGE/LICENSE" || true

TARBALL="target/logview-linux-$ARCH.tar.gz"
tar -czf "$TARBALL" -C target "logview-linux-$ARCH"
echo "    已生成 $TARBALL ($(du -h "$TARBALL" | cut -f1))"

if [ "$MAKE_APPIMAGE" -eq 0 ]; then
    echo "==> 跳过 AppImage(--no-appimage)"
    exit 0
fi

TOOL="${APPIMAGETOOL:-}"
if [ -z "$TOOL" ]; then
    TOOL="$(command -v appimagetool || true)"
fi
if [ -z "$TOOL" ] || [ ! -x "$TOOL" ]; then
    echo "提示: 未找到 appimagetool,跳过 AppImage(可用 APPIMAGETOOL=/path 指定)"
    exit 0
fi

APPDIR="target/$APP_NAME.AppDir"
echo "==> 组装 AppDir $APPDIR"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
install -m 0755 "$BIN" "$APPDIR/$BIN_NAME"
install -m 0644 assets/linux/logview.desktop "$APPDIR/logview.desktop"
install -m 0644 assets/linux/logview.png "$APPDIR/logview.png"
install -m 0644 assets/linux/logview.desktop "$APPDIR/usr/share/applications/logview.desktop"
install -m 0644 assets/linux/logview.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/logview.png"
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/logview" "$@"
APPRUN
chmod 0755 "$APPDIR/AppRun"

APPIMAGE="target/logview-$ARCH.AppImage"
echo "==> 生成 AppImage $APPIMAGE"
ARCH="$ARCH" "$TOOL" "$APPDIR" "$APPIMAGE"
echo "    已生成 $APPIMAGE ($(du -h "$APPIMAGE" | cut -f1))"
echo "==> 完成(v$VERSION,$ARCH)"