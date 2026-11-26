#!/bin/bash
# 构建「日志查看器.app」(macOS bundle),注册日志/文本文件类型,
# 使 Finder 右键「打开方式」可以看到 日志查看器。
# 用法: ./scripts/make_app.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# 应用展示名:菜单栏 / Dock / Finder / 「打开方式」都用它(与 main.rs 的 APP_NAME 一致)
APP_NAME="日志查看器"
# 可执行文件名保持 ASCII,CLI 仍叫 logview
BIN_NAME="logview"
APP="target/$APP_NAME.app"
# 版本号:优先取环境变量,其次读 Cargo.toml(避免两处手改不一致)
VERSION="${VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)}"
VERSION="${VERSION:-0.0.0}"

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"

# 应用图标:icon.png → AppIcon.icns(缺失或转换失败不阻断打包)
if [ -f icon.png ]; then
    ICONSET="target/AppIcon.iconset"
    rm -rf "$ICONSET" && mkdir -p "$ICONSET"
    for size in 16 32 64 128 256 512; do
        sips -z $size $size icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null 2>&1 || true
        if [ $size -le 512 ]; then
            sips -z $((size*2)) $((size*2)) icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null 2>&1 || true
        fi
    done
    if iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns" >/dev/null 2>&1; then
        echo "已生成应用图标 AppIcon.icns"
    else
        echo "警告: 图标转换失败,跳过(不影响运行)"
    fi
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>local.fandd.logview</string>
  <key>CFBundleExecutable</key><string>$BIN_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>10.13</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>日志/文本文件</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>CFBundleTypeExtensions</key>
      <array>
        <string>log</string><string>txt</string><string>out</string>
        <string>err</string><string>json</string><string>csv</string>
      </array>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.plain-text</string><string>public.data</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# 代码签名(如果 Keychain 里有有效身份)
IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null | grep "Apple Development" | head -1 | awk '{print $2}')
if [ -n "$IDENTITY" ]; then
    codesign --force --sign "$IDENTITY" "$APP" 2>&1 | tail -1
    echo "已签名: $IDENTITY"
else
    # 无有效身份则使用 ad-hoc 签名
    codesign --force --sign - "$APP" 2>/dev/null
    echo "使用 ad-hoc 签名(无 Apple 开发证书)"
fi

# 刷新 LaunchServices 注册,让"打开方式"立即出现 日志查看器
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
"$LSREGISTER" -f "$APP" 2>/dev/null || true

echo "已生成 $APP"
echo "试一下:"
echo "  open -a \"$PWD/$APP\" \"$PWD/sample/app.log\""
echo "或在 Finder 中右键任意 .log 文件 → 打开方式 → 日志查看器"
