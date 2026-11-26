//! 设置持久化:级别配置、颜色、轮询参数、最近打开(与 UI 框架解耦,纯 hex 颜色)

use serde::{Deserialize, Serialize};

use crate::level::{builtin_configs, LevelConfig};
use crate::source::{SourceParams, SshAuth};

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub poll_ms: u64,
    pub max_lines: usize,
    pub initial_mb: u64,
    pub tail_mb: u64,
    pub show_line_numbers: bool,
    /// 日志行左侧显示时间戳(默认关闭);开启不影响采集,只影响展示
    #[serde(default)]
    pub show_timestamps: bool,
    /// 界面字号(pt);历史设置文件缺少该字段时取默认 13
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
    /// 日志字号(pt);日志行高按此换算
    #[serde(default = "default_log_font_size")]
    pub log_font_size: f32,
    /// 界面字体族(空 = 跟随系统 UI 字体)
    #[serde(default)]
    pub ui_font_family: String,
    /// 日志字体族(空 = 跟随系统等宽字体)
    #[serde(default)]
    pub log_font_family: String,
    /// 日志行默认自动换行(新标签页的初始值;标签页内可单独切换)
    #[serde(default = "default_wrap")]
    pub wrap: bool,
    #[serde(default)]
    pub colors: Vec<String>,
    #[serde(default)]
    pub colors_light: Vec<String>,
    #[serde(default)]
    pub highlight_dark: String,
    #[serde(default)]
    pub highlight_light: String,
    #[serde(default)]
    pub primary_color: String,
    #[serde(default = "builtin_configs")]
    pub configs: Vec<LevelConfig>,
    #[serde(default)]
    pub recents: Vec<Recent>,
    /// 设置页可增删改的 SSH 主机列表(主机输入框下拉的来源之一)
    #[serde(default)]
    pub ssh_hosts: Vec<SavedHost>,
    /// 最近打开最多保留条数(默认 50)
    #[serde(default = "default_recents_limit")]
    pub recents_limit: usize,
    /// 删除 URL 最近打开记录时是否同时清除系统钥匙串里的对应凭据(默认开启)
    #[serde(default = "default_true")]
    pub recents_purge_creds: bool,
    /// 自定义快捷键:动作 id → 规格(如 "secondary+shift+k");缺省 = 默认绑定
    #[serde(default)]
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// 启动期凭据迁移提示(仅内存,不写入设置文件)
    #[serde(skip)]
    pub cred_notice: Option<String>,
}

fn default_recents_limit() -> usize {
    50
}

fn default_true() -> bool {
    true
}

fn default_wrap() -> bool {
    true
}

fn default_ui_font_size() -> f32 {
    13.0
}

fn default_log_font_size() -> f32 {
    13.0
}

/// 可重绑定的快捷键动作:动作 id 与设置页展示文案(顺序即展示顺序)
pub const SHORTCUT_ACTIONS: [(&str, &str); 7] = [
    ("new_tab", "新建标签页"),
    ("close_tab", "关闭当前标签页"),
    ("find", "聚焦检索框"),
    ("clear", "清屏"),
    ("settings", "打开设置"),
    ("copy_row", "复制选中行"),
    ("save_code", "保存代码视图修改"),
];

/// 动作的出厂快捷键规格;secondary = macOS 上 ⌘、其他平台 Ctrl
pub fn default_shortcut(action: &str) -> &'static str {
    match action {
        "new_tab" => "secondary+t",
        "close_tab" => "secondary+w",
        "find" => "secondary+f",
        "clear" => "secondary+k",
        "settings" => "secondary+,",
        "copy_row" => "secondary+c",
        "save_code" => "secondary+s",
        _ => "",
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Recent {
    pub label: String,
    pub params: SourceParams,
    /// 凭据是否已保存到系统钥匙串(旧设置文件缺省 false)
    #[serde(default)]
    pub remembered: bool,
}

/// 设置页「已保存的 SSH 主机」条目:只保存连接目标,不保存任何凭据
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedHost {
    /// 备注名(可空,用于区分同一主机的不同用途)
    #[serde(default)]
    pub name: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
}

fn default_ssh_port() -> u16 {
    22
}

impl Settings {
    /// 指定主题的级别颜色(hex,7 个,顺序 TRACE…FATAL/无级别)
    pub fn level_colors_hex(&self, light: bool) -> Vec<String> {
        let defaults: Vec<String> = if light {
            ["#8c8c8c", "#595959", "#52c41a", "#d48806", "#cf1322", "#cf1322", "#262626"]
                .iter().map(|s| s.to_string()).collect()
        } else {
            ["#75757e", "#8c8c8c", "#95de64", "#ffc53d", "#ff7875", "#ff7875", "#d9d9d9"]
                .iter().map(|s| s.to_string()).collect()
        };
        let src = if light { &self.colors_light } else { &self.colors };
        let mut out = defaults;
        for (i, item) in out.iter_mut().enumerate() {
            if let Some(s) = src.get(i) {
                if !s.is_empty() {
                    *item = s.clone();
                }
            }
        }
        out
    }

    /// 指定主题的搜索高亮底色(hex)
    pub fn highlight_hex(&self, light: bool) -> String {
        if light {
            self.highlight_light.clone()
        } else {
            self.highlight_dark.clone()
        }
    }

    /// 检索命中的文字色(与高亮底色配对,保证对比度)
    pub fn highlight_fg_hex(&self, light: bool) -> &'static str {
        if light {
            "#10233f"
        } else {
            "#ffffff"
        }
    }

    /// 动作当前生效的快捷键规格(用户未改过时取默认)
    pub fn shortcut(&self, action: &str) -> String {
        self.shortcuts
            .get(action)
            .cloned()
            .unwrap_or_else(|| default_shortcut(action).to_string())
    }

    pub fn enabled_count(&self) -> usize {
        self.configs.iter().filter(|c| c.enabled).count()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_ms: 1000,
            max_lines: 200_000,
            initial_mb: 4,
            tail_mb: 2,
            show_line_numbers: true,
            show_timestamps: false,
            ui_font_size: default_ui_font_size(),
            log_font_size: default_log_font_size(),
            ui_font_family: String::new(),
            log_font_family: String::new(),
            wrap: true,
            colors: [
                "#75757e", "#8c8c8c", "#95de64", "#ffc53d", "#ff7875", "#ff7875", "#d9d9d9",
            ].iter().map(|s| s.to_string()).collect(),
            colors_light: [
                "#8c8c8c", "#595959", "#52c41a", "#d48806", "#cf1322", "#cf1322", "#262626",
            ].iter().map(|s| s.to_string()).collect(),
            // 与级别色(灰/绿/玻珀/红)拉开色相:藕蓝底 + 白字加粗,暗色下对比充分
            highlight_dark: "#3b5bdb".to_string(),
            highlight_light: "#9db4ff".to_string(),
            primary_color: String::new(),
            configs: builtin_configs(),
            recents: Vec::new(),
            ssh_hosts: Vec::new(),
            recents_limit: default_recents_limit(),
            recents_purge_creds: default_true(),
            shortcuts: Default::default(),
            cred_notice: None,
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("logview").join("settings.json"))
}

pub fn load() -> Settings {
    let Some(p) = config_path() else {
        return Settings::default();
    };
    let mut s: Settings = match std::fs::read_to_string(&p) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Settings::default(),
    };
    // 迁移:新版本新增的内置配置(按 id 判重)追加到已有配置之后
    for b in builtin_configs() {
        if !s.configs.iter().any(|c| c.id == b.id) {
            s.configs.push(b);
        }
    }
    migrate_secrets(&mut s);
    migrate_highlight(&mut s);
    s
}

/// 旧默认高亮色对比不足(紫罗兰)或与 WARN 级别色过近(琥珀);
/// 仅当两项都还是旧默认值时替换为新默认,避免覆盖用户自选色。
fn migrate_highlight(s: &mut Settings) {
    let old = matches!(
        (s.highlight_dark.as_str(), s.highlight_light.as_str()),
        ("#e8a33d", "#ffd54f") | ("#4a3f7a", "#e6ddff")
    );
    if old {
        let d = Settings::default();
        s.highlight_dark = d.highlight_dark;
        s.highlight_light = d.highlight_light;
    }
}

/// 把旧版设置文件里的明文凭据迁移到系统钥匙串,并立刻把文件改写为不含秘密的形态。
/// 钥匙串不可用时同样移除明文,退回「仅内存 + 手动输入」,避免秘密长期留在磁盘。
fn migrate_secrets(s: &mut Settings) {
    let mut dirty = false;
    for r in &mut s.recents {
        if !r.params.has_secret() {
            continue;
        }
        // 私钥正文无法安全放入钥匙串(Windows 单条 2560 字节上限),保留原样提示用户改用文件
        if matches!(&r.params, SourceParams::Ssh { auth: SshAuth::KeyText { .. }, .. }) {
            s.cred_notice =
                Some("存在以私钥正文保存的连接,无法迁移到钥匙串;建议改用私钥文件".to_string());
            continue;
        }
        let Some(key) = crate::secrets::key_of(&r.params) else {
            continue;
        };
        let secret = crate::secrets::Secret::from_params(&r.params);
        if secret.is_empty() {
            continue;
        }
        match crate::secrets::save(&key, &secret) {
            Ok(()) => r.remembered = true,
            Err(e) => {
                r.remembered = false;
                s.cred_notice = Some(format!("{e};原明文凭据已移除,连接时需重新输入"));
            }
        }
        r.params = r.params.sanitized();
        dirty = true;
    }
    if dirty {
        save(s);
    }
}

pub fn save(s: &Settings) {
    let Some(p) = config_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(p, txt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_host_port_defaults_to_22() {
        let h: SavedHost = serde_json::from_str(r#"{"host":"h","user":"u"}"#).unwrap();
        assert_eq!(h.port, 22);
        assert!(h.name.is_empty());
    }

    #[test]
    fn settings_without_ssh_hosts_still_loads() {
        // 旧版设置文件没有 ssh_hosts 字段,必须能反序列化(空列表)
        let mut v = serde_json::to_value(Settings::default()).unwrap();
        v.as_object_mut().unwrap().remove("ssh_hosts");
        let s: Settings = serde_json::from_value(v).unwrap();
        assert!(s.ssh_hosts.is_empty());
    }

    #[test]
    fn settings_without_show_timestamps_defaults_off() {
        // 旧设置文件没有该字段:时间戳默认不开启
        let mut v = serde_json::to_value(Settings::default()).unwrap();
        v.as_object_mut().unwrap().remove("show_timestamps");
        let s: Settings = serde_json::from_value(v).unwrap();
        assert!(!s.show_timestamps);
        assert!(!Settings::default().show_timestamps);
    }

    #[test]
    fn recents_settings_fall_back_to_defaults() {
        // 旧设置文件没有这两个字段:条数默认 50、删除 URL 记录时清凭据默认开启
        let mut v = serde_json::to_value(Settings::default()).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("recents_limit");
        obj.remove("recents_purge_creds");
        let s: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(s.recents_limit, 50);
        assert!(s.recents_purge_creds);
    }

    #[test]
    fn legacy_highlight_defaults_are_migrated() {
        // 紫罗兰旧默认对比不足 → 迁移;用户自选色保持不动
        let mut s = Settings::default();
        s.highlight_dark = "#4a3f7a".into();
        s.highlight_light = "#e6ddff".into();
        migrate_highlight(&mut s);
        let d = Settings::default();
        assert_eq!(s.highlight_dark, d.highlight_dark);
        assert_eq!(s.highlight_light, d.highlight_light);

        s.highlight_dark = "#123456".into();
        s.highlight_light = "#fedcba".into();
        migrate_highlight(&mut s);
        assert_eq!(s.highlight_dark, "#123456");
        assert_eq!(s.highlight_light, "#fedcba");
    }

    #[test]
    fn shortcuts_default_override_and_reset() {
        let mut s = Settings::default();
        assert_eq!(s.shortcut("new_tab"), "secondary+t");
        s.shortcuts.insert("new_tab".into(), "secondary+alt+t".into());
        assert_eq!(s.shortcut("new_tab"), "secondary+alt+t");
        s.shortcuts.remove("new_tab");
        assert_eq!(s.shortcut("new_tab"), "secondary+t");
    }

    #[test]
    fn default_shortcuts_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (a, _) in SHORTCUT_ACTIONS {
            assert!(seen.insert(default_shortcut(a)), "重复绑定: {a}");
        }
    }
}
