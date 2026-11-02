//! M7 配置管理 ConfigStore（对应设计文档 4.7、7.4）。
//!
//! `config.json` 位置：Windows `%APPDATA%\MoNote\config.json`；
//! macOS `~/Library/Application Support/MoNote/config.json`；
//! Linux `~/.config/MoNote/config.json`。
//! 配置损坏时载入默认值并保留 `.bak`（X-13）。
//!
//! 约束：
//! - 写入一律临时文件 + rename 原子替换，避免半截 JSON；
//! - `patch` 为**深合并**：只更新前端给出的字段，未知字段忽略；
//! - 密码不在本文件（NFR-108，凭据在 OS 凭据管理器，见 `core::sync`）。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{code, AppError, Result};
use crate::types::AppConfig;

/// 配置存储。
#[derive(Debug, Clone)]
pub struct ConfigStore {
    pub path: PathBuf,
    pub config: AppConfig,
}

impl ConfigStore {
    /// 默认配置路径（按平台约定）。
    pub fn default_path() -> PathBuf {
        platform_config_dir().join("config.json")
    }

    /// 从默认路径加载（不存在/损坏时返回默认配置）。
    pub fn load_default() -> Self {
        Self::load(Self::default_path())
    }

    /// 从指定路径加载；损坏时备份为 `.bak` 并返回默认配置。
    ///
    /// - 文件不存在 → 默认配置（并创建配置目录，但不立刻写盘，避免“只读环境”启动失败）；
    /// - JSON 损坏/字段类型非法 → 原文件复制为 `config.json.bak`，使用默认值（X-13）；
    /// - 读取失败（权限等）→ 使用默认值，不阻断启动。
    pub fn load(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                // 创建配置目录是 best-effort：失败时仍可用默认配置运行（可能只读环境）。
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let config = match std::fs::read_to_string(&path) {
            Ok(text) => {
                // 兼容用户用记事本等编辑器保存时写入的 UTF-8 BOM（Windows 常见）。
                let cleaned = text.strip_prefix('\u{feff}').unwrap_or(&text);
                match serde_json::from_str::<AppConfig>(cleaned) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        eprintln!("[MoNote] config.json 解析失败，已使用默认配置并保留 .bak：{e}");
                        if let Err(err) = backup_corrupt_config(&path) {
                            eprintln!("[MoNote] 保留损坏配置副本失败：{err}");
                        }
                        AppConfig::default()
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
            Err(e) => {
                eprintln!("[MoNote] 读取 config.json 失败，使用默认配置：{e}");
                AppConfig::default()
            }
        };
        ConfigStore { path, config }
    }

    /// 原子保存（临时文件 + rename）。
    pub fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.config).map_err(AppError::from)?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::io_ctx(format!("创建配置目录失败：{}", parent.display()), &e)
                })?;
            }
        }
        atomic_write(&self.path, &bytes)
    }

    /// 深合并 patch（前端只传变更字段），保存并返回新配置。
    ///
    /// 合并规则：对象递归合并、其余类型直接替换；`null` 可清空 Option 字段
    /// （如 `{"vault":{"root":null}}`）。未知字段在反序列化时被忽略（不写入磁盘）。
    /// 若 patch 中含有类型非法的字段（如 `zoom: "big"`），返回错误且不改变当前配置。
    pub fn patch(&mut self, patch: &serde_json::Value) -> Result<AppConfig> {
        let patch_obj = patch
            .as_object()
            .ok_or_else(|| AppError::invalid_arg("配置补丁必须是 JSON 对象"))?;
        let mut merged = serde_json::to_value(&self.config).map_err(AppError::from)?;
        deep_merge(&mut merged, &serde_json::Value::Object(patch_obj.clone()));
        let next: AppConfig = serde_json::from_value(merged)
            .map_err(|e| AppError::new(code::INVALID_ARG, "配置字段类型非法，已忽略本次修改").with_detail(e.to_string()))?;
        // 先落盘再更新内存：写盘失败时内存配置保持旧值，避免 UI 与实际磁盘不一致。
        let next_store = ConfigStore {
            path: self.path.clone(),
            config: next,
        };
        next_store.save()?;
        self.config = next_store.config;
        Ok(self.config.clone())
    }

    /// 设置库根路径并保存。
    pub fn set_vault_root(&mut self, root: &str) -> Result<()> {
        let trimmed = root.trim();
        self.config.vault.root = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self.save()
    }
}

/// 平台配置目录（不存在时创建）。
pub fn platform_config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("MoNote")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("MoNote")
    } else {
        Path::new(".").join("MoNote-config")
    }
}

// ===========================================================================
// 私有辅助
// ===========================================================================

/// 深合并：`base` 原地更新；两值均为对象时递归，否则用 `patch` 覆盖。
fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
            for (key, patch_val) in patch_map {
                match base_map.get_mut(key) {
                    Some(slot) => deep_merge(slot, patch_val),
                    None => {
                        base_map.insert(key.clone(), patch_val.clone());
                    }
                }
            }
        }
        (base_slot, patch_val) => *base_slot = patch_val.clone(),
    }
}

/// 损坏配置保留副本：`config.json` → `config.json.bak`（覆盖旧 .bak）。
fn backup_corrupt_config(path: &Path) -> std::io::Result<()> {
    let mut dest = path.as_os_str().to_owned();
    dest.push(".bak");
    std::fs::copy(path, PathBuf::from(dest)).map(|_| ())
}

/// 原子写：同目录临时文件 + fsync + rename；失败清理临时文件。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::invalid_arg("配置路径无效"))?;
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("写配置文件失败：{}", path.display()), &e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("替换配置文件失败：{}", path.display()), &e));
    }
    Ok(())
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_config_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("MoNote").join("config.json")
    }

    #[test]
    fn load_missing_file_returns_defaults_and_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        let store = ConfigStore::load(path.clone());
        assert_eq!(store.config.version, 1);
        assert_eq!(store.config.ui.theme, "auto");
        assert_eq!(store.config.backup.keep, 5);
        assert!(store.config.update.auto_check);
        assert!(store.config.hotkeys.contains_key("command.palette"));
        assert!(!path.exists(), "加载时不应立刻写盘");
        assert!(path.parent().unwrap().exists(), "应创建配置目录");
    }

    #[test]
    fn load_corrupt_json_backs_up_and_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = ConfigStore::load(path.clone());
        assert_eq!(store.config.ui.theme, "auto", "损坏配置回退默认值");
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(bak.exists(), "损坏配置应保留 .bak");
        let bak_text = std::fs::read_to_string(&bak).unwrap();
        assert_eq!(bak_text, "{ this is not json");
        // 损坏配置可被修复覆盖
        store.save().unwrap();
        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.config.version, 1);
    }

    #[test]
    fn load_utf8_bom_config_is_accepted() {
        // Windows 记事本保存的 config.json 可能带 UTF-8 BOM，不应被当损坏配置。
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let json = serde_json::to_string_pretty(&AppConfig::default()).unwrap();
        std::fs::write(&path, format!("\u{feff}{json}")).unwrap();
        let store = ConfigStore::load(path.clone());
        assert_eq!(store.config.version, 1, "BOM 配置应正常解析");
        assert!(!PathBuf::from(format!("{}.bak", path.display())).exists(), "不应误判为损坏配置");
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        let mut store = ConfigStore::load(path.clone());
        store.config.ui.theme = "dark".into();
        store.config.backup.keep = 8;
        store.save().unwrap();
        assert!(path.exists());
        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.config.ui.theme, "dark");
        assert_eq!(reloaded.config.backup.keep, 8);
    }

    #[test]
    fn patch_deep_merges_nested_objects() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        let mut store = ConfigStore::load(path.clone());
        let patch = serde_json::json!({
            "ui": { "theme": "light", "zoom": 1.25 },
            "editor": { "autosaveDebounceMs": 500 },
            "backup": { "keep": 7 },
            "sync": { "url": "https://dav.example.com/monote/", "username": "u1" },
            "update": { "autoCheck": false },
            "hotkeys": { "note.new": "Ctrl+Alt+N" },
            "onboardingDone": true
        });
        let updated = store.patch(&patch).unwrap();
        assert_eq!(updated.ui.theme, "light");
        assert_eq!(updated.ui.zoom, 1.25);
        assert_eq!(updated.ui.language, "zh-CN", "未给出的字段保持原值");
        assert_eq!(updated.editor.autosave_debounce_ms, 500);
        assert_eq!(updated.backup.keep, 7);
        assert_eq!(updated.sync.url, "https://dav.example.com/monote/");
        assert_eq!(updated.sync.username, "u1");
        assert!(!updated.update.auto_check);
        assert_eq!(updated.hotkeys.get("note.new").map(String::as_str), Some("Ctrl+Alt+N"));
        assert_eq!(
            updated.hotkeys.get("search.full").map(String::as_str),
            Some("Ctrl+Shift+F"),
            "未覆盖的快捷键保持默认"
        );
        assert!(updated.onboarding_done);

        // 深合并结果已持久化
        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.config.ui.zoom, 1.25);
        assert_eq!(reloaded.config.hotkeys.get("note.new").map(String::as_str), Some("Ctrl+Alt+N"));
    }

    #[test]
    fn patch_ignores_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ConfigStore::load(tmp_config_path(&dir));
        let updated = store
            .patch(&serde_json::json!({ "unknownField": true, "ui": { "theme": "dark", "mystery": 1 } }))
            .unwrap();
        assert_eq!(updated.ui.theme, "dark");
        let text = std::fs::read_to_string(&store.path).unwrap();
        assert!(!text.contains("unknownField"), "未知字段不应写入磁盘");
        assert!(!text.contains("mystery"));
    }

    #[test]
    fn patch_invalid_value_rejected_without_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ConfigStore::load(tmp_config_path(&dir));
        let before = store.config.ui.theme.clone();
        let err = store.patch(&serde_json::json!({ "ui": { "zoom": "huge" } })).unwrap_err();
        assert_eq!(err.code, code::INVALID_ARG);
        assert_eq!(store.config.ui.theme, before);
        assert_eq!(store.config.ui.zoom, 1.0, "非法值不改变配置");
        assert!(store.patch(&serde_json::json!([1, 2, 3])).is_err(), "非对象补丁应报错");
    }

    #[test]
    fn patch_can_clear_optional_field_with_null() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ConfigStore::load(tmp_config_path(&dir));
        store.set_vault_root("E:\\Notes").unwrap();
        assert_eq!(store.config.vault.root.as_deref(), Some("E:\\Notes"));
        store.patch(&serde_json::json!({ "vault": { "root": null } })).unwrap();
        assert!(store.config.vault.root.is_none());
    }

    #[test]
    fn set_vault_root_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_config_path(&dir);
        let mut store = ConfigStore::load(path.clone());
        store.set_vault_root("E:\\MyNotes\\中文库").unwrap();
        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.config.vault.root.as_deref(), Some("E:\\MyNotes\\中文库"));
        // 空串清除
        let mut store2 = reloaded;
        store2.set_vault_root("   ").unwrap();
        let reloaded2 = ConfigStore::load(store2.path.clone());
        assert!(reloaded2.config.vault.root.is_none());
    }

    #[test]
    fn default_path_ends_with_config_json() {
        let p = ConfigStore::default_path();
        assert_eq!(p.file_name().and_then(|f| f.to_str()), Some("config.json"));
    }
}
