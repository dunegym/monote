//! 全局状态与服务编排层（对应设计文档 3.2 L2/L3 之间的门面）。
//!
//! `AppState` 持有：配置、当前库 Vault、派生索引 SearchIndex、通知中心、修订号表、
//! 自写白名单（ExternalGuard）与文件 watcher。IPC 层只做参数解析与 envelope 包装，
//! 业务编排集中在本模块，保证"任何层不绕过 M2 直接写文件"。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::core;
use crate::core::config::ConfigStore;
use crate::core::index::SearchIndex;
use crate::core::notify::NotificationCenter;
use crate::core::vault::Vault;
use crate::error::{AppError, Result};
use crate::types::*;

/// 应用全局状态（Tauri managed state）。
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Mutex<Inner>>,
    pub app: Option<AppHandle>,
    pub watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

/// 受同一把锁保护的核心状态。
pub struct Inner {
    pub config: ConfigStore,
    pub vault: Option<Vault>,
    pub index: Option<SearchIndex>,
    pub notify: NotificationCenter,
    /// 库内相对路径 → 内存修订号（乐观并发控制，vault.write 的 expectRev）
    pub revs: HashMap<String, u64>,
    /// 自写白名单：路径 → 内容 hash（ExternalGuard，F-04）
    pub self_writes: HashMap<String, String>,
    pub index_building: bool,
    pub index_progress_done: u32,
    pub index_progress_total: u32,
    pub degraded: bool,
    pub last_sync: Option<String>,
}

impl AppState {
    /// 创建状态：加载默认配置（不打开库，打开库在 [`AppState::start`]）。
    pub fn new() -> Self {
        Self::new_with_config(ConfigStore::default_path())
    }

    /// 创建状态并指定 config.json 路径（集成测试用，避免读写真实用户配置）。
    pub fn new_with_config(config_path: std::path::PathBuf) -> Self {
        let config = ConfigStore::load(config_path);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                vault: None,
                index: None,
                notify: NotificationCenter::new(),
                revs: HashMap::new(),
                self_writes: HashMap::new(),
                index_building: false,
                index_progress_done: 0,
                index_progress_total: 0,
                degraded: false,
                last_sync: None,
            })),
            app: None,
            watcher: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_app(&mut self, app: AppHandle) {
        self.app = Some(app);
    }

    /// 向前端推送事件（失败静默：窗口可能尚未加载）。
    pub fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Some(app) = &self.app {
            let _ = app.emit(event, payload);
        }
    }

    /// 发布通知：入通知中心 + 推送 `notify:new`。
    pub fn notify(&self, level: &str, title: &str, detail: Option<String>) -> NotificationItem {
        let mut inner = self.inner.lock();
        let item = inner.notify.publish(level, title, detail);
        drop(inner);
        self.emit("notify:new", json!(item));
        item
    }

    /// 启动流程：清理残留 → 打开上次库 → 启动 watcher → 后台巡检/备份/更新检查。
    pub fn start(&self) {
        // 1) 清理崩溃残留临时文件（库已存在时）
        if let Some(root) = self.inner.lock().config.config.vault.root.clone() {
            let p = PathBuf::from(&root);
            if p.exists() {
                let state = self.clone();
                std::thread::spawn(move || {
                    if let Err(e) = state.open_vault_internal(&p) {
                        state.notify("error", "打开上次的笔记库失败", Some(e.to_string()));
                    }
                });
            }
        }
        // 2) 后台任务（备份 / 更新检查）
        let state = self.clone();
        std::thread::spawn(move || state.background_maintenance());
    }

    // =====================================================================
    // 库生命周期
    // =====================================================================

    /// 打开/切换库（IPC `vault_open`）。切换前由前端负责 flush 当前编辑内容。
    pub fn open_vault(&self, root: &Path) -> Result<OpenedVault> {
        self.open_vault_internal(root)
    }

    fn open_vault_internal(&self, root: &Path) -> Result<OpenedVault> {
        let mut inner = self.inner.lock();
        let vault = Vault::open(root)?;
        vault.cleanup_temp_files()?;
        let _ = vault.purge_expired_trash();

        // 打开/重建派生索引
        let db_path = vault.root().join(".monote").join("index.db");
        let index = match SearchIndex::open(&db_path) {
            Ok(idx) => Some(idx),
            Err(e) => {
                // 索引损坏：改名保留现场，重建（X-2）
                let _ = std::fs::rename(&db_path, db_path.with_extension(format!(
                    "corrupt-{}.db",
                    chrono::Local::now().format("%Y%m%d%H%M%S")
                )));
                match SearchIndex::open(&db_path) {
                    Ok(idx) => Some(idx),
                    Err(e2) => {
                        inner.degraded = true;
                        inner.notify.publish(
                            "error",
                            "索引不可用，已降级为逐文件扫描",
                            Some(format!("{e}; {e2}")),
                        );
                        None
                    }
                }
            }
        };

        inner.degraded = index.is_none();
        inner.index = index;
        inner.vault = Some(vault.clone());
        inner.revs.clear();
        inner.self_writes.clear();
        inner.config.set_vault_root(&vault.root().to_string_lossy())?;

        // 全库对账（X-2 / F-04 启动兜底）
        let mut note_count = 0u32;
        match reconcile_index(&mut inner) {
            Ok(n) => note_count = n,
            Err(e) => {
                inner.degraded = true;
                inner.notify.publish("warn", "索引对账失败，已降级", Some(e.to_string()));
            }
        }
        let opened = OpenedVault {
            root: vault.root().to_string_lossy().to_string(),
            name: vault.name(),
            note_count,
            index_ok: inner.index.is_some(),
            index_building: false,
        };
        drop(inner);

        // 重启 watcher
        self.restart_watcher(&vault);
        self.emit("vault:changed", json!({ "reason": "opened" }));
        self.emit("vault:opened", json!(opened));
        Ok(opened)
    }

    fn restart_watcher(&self, vault: &Vault) {
        if let Some(app) = &self.app {
            let w = crate::watcher::start(app.clone(), self.inner.clone(), vault.root().to_path_buf());
            *self.watcher.lock() = w;
        }
    }

    // =====================================================================
    // 笔记读写（F-01）
    // =====================================================================

    pub fn vault_info(&self) -> Option<VaultInfo> {
        let inner = self.inner.lock();
        let vault = inner.vault.as_ref()?;
        let count = inner.index.as_ref().and_then(|i| i.note_count().ok()).unwrap_or(0);
        Some(VaultInfo { root: vault.root().to_string_lossy().to_string(), name: vault.name(), note_count: count })
    }

    pub fn list_tree(&self, depth: Option<usize>) -> Result<Vec<TreeNode>> {
        let inner = self.inner.lock();
        inner.require_vault()?.list_tree(depth)
    }

    pub fn read_note(&self, path: &str) -> Result<NoteRead> {
        let mut inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let (content, mtime) = vault.read_note_raw(path)?;
        let fm = core::links::read_meta(&content);
        let title = core::links::display_title(fm.title.as_deref(), path);
        let tags = merge_tags(&content);
        let rev = inner.revs.get(path).copied().unwrap_or(0);
        if let Some(index) = inner.index.as_mut() {
            let _ = index.touch_opened(path);
        }
        Ok(NoteRead {
            path: path.to_string(),
            title,
            content,
            rev,
            mtime,
            tags,
            created: fm.created,
            updated: fm.updated,
        })
    }

    pub fn write_note(&self, path: &str, content: &str, expect_rev: Option<u64>) -> Result<WriteResult> {
        let now_iso = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let (result, rev) = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let current_rev = inner.revs.get(path).copied().unwrap_or(0);
            if let Some(exp) = expect_rev {
                if exp != current_rev {
                    return Err(AppError::rev_conflict(path, exp, current_rev));
                }
            }
            let fallback = core::links::display_title(None, path);
            let new_content = core::links::touch_front_matter(content, &fallback, &now_iso);
            let hash = core::links::content_hash(&new_content);
            let mtime = vault.write_atomic(path, new_content.as_bytes())?;
            inner.self_writes.insert(path.to_string(), hash.clone());
            let size = new_content.len() as u64;
            let parsed = core::links::parse_note(path, &new_content, mtime, size);
            if let Some(index) = inner.index.as_mut() {
                if let Err(e) = index.reindex_note(&parsed) {
                    inner.notify.publish("warn", "索引更新失败", Some(e.to_string()));
                }
            }
            let rev = current_rev + 1;
            inner.revs.insert(path.to_string(), rev);
            let result = WriteResult {
                path: path.to_string(),
                rev,
                mtime,
                title: parsed.title.clone(),
                tags: parsed.tags.clone(),
                size,
            };
            (result, rev)
        };
        self.emit("notes:written", json!({ "path": path, "rev": rev }));
        self.emit("vault:changed", json!({ "reason": "written" }));
        Ok(result)
    }

    pub fn create_note(&self, name: Option<&str>, parent: Option<&str>) -> Result<CreateResult> {
        let now_iso = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let (res, title) = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let raw_name = name.unwrap_or("未命名笔记").trim();
            let base = if raw_name.is_empty() { "未命名笔记" } else { raw_name };
            let file_name = format!("{}.md", core::links::safe_file_name(base));
            let rel = match parent {
                Some(p) if !p.is_empty() => format!("{}/{}", p.trim_matches('/'), file_name),
                _ => file_name,
            };
            let rel = unique_note_path(&vault, &rel)?;
            let content = core::links::touch_front_matter("", &core::links::display_title(None, &rel), &now_iso);
            vault.create_note(&rel, &content)?;
            let mtime = vault.read_note_raw(&rel)?.1;
            let hash = core::links::content_hash(&content);
            inner.self_writes.insert(rel.clone(), hash);
            let parsed = core::links::parse_note(&rel, &content, mtime, content.len() as u64);
            if let Some(index) = inner.index.as_mut() {
                let _ = index.reindex_note(&parsed);
            }
            (CreateResult { path: rel.clone(), existed: false }, parsed.title)
        };
        self.emit("vault:changed", json!({ "reason": "created", "name": title }));
        Ok(res)
    }

    pub fn create_folder(&self, path: &str) -> Result<CreateResult> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let rel = path.trim_matches('/').to_string();
        if rel.is_empty() {
            return Err(AppError::invalid_arg("文件夹名不能为空"));
        }
        vault.create_folder(&rel)?;
        Ok(CreateResult { path: rel, existed: false })
    }

    /// FR-203：缺失双链目标时创建同名空白笔记（已存在则直接返回）。
    pub fn create_empty_note(&self, name: &str) -> Result<CreateResult> {
        let now_iso = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let (res, existed, trash_conflict) = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let clean = core::links::normalize_target(name);
            if clean.is_empty() {
                return Err(AppError::invalid_arg("笔记名为空"));
            }
            let all = vault.find_by_name(&clean)?;
            let has_trash = all.iter().any(|p| p.starts_with(".trash"));
            let mut candidates: Vec<String> = all
                .into_iter()
                .filter(|p| !p.starts_with(".trash") && vault.abs_path(p).map(|a| a.exists()).unwrap_or(false))
                .collect();
            if candidates.is_empty() {
                let rel = format!("{}.md", core::links::safe_file_name(&clean));
                let rel = unique_note_path(&vault, &rel)?;
                let content =
                    core::links::touch_front_matter("", &core::links::display_title(None, &rel), &now_iso);
                vault.create_note(&rel, &content)?;
                let mtime = vault.read_note_raw(&rel)?.1;
                let hash = core::links::content_hash(&content);
                inner.self_writes.insert(rel.clone(), hash);
                let parsed = core::links::parse_note(&rel, &content, mtime, content.len() as u64);
                if let Some(index) = inner.index.as_mut() {
                    let _ = index.reindex_note(&parsed);
                }
                (CreateResult { path: rel, existed: false }, false, has_trash)
            } else {
                candidates.sort_by_key(|p| p.matches('/').count());
                (CreateResult { path: candidates.remove(0), existed: true }, true, false)
            }
        };
        if !existed {
            self.emit("vault:changed", json!({ "reason": "created" }));
            if trash_conflict {
                self.notify(
                    "info",
                    "回收站中存在同名笔记",
                    Some("已新建同名笔记；如欲恢复旧版本，请到回收站还原后重命名。".into()),
                );
            }
        }
        Ok(res)
    }

    pub fn rename(&self, path: &str, new_path: &str, rewrite_links: bool) -> Result<RenameResult> {
        let (result, rewritten) = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            vault.rename_path(path, new_path)?;

            // 索引：删除旧路径条目、按新路径重索引
            let old_title = core::links::display_title(None, path);
            let new_title = core::links::display_title(None, new_path);
            if let Some(index) = inner.index.as_mut() {
                let _ = index.remove_note(path);
            }
            if new_path.ends_with(".md") {
                if let Ok((content, mtime)) = vault.read_note_raw(new_path) {
                    let hash = core::links::content_hash(&content);
                    inner.self_writes.insert(new_path.to_string(), hash);
                    let parsed = core::links::parse_note(new_path, &content, mtime, content.len() as u64);
                    if let Some(index) = inner.index.as_mut() {
                        let _ = index.reindex_note(&parsed);
                    }
                }
            } else {
                // 文件夹重命名：重索引其下所有笔记
                let _ = reconcile_index(&mut inner);
            }
            if let Some(r) = inner.revs.remove(path) {
                inner.revs.insert(new_path.to_string(), r);
            }

            // 链接改写（仅库内笔记，DD-10）
            let mut rewritten = Vec::new();
            if rewrite_links && old_title != new_title {
                let files = vault.scan_notes()?;
                for f in files {
                    if f.rel == new_path {
                        continue;
                    }
                    if let Ok((content, mtime)) = vault.read_note_raw(&f.rel) {
                        let (new_content, count) =
                            core::links::rewrite_links(&content, &old_title, &new_title);
                        if count > 0 {
                            let hash = core::links::content_hash(&new_content);
                            if let Ok(new_mtime) = vault.write_atomic(&f.rel, new_content.as_bytes()) {
                                inner.self_writes.insert(f.rel.clone(), hash);
                                let parsed = core::links::parse_note(
                                    &f.rel,
                                    &new_content,
                                    new_mtime,
                                    new_content.len() as u64,
                                );
                                if let Some(index) = inner.index.as_mut() {
                                    let _ = index.reindex_note(&parsed);
                                }
                                let _ = mtime;
                                rewritten.push(RewriteCount { file: f.rel.clone(), count });
                            }
                        }
                    }
                }
            }
            let result = RenameResult {
                path: path.to_string(),
                new_path: new_path.to_string(),
                rewritten: rewritten.clone(),
            };
            (result, rewritten)
        };
        let _ = rewritten;
        self.emit("vault:changed", json!({ "reason": "renamed", "path": path, "newPath": new_path }));
        Ok(result)
    }

    pub fn trash(&self, path: &str) -> Result<TrashResult> {
        let ids = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let ids = vault.trash_path(path)?;
            if let Some(index) = inner.index.as_mut() {
                let _ = index.set_deleted(path, true);
            }
            inner.revs.remove(path);
            ids
        };
        let trash_id = ids.first().cloned().unwrap_or_default();
        self.emit("vault:changed", json!({ "reason": "trashed" }));
        Ok(TrashResult { trash_id })
    }

    pub fn trash_list(&self) -> Result<TrashList> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let items = vault.list_trash()?;
        let total = items.len();
        Ok(TrashList { items, total })
    }

    pub fn trash_restore(&self, trash_id: &str) -> Result<RestoreResult> {
        let (path, renamed_to) = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let (path, renamed) = vault.restore_trash(trash_id)?;
            if let Ok((content, mtime)) = vault.read_note_raw(&path) {
                let hash = core::links::content_hash(&content);
                inner.self_writes.insert(path.clone(), hash);
                let parsed = core::links::parse_note(&path, &content, mtime, content.len() as u64);
                if let Some(index) = inner.index.as_mut() {
                    let _ = index.set_deleted(&path, false);
                    let _ = index.reindex_note(&parsed);
                }
            }
            (path, renamed)
        };
        self.emit("vault:changed", json!({ "reason": "restored" }));
        Ok(RestoreResult { path, renamed_to })
    }

    pub fn empty_trash(&self) -> Result<EmptyTrashResult> {
        let cleared = {
            let inner = self.inner.lock();
            inner.require_vault()?.empty_trash()?
        };
        self.emit("vault:changed", json!({ "reason": "trash-emptied" }));
        Ok(EmptyTrashResult { cleared })
    }

    // =====================================================================
    // 附件 / 链接 / 标签
    // =====================================================================

    pub fn save_attachment(&self, file_name: &str, data_base64: &str) -> Result<serde_json::Value> {
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_base64)
            .map_err(|e| AppError::invalid_arg(format!("附件 base64 解码失败: {e}")))?;
        if bytes.len() > 10 * 1024 * 1024 {
            return Err(AppError::invalid_arg("图片超过 10 MB 限制"));
        }
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let path = vault.save_attachment(file_name, &bytes)?;
        Ok(json!({ "path": path }))
    }

    pub fn image_data(&self, path: &str) -> Result<serde_json::Value> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let bytes = vault.read_file_bytes(path)?;
        let mime = mime_for(path);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        Ok(json!({ "dataUrl": format!("data:{mime};base64,{encoded}") }))
    }

    pub fn suggest(&self, prefix: &str) -> Result<Vec<SuggestItem>> {
        let inner = self.inner.lock();
        if let Some(index) = inner.index.as_ref() {
            match index.suggest(prefix, 10) {
                Ok(items) => return Ok(items),
                Err(_) => {}
            }
        }
        // 降级：文件名扫描
        let vault = inner.require_vault()?;
        let mut items: Vec<SuggestItem> = vault
            .name_index()?
            .into_iter()
            .filter(|(title, _, _)| title.to_lowercase().contains(&prefix.to_lowercase()))
            .map(|(name, path, mtime)| SuggestItem { name, path, mtime })
            .collect();
        items.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        items.truncate(10);
        Ok(items)
    }

    pub fn backlinks(&self, path: &str) -> Result<Vec<BacklinkItem>> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let (content, _) = vault.read_note_raw(path)?;
        let fm = core::links::read_meta(&content);
        let mut names = vec![core::links::display_title(fm.title.as_deref(), path)];
        names.push(core::links::normalize_target(
            Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default().as_str(),
        ));
        names.sort();
        names.dedup();
        let index = match inner.index.as_ref() {
            Some(i) => i,
            None => return Ok(Vec::new()),
        };
        let rows = index.backlinks(&names)?;
        let items = rows
            .into_iter()
            .map(|r| BacklinkItem {
                src_path: r.src_path,
                src_title: r.src_title,
                line: r.line,
                context: context_around_line(&r.src_content, r.line, 80),
            })
            .collect();
        Ok(items)
    }

    pub fn tags(&self) -> Result<Vec<TagCount>> {
        let inner = self.inner.lock();
        if let Some(index) = inner.index.as_ref() {
            if let Ok(tags) = index.tags() {
                return Ok(tags);
            }
        }
        Ok(Vec::new())
    }

    // =====================================================================
    // 检索（F-03）
    // =====================================================================

    pub fn search(
        &self,
        kind: &str,
        term: &str,
        filters: SearchFilters,
        sort: &str,
        page: usize,
        page_size: usize,
    ) -> Result<SearchResult> {
        let inner = self.inner.lock();
        if let Some(index) = inner.index.as_ref() {
            let r = if kind == "title" {
                index.title_search(term, &filters, page, page_size)
            } else {
                index.full_search(term, &filters, sort, page, page_size)
            };
            match r {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // 查询失败：降级为逐文件扫描
                    let vault = inner.require_vault()?;
                    return scan_search(&vault, kind, term, &filters, sort, page, page_size)
                        .map_err(|_| e);
                }
            }
        }
        let vault = inner.require_vault()?;
        scan_search(&vault, kind, term, &filters, sort, page, page_size)
    }

    /// 全量重建索引（后台线程，进度经 `index:progress` 事件推送）。
    pub fn rebuild_index(&self) -> Result<serde_json::Value> {
        let state = self.clone();
        let job_id = uuid::Uuid::new_v4().to_string();
        {
            let mut inner = self.inner.lock();
            inner.require_vault()?;
            if inner.index_building {
                return Err(AppError::invalid_arg("索引重建已在进行中"));
            }
            inner.index_building = true;
            inner.index_progress_done = 0;
            inner.index_progress_total = 0;
        }
        std::thread::spawn(move || {
            let result = state.rebuild_index_job();
            let mut inner = state.inner.lock();
            inner.index_building = false;
            if let Err(e) = &result {
                inner.notify.publish("error", "索引重建失败", Some(e.to_string()));
            }
            drop(inner);
            state.emit("index:progress", json!({ "done": 0, "total": 0, "phase": "done" }));
            if result.is_err() {
                state.notify("error", "索引重建失败", result.err().map(|e| e.to_string()));
            } else {
                state.notify("info", "索引重建完成", None);
            }
        });
        Ok(json!({ "jobId": job_id }))
    }

    fn rebuild_index_job(&self) -> Result<()> {
        let vault = {
            let inner = self.inner.lock();
            inner.require_vault()?
        };
        let mut index = {
            let mut inner = self.inner.lock();
            inner
                .index
                .take()
                .ok_or_else(|| AppError::index_degraded("索引不可用"))?
        };
        let outcome = (|| -> Result<()> {
            index.clear()?;
            let files = vault.scan_notes()?;
            let total = files.len() as u32;
            for (i, f) in files.iter().enumerate() {
                let (content, mtime) = vault.read_note_raw(&f.rel)?;
                let parsed = core::links::parse_note(&f.rel, &content, mtime, content.len() as u64);
                index.reindex_note(&parsed)?;
                {
                    let mut inner = self.inner.lock();
                    inner.index_progress_done = i as u32 + 1;
                    inner.index_progress_total = total;
                }
                if i % 50 == 0 || i + 1 == files.len() {
                    self.emit(
                        "index:progress",
                        json!({ "done": i as u32 + 1, "total": total, "phase": "rebuild" }),
                    );
                }
            }
            Ok(())
        })();
        // 无论成功失败都把索引放回 Inner，避免索引丢失（X-2 恢复路径）
        let mut inner = self.inner.lock();
        inner.index = Some(index);
        match outcome {
            Ok(()) => {
                inner.degraded = false;
                Ok(())
            }
            Err(e) => {
                inner.degraded = true;
                Err(e)
            }
        }
    }

    pub fn index_status(&self) -> IndexStatus {
        let inner = self.inner.lock();
        let indexed = inner.index.as_ref().and_then(|i| i.note_count().ok()).unwrap_or(0);
        IndexStatus {
            building: inner.index_building,
            degraded: inner.degraded,
            indexed,
            progress_done: inner.index_progress_done,
            progress_total: inner.index_progress_total,
            fts_available: inner.index.as_ref().map(|i| i.fts_available()).unwrap_or(false),
        }
    }

    // =====================================================================
    // 导入导出 / 备份
    // =====================================================================

    pub fn import(&self, src: &Path, target_dir: Option<&str>) -> Result<ImportReport> {
        let report = {
            let mut inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let target = target_dir.unwrap_or("");
            let report = core::io::import_path(&vault, src, target)?;
            let _ = reconcile_index(&mut inner);
            report
        };
        self.emit("vault:changed", json!({ "reason": "imported" }));
        Ok(report)
    }

    pub fn export_note(
        &self,
        path: &str,
        fmt: &str,
        out_path: Option<&Path>,
        rendered_html: Option<&str>,
    ) -> Result<ExportResult> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let (content, _) = vault.read_note_raw(path)?;
        let title = core::links::display_title(
            core::links::read_meta(&content).title.as_deref(),
            path,
        );
        match fmt {
            "md" => {
                let out = resolve_out(out_path, &format!("{title}.md"))?;
                core::io::export_note_md(&vault, path, &out)
            }
            "html" => {
                let html = rendered_html.ok_or_else(|| AppError::invalid_arg("缺少渲染后的 HTML"))?;
                let out = resolve_out(out_path, &format!("{title}.html"))?;
                core::io::export_note_html(&vault, path, &title, html, &out)
            }
            other => Err(AppError::invalid_arg(format!("不支持的导出格式: {other}"))),
        }
    }

    pub fn export_vault(&self, out_path: Option<&Path>) -> Result<ExportResult> {
        let inner = self.inner.lock();
        let vault = inner.require_vault()?;
        let out = resolve_out(out_path, &format!("{}-vault.zip", vault.name()))?;
        core::io::export_vault_zip(&vault, &out)
    }

    pub fn backup_now(&self) -> Result<BackupResult> {
        let (vault, backup_dir, keep) = {
            let inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let cfg = &inner.config.config.backup;
            (vault, PathBuf::from(&cfg.dir), cfg.keep)
        };
        let result = core::backup::run_backup(vault.root(), &backup_dir, keep)?;
        {
            let mut inner = self.inner.lock();
            inner.config.config.backup.last_backup_at =
                Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
            let _ = inner.config.save();
        }
        self.emit("backup:done", json!({ "file": result.file, "size": result.size }));
        Ok(result)
    }

    pub fn backup_list(&self) -> Result<Vec<BackupEntry>> {
        let inner = self.inner.lock();
        let dir = PathBuf::from(&inner.config.config.backup.dir);
        core::backup::list_backups(&dir)
    }

    // =====================================================================
    // 同步（F-05）
    // =====================================================================

    pub fn sync_config(&self) -> SyncConfigView {
        let inner = self.inner.lock();
        let cfg = &inner.config.config.sync;
        core::sync::config_view(&cfg.url, &cfg.username, cfg.auto_on_start, inner.last_sync.clone())
    }

    pub fn sync_configure(
        &self,
        url: &str,
        username: &str,
        password: Option<&str>,
        auto_on_start: bool,
    ) -> Result<SyncConfigView> {
        if !url.is_empty() && !url.starts_with("https://") {
            let _ = &url;
            // HTTP 仅警告不阻止（AC-031）：通过通知中心提示
            self.notify("warn", "WebDAV 使用非 HTTPS 地址", Some("建议改用 HTTPS 以保护凭据与笔记内容".into()));
        }
        if let Some(pwd) = password {
            if !pwd.is_empty() && !username.is_empty() {
                core::sync::set_password(username, pwd)?;
            }
        }
        {
            let mut inner = self.inner.lock();
            inner.config.config.sync.url = url.to_string();
            inner.config.config.sync.username = username.to_string();
            inner.config.config.sync.auto_on_start = auto_on_start;
            inner.config.save()?;
        }
        Ok(self.sync_config())
    }

    pub fn sync_run(&self) -> Result<SyncPlan> {
        let (vault_root, url, username) = {
            let inner = self.inner.lock();
            let vault = inner.require_vault()?;
            let cfg = inner.config.config.sync.clone();
            (vault.root().to_path_buf(), cfg.url, cfg.username)
        };
        if url.is_empty() || username.is_empty() {
            return Err(AppError::invalid_arg("尚未配置 WebDAV 服务器"));
        }
        let password = core::sync::get_password(&username)?
            .ok_or_else(|| AppError::network("未保存 WebDAV 密码，请在设置中重新输入"))?;
        self.emit("sync:progress", json!({ "phase": "start", "path": "" }));
        let plan = core::sync::run_sync(
            &vault_root,
            &url,
            &username,
            &password,
            chrono::Local::now(),
        )?;
        {
            let mut inner = self.inner.lock();
            inner.last_sync = Some(plan.last_run.clone());
            let _ = reconcile_index(&mut inner);
        }
        self.emit("vault:changed", json!({ "reason": "synced" }));
        Ok(plan)
    }

    // =====================================================================
    // 配置 / 通知 / 更新 / 快捷键 / 关于
    // =====================================================================

    pub fn get_config(&self) -> AppConfig {
        self.inner.lock().config.config.clone()
    }

    pub fn set_config(&self, patch: &serde_json::Value) -> Result<AppConfig> {
        let cfg = {
            let mut inner = self.inner.lock();
            inner.config.patch(patch)?
        };
        self.emit("config:changed", json!(cfg));
        Ok(cfg)
    }

    pub fn update_check(&self, manual: bool) -> Result<UpdateInfo> {
        let (cfg, now) = {
            let inner = self.inner.lock();
            (inner.config.config.update.clone(), chrono::Local::now())
        };
        let result = core::update::check_update(&cfg, manual, now)?;
        if result.checked {
            let mut inner = self.inner.lock();
            inner.config.config.update.last_check = result.checked_at.clone();
            let _ = inner.config.save();
        }
        if result.has_update {
            self.notify(
                "info",
                &format!("发现新版本 {}", result.latest.clone().unwrap_or_default()),
                result.notes.clone(),
            );
        }
        Ok(result)
    }

    pub fn notify_list(&self, limit: Option<usize>) -> NotificationList {
        self.inner.lock().notify.list(limit)
    }

    pub fn notify_mark_read(&self, ids: Option<Vec<u64>>) -> usize {
        self.inner.lock().notify.mark_read(ids.as_deref())
    }

    pub fn hotkey_set(&self, command_id: &str, combo: &str, reset: bool) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock();
        let defaults: HashMap<&str, &str> = crate::types::default_hotkeys().iter().copied().collect();
        let desired = if reset {
            defaults.get(command_id).copied().map(|s| s.to_string())
        } else {
            Some(combo.to_string())
        };
        if let Some(desired) = desired {
            // 冲突检测（同一组合键绑定到其它命令）
            if !desired.is_empty() {
                for (cmd, combo) in inner.config.config.hotkeys.iter() {
                    if cmd != command_id && combo.eq_ignore_ascii_case(&desired) {
                        return Err(AppError::new(
                            crate::error::code::HOTKEY_CONFLICT,
                            format!("快捷键冲突：已被「{}」占用", cmd),
                        ));
                    }
                }
            }
            inner.config.config.hotkeys.insert(command_id.to_string(), desired);
        } else {
            inner.config.config.hotkeys.remove(command_id);
        }
        let hotkeys = inner.config.config.hotkeys.clone();
        inner.config.save()?;
        Ok(json!({ "ok": true, "hotkeys": hotkeys }))
    }

    pub fn app_info(&self) -> serde_json::Value {
        let inner = self.inner.lock();
        json!({
            "version": crate::core::update::CURRENT_VERSION,
            "platform": std::env::consts::OS,
            "vault": inner.vault.as_ref().map(|v| v.root().to_string_lossy().to_string()),
            "configPath": inner.config.path.to_string_lossy(),
            "noteCount": inner.index.as_ref().and_then(|i| i.note_count().ok()).unwrap_or(0),
        })
    }

    // =====================================================================
    // 后台任务
    // =====================================================================

    fn background_maintenance(&self) {
        // 自动备份（FR-502）：晚 5s 执行，避免与启动争 IO
        std::thread::sleep(std::time::Duration::from_secs(5));
        let (has_vault, _backup_dir, _keep, last) = {
            let inner = self.inner.lock();
            match inner.vault {
                Some(_) => (
                    true,
                    PathBuf::from(&inner.config.config.backup.dir),
                    inner.config.config.backup.keep,
                    inner.config.config.backup.last_backup_at.clone(),
                ),
                None => (false, PathBuf::new(), 0, None),
            }
        };
        if has_vault && core::backup::should_backup(last.as_deref(), chrono::Local::now()) {
            match self.backup_now() {
                Ok(r) => {
                    self.notify("info", "已自动备份", Some(format!("{} ({:.1} MB)", r.file, r.size as f64 / 1048576.0)));
                }
                Err(e) => {
                    self.notify("error", "自动备份失败", Some(e.to_string()));
                }
            }
        }

        // 自动更新检查（FR-605）：10s 后执行一次，7 天节流
        std::thread::sleep(std::time::Duration::from_secs(5));
        let cfg = self.inner.lock().config.config.update.clone();
        if cfg.auto_check && core::update::should_check(cfg.last_check.as_deref(), chrono::Local::now()) {
            match self.update_check(false) {
                Ok(_) => {}
                Err(e) => {
                    let _ = e; // 自动检查失败静默
                }
            }
        }
    }
}

// ===========================================================================
// 辅助函数
// ===========================================================================

impl Inner {
    pub fn require_vault(&self) -> Result<Vault> {
        self.vault.clone().ok_or_else(AppError::vault_not_open)
    }
}

/// 扫描-对账：目录树 vs 索引，按 mtime 差异重索引；孤儿条目删除。返回笔记总数。
pub fn reconcile_index(inner: &mut Inner) -> Result<u32> {
    let vault = inner.require_vault()?;
    let files = vault.scan_notes()?;
    let total = files.len() as u32;
    let indexed: HashMap<String, i64> = inner
        .index
        .as_ref()
        .and_then(|i| i.list_indexed().ok())
        .map(|v| v.into_iter().map(|s| (s.path, s.mtime)).collect())
        .unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    for f in &files {
        seen.insert(f.rel.clone());
        let changed = indexed.get(&f.rel).map(|m| *m != f.mtime).unwrap_or(true);
        if !changed {
            continue;
        }
        if let Ok((content, mtime)) = vault.read_note_raw(&f.rel) {
            let parsed = core::links::parse_note(&f.rel, &content, mtime, content.len() as u64);
            if let Some(index) = inner.index.as_mut() {
                if let Err(e) = index.reindex_note(&parsed) {
                    inner.degraded = true;
                    let _ = e;
                }
            }
        }
    }
    // 孤儿清理
    for path in indexed.keys() {
        if !seen.contains(path) {
            if let Some(index) = inner.index.as_mut() {
                let _ = index.remove_note(path);
            }
        }
    }
    Ok(total)
}

/// 检索降级路径：逐文件扫描（DD-3），与正常路径结果结构一致。
fn scan_search(
    vault: &Vault,
    kind: &str,
    term: &str,
    filters: &SearchFilters,
    sort: &str,
    page: usize,
    page_size: usize,
) -> Result<SearchResult> {
    let started = std::time::Instant::now();
    let needle = term.trim().to_lowercase();
    let files = vault.scan_notes()?;
    let mut items: Vec<SearchItem> = Vec::new();
    for f in files {
        if let Some(folder) = &filters.folder {
            if !f.rel.starts_with(folder.trim_matches('/')) {
                continue;
            }
        }
        if let (Some(from), _) = (filters.mtime_from, ()) {
            if f.mtime < from {
                continue;
            }
        }
        if let Some(to) = filters.mtime_to {
            if f.mtime > to {
                continue;
            }
        }
        let Ok((content, _mtime)) = vault.read_note_raw(&f.rel) else { continue };
        let fm = core::links::read_meta(&content);
        let title = core::links::display_title(fm.title.as_deref(), &f.rel);
        if kind == "title" {
            if !needle.is_empty() && !title.to_lowercase().contains(&needle) {
                continue;
            }
            items.push(SearchItem {
                path: f.rel.clone(),
                title: title.clone(),
                snippet: title.clone(),
                hl_offset: title.to_lowercase().find(&needle),
                hl_len: Some(needle.chars().count()),
                mtime: f.mtime,
                score: 0.0,
            });
        } else {
            if needle.is_empty() {
                continue;
            }
            let lower = content.to_lowercase();
            if let Some(pos) = lower.find(&needle) {
                let (snippet, hl_offset) = make_snippet(&content, pos, needle.chars().count());
                items.push(SearchItem {
                    path: f.rel.clone(),
                    title: title.clone(),
                    snippet,
                    hl_offset,
                    hl_len: Some(needle.chars().count()),
                    mtime: f.mtime,
                    score: 0.0,
                });
            }
        }
    }
    if sort == "mtime" || kind == "title" {
        items.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    }
    let total = items.len();
    let start = page.saturating_mul(page_size.max(1));
    let items = items.into_iter().skip(start).take(page_size.max(1)).collect();
    Ok(SearchResult {
        items,
        total,
        degraded: true,
        took_ms: started.elapsed().as_millis() as u64,
        parsed: Some("scan-fallback".into()),
    })
}

fn make_snippet(content: &str, byte_pos: usize, needle_chars: usize) -> (String, Option<usize>) {
    // 以字符为单位取命中位置前后各 60 字符窗口
    let char_pos = content[..byte_pos].chars().count();
    let chars: Vec<char> = content.chars().collect();
    let start = char_pos.saturating_sub(60);
    let end = (char_pos + needle_chars + 60).min(chars.len());
    let snippet: String = chars[start..end].iter().collect::<String>().replace('\n', " ");
    (snippet, Some(char_pos - start))
}

fn context_around_line(content: &str, line: u32, radius: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let idx = (line as usize).min(lines.len().saturating_sub(1));
    let row = lines.get(idx).copied().unwrap_or("");
    let chars: Vec<char> = row.chars().collect();
    let total = chars.len();
    if total <= radius * 2 {
        return row.to_string();
    }
    // 找到双链位置附近截取上下文
    let link_pos = row.find("[[").map(|b| row[..b].chars().count()).unwrap_or(0);
    let center = link_pos.min(total);
    let start = center.saturating_sub(radius);
    let end = (center + radius).min(total);
    let mut s: String = chars[start..end].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if end < total {
        s.push('…');
    }
    s
}

/// 生成库内唯一的笔记路径（重名时 `<stem>-2.md`、`-3` …）。
fn unique_note_path(vault: &Vault, rel: &str) -> Result<String> {
    let p = Path::new(rel);
    let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut candidate = rel.to_string();
    let mut n = 2;
    while vault.abs_path(&candidate).map(|p| p.exists()).unwrap_or(true) {
        candidate = if dir.is_empty() {
            format!("{stem}-{n}.md")
        } else {
            format!("{dir}/{stem}-{n}.md")
        };
        n += 1;
        if n > 9999 {
            break;
        }
    }
    Ok(candidate)
}

fn merge_tags(content: &str) -> Vec<String> {
    let fm = core::links::read_meta(content);
    let (_, body) = core::links::split_front_matter(content);
    let mut tags = fm.tags;
    for t in core::links::extract_inline_tags(body) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags
}

/// 解析导出目标路径：未指定时使用 `%USERPROFILE%\Documents\<默认文件名>`。
fn resolve_out(out: Option<&Path>, default_name: &str) -> Result<PathBuf> {
    if let Some(p) = out {
        return Ok(p.to_path_buf());
    }
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    Ok(PathBuf::from(base).join("Documents").join(sanitize_file_name(default_name)))
}

fn sanitize_file_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    if s.trim().is_empty() {
        s = "export".into();
    }
    s
}

fn mime_for(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

