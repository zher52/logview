//! 打包资源的静态校验:桌面条目、图标与脚本必须与打包脚本的约定保持一致。
//! 真正的 Linux 产物由 CI 构建,这里守住最容易改错、又不会在编译期暴露的字段。

use std::path::Path;

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", p.display()))
}

#[test]
fn desktop_entry_matches_packaging_contract() {
    let entry = read("assets/linux/logview.desktop");
    assert!(
        entry.starts_with("[Desktop Entry]\n"),
        "缺少 [Desktop Entry] 头"
    );
    for key in [
        "Type=Application",
        "Exec=logview %f",
        "TryExec=logview",
        "Icon=logview",
        "Terminal=false",
    ] {
        assert!(entry.lines().any(|l| l == key), "桌面条目缺少 {key}");
    }

    let mime = entry
        .lines()
        .find(|l| l.starts_with("MimeType="))
        .expect("桌面条目缺少 MimeType");
    assert!(mime.ends_with(';'), "MimeType 必须以 ; 结尾");
    for ty in ["text/plain;", "text/x-log;", "application/json;"] {
        assert!(mime.contains(ty), "MimeType 缺少 {ty}");
    }

    for key in ["Name[zh_CN]=", "Name[ru]=", "Comment[zh_CN]="] {
        assert!(entry.contains(key), "桌面条目缺少本地化字段 {key}");
    }
}

#[test]
fn linux_assets_are_present_and_icon_is_256px() {
    let png = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/linux/logview.png"))
        .expect("缺少 assets/linux/logview.png");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "图标不是 PNG");
    // IHDR:签名 8 字节 + 长度 4 + "IHDR" 4,随后是大端宽、高各 4 字节
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (256, 256), "桌面图标应为 256x256");

    let script = read("scripts/make_linux.sh");
    for needle in [
        "--no-appimage",
        "appimagetool",
        "install.sh",
        "logview.desktop",
    ] {
        assert!(script.contains(needle), "打包脚本缺少 {needle}");
    }
    let installer = read("assets/linux/install.sh");
    for needle in ["--uninstall", "hicolor/256x256/apps", "logview.desktop"] {
        assert!(installer.contains(needle), "安装脚本缺少 {needle}");
    }
}
