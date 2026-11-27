#!/bin/sh
# 把便携包安装到当前用户目录(默认 ~/.local),并注册桌面条目与图标。
#
# 用法:
#   ./install.sh              # 安装到 $PREFIX(默认 ~/.local)
#   PREFIX=/usr/local ./install.sh
#   ./install.sh --uninstall  # 卸载
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}
bindir="$prefix/bin"
appdir="$prefix/share/applications"
icondir="$prefix/share/icons/hicolor/256x256/apps"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bindir/logview" "$appdir/logview.desktop" "$icondir/logview.png"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$appdir" || true
    echo "已卸载:$bindir/logview"
    exit 0
fi

mkdir -p "$bindir" "$appdir" "$icondir"
install -m 0755 "$here/logview" "$bindir/logview"
install -m 0644 "$here/logview.desktop" "$appdir/logview.desktop"
install -m 0644 "$here/logview.png" "$icondir/logview.png"

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$appdir" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" || true

echo "已安装:$bindir/logview"
case ":$PATH:" in
    *":$bindir:"*) ;;
    *) echo "提示:$bindir 不在 PATH 中,请加入后重新登录,或用绝对路径启动。" ;;
esac
echo "文件管理器里右键 .log/.txt →「打开方式」即可看到「日志查看器」。"