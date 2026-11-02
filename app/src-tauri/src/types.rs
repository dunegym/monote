//! 跨层数据传输对象（DTO）与核心数据模型。
//!
//! 约定：
//! - 对前端序列化统一使用 camelCase（与设计文档 6.2 的接口签名一致）；
//! - 命令入参由 Tauri 默认的 camelCase → snake_case 映射（JS 传 `rootPath` 对应 Rust `root_path`）；
//! - 路径一律为**库内相对路径**（`/` 分隔），IPC 层负责校验，禁止绝对路径与 `..`。

use serde::{Deserialize, Serialize};

// ===========================================================================
// 库 / 目录树
// ===========================================================================

/// 目录树节点（`vault.list` 输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// 库内相对路径（文件夹不带结尾斜杠）
    pub path: String,
    /// 展示名（文件名/文件夹名）
    pub name: String,
    /// "note" | "folder"
    pub kind: String,
    /// 笔记标题（kind = note 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 文件 mtime（unix ms，kind = note 时）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// 子节点（kind = folder 时；按 depth 截断时为空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode>>,
    /// 是否因 depth 限制而未展开
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// 打开库的结果（`vault.open`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedVault {
    pub root: String,
    pub name: String,
    pub note_count: u32,
    pub index_ok: bool,
    /// 索引重建中（降级/首次建库）
    pub index_building: bool,
}

/// 库信息快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    pub root: String,
    pub name: String,
    pub note_count: u32,
}

// ===========================================================================
// 笔记读写
// ===========================================================================

/// 读取笔记结果（`vault.read`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRead {
    pub path: String,
    pub title: String,
    pub content: String,
    /// 修订号：每次经本应用成功写入 +1（用于乐观并发控制）
    pub rev: u64,
    /// 文件 mtime（unix ms）
    pub mtime: i64,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

/// 写入笔记结果（`vault.write`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub path: String,
    pub rev: u64,
    pub mtime: i64,
    pub title: String,
    pub tags: Vec<String>,
    /// 实际写入的字节数
    pub size: u64,
}

/// 新建空笔记/文件夹结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub path: String,
    /// 是否已存在（幂等创建时）
    pub existed: bool,
}

/// 重命名/移动结果（`vault.rename`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameResult {
    pub path: String,
    pub new_path: String,
    pub rewritten: Vec<RewriteCount>,
}

/// 单文件链接改写计数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewriteCount {
    pub file: String,
    pub count: u32,
}

// ===========================================================================
// 回收站
// ===========================================================================

/// 移入回收站结果（`vault.trash`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashResult {
    pub trash_id: String,
}

/// 回收站条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub id: String,
    /// 原库内相对路径
    pub original_path: String,
    pub name: String,
    /// 入站时间（ISO 8601，含时区）
    pub deleted_at: String,
    /// 距 30 天自动清除的剩余天数（负数表示已到期待清除）
    pub remaining_days: i64,
    /// 是否为文件夹（整个文件夹被删除）
    pub is_dir: bool,
    /// 文件大小（字节，文件夹为 0）
    pub size: u64,
}

/// 回收站列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashList {
    pub items: Vec<TrashEntry>,
    pub total: usize,
}

/// 还原结果（`vault.trashRestore`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub path: String,
    /// 因同名冲突发生的重命名（None = 原路径还原）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<String>,
}

/// 清空回收站结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyTrashResult {
    pub cleared: u32,
}

// ===========================================================================
// 链接 / 标签
// ===========================================================================

/// 双链补全项（`link.suggest`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestItem {
    pub name: String,
    pub path: String,
    /// unix ms
    pub mtime: i64,
}

/// 反链条目（`link.backlinks`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinkItem {
    pub src_path: String,
    pub src_title: String,
    /// 0-based 行号
    pub line: u32,
    /// 引用处上下文（前后各至多 80 字符，已折叠换行）
    pub context: String,
}

/// 标签计数（`tag.list`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub name: String,
    pub count: u32,
    /// 仅来自 front-matter 的计数 / 仅来自正文行内标签的计数（双来源合并展示）
    pub property_count: u32,
    pub inline_count: u32,
}

/// 单个笔记的标签归位结果（`tag.normalize`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagNormalizeItem {
    pub path: String,
    /// 本次新写入 front-matter 的标签（已去重）
    pub added_tags: Vec<String>,
    /// 从正文移除的行内标签
    pub removed_inline_tags: Vec<String>,
}

/// 标签归位报告（`tag.normalize`，`apply=false` 时为预览）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagNormalizeReport {
    pub applied: bool,
    pub scanned: u32,
    pub changed: Vec<TagNormalizeItem>,
    /// 因文件在扫描后被外部修改而跳过的路径
    pub failed: Vec<String>,
}

/// 一个已解析的双链。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLink {
    /// 目标笔记名（去空白；`[[#标题]]` 时为当前笔记标题 + #标题）
    pub target_name: String,
    /// 显示文字（别名为 None）
    pub alias: Option<String>,
    /// 0-based 行号
    pub line: u32,
    /// 是否为笔记内锚点（`[[#标题]]`）
    pub is_anchor: bool,
}

/// 一篇笔记的解析结果（内容 → 元数据 + 链接 + 标签）。
#[derive(Debug, Clone)]
pub struct ParsedNote {
    pub path: String,
    pub title: String,
    /// 原始正文（含 front-matter，UTF-8 字符串）
    pub content: String,
    /// front-matter 中的原始 YAML 文本（不含首尾 `---`）
    pub front_matter: Option<String>,
    pub tags: Vec<String>,
    pub inline_tags: Vec<String>,
    pub property_tags: Vec<String>,
    pub links: Vec<ParsedLink>,
    pub created: Option<String>,
    pub updated: Option<String>,
    /// 内容 xxhash64（十六进制）
    pub hash: String,
    /// 文件 mtime（unix ms）
    pub mtime: i64,
    pub size: u64,
}

// ===========================================================================
// 检索
// ===========================================================================

/// 检索结果条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub path: String,
    pub title: String,
    /// 命中行摘要；标题检索时为标题所在行的上下文
    pub snippet: String,
    /// 摘要中命中词的起始字符偏移（用于前端高亮；None = 无命中词）
    pub hl_offset: Option<usize>,
    /// 命中词长度（字符数）
    pub hl_len: Option<usize>,
    pub mtime: i64,
    /// 相关度（越小越相关，bm25 口径；标题检索时为 0）
    pub score: f64,
}

/// 检索过滤条件（`search.query.filters`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFilters {
    /// 文件夹前缀（库内相对路径，含子目录）
    pub folder: Option<String>,
    /// 标签名（不含 `#`）
    pub tag: Option<String>,
    /// mtime 下限（unix ms，含）
    pub mtime_from: Option<i64>,
    /// mtime 上限（unix ms，含）
    pub mtime_to: Option<i64>,
}

/// 检索结果集（`search.query`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub items: Vec<SearchItem>,
    pub total: usize,
    /// 索引损坏/不可用，已降级为逐文件扫描
    pub degraded: bool,
    pub took_ms: u64,
    /// 本次查询语法解析结果回显（便于调试/前端展示）
    pub parsed: Option<String>,
}

/// 索引状态（`index.status`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub building: bool,
    pub degraded: bool,
    pub indexed: u32,
    pub progress_done: u32,
    pub progress_total: u32,
    pub fts_available: bool,
}

// ===========================================================================
// 导入导出
// ===========================================================================

/// 导入完成报告（`io.import`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub job_id: String,
    /// 成功导入的文件数
    pub imported: u32,
    /// 目标文件夹（库内相对路径；导入单文件时为所在目录）
    pub target_dir: String,
    /// GB18030 等转码导入的文件
    pub transcoded: Vec<String>,
    pub skipped: Vec<SkippedFile>,
}

/// 导入失败/跳过条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    pub file: String,
    pub reason: String,
}

/// 导出结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub out_path: String,
    /// 字节数
    pub size: u64,
}

// ===========================================================================
// 备份
// ===========================================================================

/// 备份结果（`backup.now`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupResult {
    pub file: String,
    pub size: u64,
    /// 保留的备份数量
    pub kept: u32,
}

/// 备份文件条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub file: String,
    pub size: u64,
    /// unix ms
    pub mtime: i64,
}

// ===========================================================================
// WebDAV 同步
// ===========================================================================

/// 同步配置（对前端可见部分；密码不回传）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfigView {
    pub configured: bool,
    pub url: String,
    pub username: String,
    pub password_set: bool,
    pub auto_on_start: bool,
    pub last_run: Option<String>,
}

/// 同步计划（`sync.run`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub up: u32,
    pub down: u32,
    pub conflict: u32,
    pub skipped: u32,
    pub failed: u32,
    pub last_run: String,
}

/// 远端文件条目（PROPFIND 结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub path: String,
    pub etag: String,
    pub mtime: i64,
    pub size: u64,
}

/// 本地文件条目（同步清单）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEntry {
    pub path: String,
    pub mtime: i64,
    pub hash: String,
    pub size: u64,
}

// ===========================================================================
// 通知 / 配置 / 更新
// ===========================================================================

/// 通知条目（`notify.list`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationItem {
    pub id: u64,
    /// info | warn | error
    pub level: String,
    pub title: String,
    pub detail: Option<String>,
    /// unix ms
    pub ts: i64,
    pub read: bool,
}

/// 通知列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationList {
    pub items: Vec<NotificationItem>,
    pub unread: usize,
}

/// 更新检查结果（`app.updateCheck`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: Option<String>,
    pub has_update: bool,
    pub notes: Option<String>,
    pub url: Option<String>,
    pub checked_at: Option<String>,
    /// 本次是否真正发起了网络检查（7 天节流时 false）
    pub checked: bool,
}

// ===========================================================================
// 文件对话框
// ===========================================================================

/// 文件对话框模式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DialogMode {
    File,
    Folder,
    Save,
}

/// 文件对话框结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogResult {
    pub path: Option<String>,
}

// ===========================================================================
// 应用配置（与设计文档 7.4 config.json schema 对齐）
// ===========================================================================

/// 应用配置（config.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub version: u32,
    pub vault: VaultConfig,
    pub ui: UiConfig,
    pub editor: EditorConfig,
    pub backup: BackupConfig,
    pub sync: SyncConfig,
    pub update: UpdateConfig,
    pub hotkeys: std::collections::HashMap<String, String>,
    pub onboarding_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VaultConfig {
    /// 上次打开的库根目录（绝对路径）
    pub root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiConfig {
    /// auto | light | dark
    pub theme: String,
    pub language: String,
    /// 1.0 ~ 2.0（NFR-111）
    pub zoom: f64,
    pub toolbar_visible: bool,
    pub sidebar_visible: bool,
    pub preview_visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EditorConfig {
    pub autosave_debounce_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupConfig {
    pub dir: String,
    pub keep: u32,
    pub last_backup_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncConfig {
    pub auto_on_start: bool,
    pub url: String,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateConfig {
    pub auto_check: bool,
    pub last_check: Option<String>,
    /// 更新清单地址（默认官方地址；自托管测试时可改）
    pub manifest_url: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut hotkeys = std::collections::HashMap::new();
        for (k, v) in default_hotkeys() {
            hotkeys.insert(k.to_string(), v.to_string());
        }
        Self {
            version: 1,
            vault: VaultConfig::default(),
            ui: UiConfig::default(),
            editor: EditorConfig::default(),
            backup: BackupConfig::default(),
            sync: SyncConfig::default(),
            update: UpdateConfig::default(),
            hotkeys,
            onboarding_done: false,
        }
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self { root: None }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            language: "zh-CN".into(),
            zoom: 1.0,
            toolbar_visible: true,
            sidebar_visible: true,
            preview_visible: true,
        }
    }
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self { autosave_debounce_ms: 2000 }
    }
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self { dir: default_backup_dir(), keep: 5, last_backup_at: None }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self { auto_on_start: false, url: String::new(), username: String::new() }
    }
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_check: None,
            manifest_url: "https://update.monote.app/manifest.json".into(),
        }
    }
}

/// 默认备份目录：`<系统文档目录>/MoNote-Backup`。
pub fn default_backup_dir() -> String {
    if let Ok(home) = std::env::var("USERPROFILE") {
        format!("{home}\\Documents\\MoNote-Backup")
    } else if let Ok(home) = std::env::var("HOME") {
        format!("{home}/Documents/MoNote-Backup")
    } else {
        "MoNote-Backup".into()
    }
}

/// 默认快捷键表（FR-604/FR-606，对应设计文档 7.4）。
pub fn default_hotkeys() -> &'static [(&'static str, &'static str)] {
    &[
        ("command.palette", "Ctrl+Shift+P"),
        ("search.title", "Ctrl+P"),
        ("search.full", "Ctrl+Shift+F"),
        ("note.new", "Ctrl+N"),
        ("note.save", "Ctrl+S"),
        ("sidebar.toggle", "Ctrl+\\"),
        ("toolbar.toggle", "Ctrl+Shift+T"),
        ("preview.toggle", "Ctrl+Shift+M"),
        ("trash.open", "Ctrl+Shift+Backspace"),
        ("settings.open", "Ctrl+,"),
        ("search.next", "Ctrl+Shift+J"),
    ]
}
