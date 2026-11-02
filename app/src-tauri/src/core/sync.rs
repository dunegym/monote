//! M6 同步子服务 SYN（对应设计文档 4.6、5.5、6.3、7.3、FR-503）。
//!
//! WebDAV 同步：
//! - 三方比对（本地 / 远端 / 上次同步状态）→ 变更集；
//! - 冲突策略"新者为准 + 冲突副本"（`<名>-conflict-<ts>.md`），两版都保留；
//! - 凭据存 OS 凭据管理器（Windows Credential Manager，服务名 `MoNote.WebDAV`），
//!   不写入 config.json（NFR-108）；
//! - 仅使用 PROPFIND/GET/PUT/MKCOL 四个动词，HTTPS 强制（HTTP 仅警告）。
//!
//! 服务端能力边界（设计 6.3）：不假设服务端支持版本历史/回收站；删除同步不在本轮实现
//! （v1 只做"新增/修改"收敛，避免误删用户数据，作为已知限制在实现说明中声明）。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{code, AppError, Result};
use crate::types::{LocalEntry, RemoteEntry, SyncConfigView, SyncPlan};

/// 凭据管理器服务名。
pub const CRED_SERVICE: &str = "MoNote.WebDAV";

/// 请求节流：设计 6.3 要求 500 ms/请求（适配坚果云等限流服务）。
const REQUEST_GAP_MS: u64 = 500;
/// 单请求超时（设计 6.3：60 s）。
const HTTP_TIMEOUT_SECS: u64 = 60;
/// 递归枚举 Depth:1 时的最大目录层级与请求数保护。
const MAX_RECURSIVE_DEPTH: usize = 16;
const MAX_RECURSIVE_REQUESTS: usize = 500;

/// PROPFIND 请求体：仅取同步需要的 4 个属性。
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:getetag/>
    <d:getlastmodified/>
    <d:getcontentlength/>
    <d:resourcetype/>
  </d:prop>
</d:propfind>"#;

/// sync-state.json（设计文档 7.3）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncState {
    pub server: String,
    pub last_run: Option<String>,
    pub entries: BTreeMap<String, SyncEntry>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncEntry {
    /// 服务端 ETag（原样保存，含引号）
    pub etag: String,
    /// 上次同步时本地修订号（本应用内存修订号的持久化快照）
    pub local_rev: u64,
    /// 上次同步时间（ISO 8601）
    pub last_sync: Option<String>,
}

/// 单个文件的同步动作（纯函数 planning 的输出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// 仅本地改：上传
    Upload { path: String, remote_etag: Option<String> },
    /// 仅远端改：下载
    Download { path: String, remote_etag: String, remote_mtime: i64 },
    /// 双端改：新者为准 + 冲突副本
    Conflict { path: String, local_newer: bool, remote_etag: String, remote_mtime: i64 },
    /// 双端相同 / 无需动作
    Skip { path: String },
}

/// 纯函数：三方比对生成同步计划（可单测，NFR-110）。
///
/// 判定规则（可解释、无隐藏状态）：
/// - 本地有、远端无（含"上次同步过但远端被删"）：**Upload**（`If-None-Match: *` 新建；
///   v1 不做删除传播，宁可远端复活也不丢本地数据）；
/// - 远端有、本地无：**Download**；
/// - 双端都有：
///   - 有同步基线（sync-state）：本地是否改 = 本地 mtime > 上次同步时间；
///     远端是否改 = ETag 与基线不同（服务端不返回 ETag 时退化为远端 mtime > 上次同步时间）；
///     - 均未改 → **Skip**；
///     - 仅本地改 → **Upload**（`If-Match: 基线 ETag` 乐观锁）；
///     - 仅远端改 → **Download**；
///     - 均改 → **Conflict**，`local_newer` 由 mtime 判定（新者为主版本，旧者转冲突副本）；
///   - 无基线（首次同步/状态丢失）：大小与 mtime 完全一致视为同一版本 **Skip**，
///     否则保守判 **Conflict**（不静默覆盖任何一侧）。
pub fn plan_sync(local: &[LocalEntry], remote: &[RemoteEntry], state: &SyncState) -> Vec<SyncAction> {
    let locals: BTreeMap<&str, &LocalEntry> = local.iter().map(|e| (e.path.as_str(), e)).collect();
    let remotes: BTreeMap<&str, &RemoteEntry> = remote.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut paths: BTreeSet<&str> = locals.keys().copied().collect();
    paths.extend(remotes.keys().copied());

    let mut actions = Vec::new();
    for path in paths {
        match (locals.get(path), remotes.get(path)) {
            // 本地新增 / 远端已删除：上传（新建语义）
            (Some(_), None) => actions.push(SyncAction::Upload {
                path: path.to_string(),
                remote_etag: None,
            }),
            // 远端新增 / 本地已删除：下载（不丢远端数据）
            (None, Some(r)) => actions.push(SyncAction::Download {
                path: path.to_string(),
                remote_etag: r.etag.clone(),
                remote_mtime: r.mtime,
            }),
            (Some(l), Some(r)) => {
                match state.entries.get(path) {
                    None => {
                        if l.size == r.size && l.mtime == r.mtime {
                            actions.push(SyncAction::Skip { path: path.to_string() });
                        } else {
                            actions.push(SyncAction::Conflict {
                                path: path.to_string(),
                                local_newer: l.mtime >= r.mtime,
                                remote_etag: r.etag.clone(),
                                remote_mtime: r.mtime,
                            });
                        }
                    }
                    Some(baseline) => {
                        let local_changed = local_changed_since(l, baseline);
                        let remote_changed = remote_changed_since(r, baseline);
                        match (local_changed, remote_changed) {
                            (false, false) => actions.push(SyncAction::Skip { path: path.to_string() }),
                            (true, false) => actions.push(SyncAction::Upload {
                                path: path.to_string(),
                                remote_etag: if baseline.etag.is_empty() {
                                    None
                                } else {
                                    Some(baseline.etag.clone())
                                },
                            }),
                            (false, true) => actions.push(SyncAction::Download {
                                path: path.to_string(),
                                remote_etag: r.etag.clone(),
                                remote_mtime: r.mtime,
                            }),
                            (true, true) => actions.push(SyncAction::Conflict {
                                path: path.to_string(),
                                local_newer: l.mtime >= r.mtime,
                                remote_etag: r.etag.clone(),
                                remote_mtime: r.mtime,
                            }),
                        }
                    }
                }
            }
            (None, None) => {}
        }
    }
    actions
}

/// 本地自上次同步后是否修改：mtime 晚于状态里的 `last_sync`；无可用基线时间则保守视为已修改。
fn local_changed_since(local: &LocalEntry, baseline: &SyncEntry) -> bool {
    match baseline.last_sync.as_deref().and_then(parse_iso_ms) {
        Some(sync_ms) => local.mtime > sync_ms,
        None => true,
    }
}

/// 远端自上次同步后是否修改：优先比较 ETag（可靠），服务端无 ETag 时退回 mtime。
fn remote_changed_since(remote: &RemoteEntry, baseline: &SyncEntry) -> bool {
    if !remote.etag.is_empty() && !baseline.etag.is_empty() {
        return remote.etag != baseline.etag;
    }
    match baseline.last_sync.as_deref().and_then(parse_iso_ms) {
        Some(sync_ms) => remote.mtime > sync_ms,
        None => true,
    }
}

fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.timestamp_millis())
}

// ===========================================================================
// 状态读写
// ===========================================================================

fn state_path(vault_root: &Path) -> PathBuf {
    vault_root.join(".monote").join("sync-state.json")
}

/// 读取 sync-state.json（不存在时返回默认）。
///
/// 状态是派生数据（设计 7.3）：文件损坏时按空状态处理，下一轮重新建立基线，不阻断同步。
pub fn load_state(vault_root: &Path) -> Result<SyncState> {
    let path = state_path(vault_root);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<SyncState>(&text) {
            Ok(state) => Ok(state),
            Err(e) => {
                eprintln!("[MoNote] sync-state.json 损坏，按空状态重新建立基线（派生数据可重建）：{e}");
                Ok(SyncState::default())
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SyncState::default()),
        Err(e) => Err(AppError::io_ctx(
            format!("读取同步状态失败：{}", path.display()),
            &e,
        )),
    }
}

/// 原子保存 sync-state.json。
pub fn save_state(vault_root: &Path, state: &SyncState) -> Result<()> {
    let path = state_path(vault_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::io_ctx(format!("创建 .monote 目录失败：{}", parent.display()), &e)
        })?;
    }
    let json = serde_json::to_vec_pretty(state).map_err(AppError::from)?;
    atomic_write(&path, &json)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::invalid_arg("同步状态路径无效"))?;
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("写同步状态失败：{}", path.display()), &e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("替换同步状态失败：{}", path.display()), &e));
    }
    Ok(())
}

// ===========================================================================
// 同步执行
// ===========================================================================

/// 执行同步：读取本地清单 → PROPFIND 远端 → plan → 逐文件传输 → 更新 sync-state。
pub fn run_sync(
    vault_root: &Path,
    url: &str,
    username: &str,
    password: &str,
    now: chrono::DateTime<chrono::Local>,
) -> Result<SyncPlan> {
    let url = url.trim();
    if url.is_empty() {
        return Err(AppError::invalid_arg("未配置 WebDAV 地址（请先在设置中填写）"));
    }
    if !vault_root.is_dir() {
        return Err(AppError::invalid_arg(format!("库目录不存在：{}", vault_root.display())));
    }
    if url.to_ascii_lowercase().starts_with("http://") {
        eprintln!("[MoNote] 警告：WebDAV 使用 HTTP 明文连接，凭据与笔记内容可能被窃听；建议改用 HTTPS（NFR-108）");
    }

    // 1. 本地清单（原子快照语义：一次扫描，后续写入以快照 mtime 判断是否被并发编辑）
    let local = collect_local_entries(vault_root)?;
    let local_snapshot: BTreeMap<String, i64> =
        local.iter().map(|e| (e.path.clone(), e.mtime)).collect();

    // 2. 远端清单 + 三方比对
    let mut state = load_state(vault_root)?;
    let client = WebDavClient::new(url, username, password)?;
    let remote = client.propfind_all()?;
    let actions = plan_sync(&local, &remote, &state);

    // 3. 逐文件执行（单文件失败不中断整轮，计入 failed；
    //    断网/认证失败属于终止性错误 → 终止本轮，已传文件不回滚，下轮续跑，设计 5.5 ✗）
    let now_iso = now.to_rfc3339();
    let ts = now.format("%Y%m%d-%H%M%S").to_string();
    let (plan, fatal) = {
        let mut runner = SyncRunner {
            client: &client,
            root: vault_root,
            state: &mut state,
            local_snapshot: &local_snapshot,
            now_iso: &now_iso,
            ts: &ts,
            plan: SyncPlan {
                up: 0,
                down: 0,
                conflict: 0,
                skipped: 0,
                failed: 0,
                last_run: now_iso.clone(),
            },
        };
        let mut network_actions = 0usize;
        let mut fatal: Option<AppError> = None;
        'actions: for action in actions {
            let is_network = !matches!(action, SyncAction::Skip { .. });
            if is_network {
                if network_actions > 0 {
                    // 节流（设计 6.3）；阻塞式同步在专用线程执行，不阻塞 UI。
                    std::thread::sleep(Duration::from_millis(REQUEST_GAP_MS));
                }
                network_actions += 1;
            }
            let result = match &action {
                SyncAction::Upload { path, remote_etag } => runner.upload(path, remote_etag.as_deref()),
                SyncAction::Download { path, remote_etag, .. } => runner.download(path, remote_etag),
                SyncAction::Conflict { path, remote_etag, remote_mtime, .. } => {
                    runner.conflict(path, remote_etag, *remote_mtime)
                }
                SyncAction::Skip { .. } => {
                    runner.plan.skipped += 1;
                    Ok(())
                }
            };
            if let Err(e) = result {
                eprintln!("[MoNote] 同步动作失败（{action:?}）：{e}");
                runner.plan.failed += 1;
                if e.code == code::NETWORK_UNREACHABLE || e.code == code::WEBDAV_AUTH {
                    fatal = Some(e);
                    break 'actions;
                }
            }
        }
        (runner.plan, fatal)
    };

    // 4. 更新同步状态（含本轮已完成文件的基线；终止性错误也保存已取得的进度）
    state.last_run = Some(now_iso.clone());
    state.server = format!("{}/", url.trim_end_matches('/'));
    save_state(vault_root, &state)?;

    if let Some(e) = fatal {
        return Err(AppError::new(e.code, format!("同步中断：{}", e.msg)).with_detail(format!(
            "已完成 up={} down={} conflict={} skipped={}，失败 {}；已传文件不回滚，下轮续跑（原始错误：{}）",
            plan.up,
            plan.down,
            plan.conflict,
            plan.skipped,
            plan.failed,
            e.detail.unwrap_or_default()
        )));
    }
    Ok(plan)
}

struct SyncRunner<'a> {
    client: &'a WebDavClient,
    root: &'a Path,
    state: &'a mut SyncState,
    local_snapshot: &'a BTreeMap<String, i64>,
    now_iso: &'a str,
    ts: &'a str,
    plan: SyncPlan,
}

impl<'a> SyncRunner<'a> {
    fn mark_state(&mut self, rel: &str, etag: Option<String>) {
        let rev = self
            .state
            .entries
            .get(rel)
            .map(|e| e.local_rev)
            .unwrap_or(0)
            .saturating_add(1);
        self.state.entries.insert(
            rel.to_string(),
            SyncEntry {
                etag: etag.unwrap_or_default(),
                local_rev: rev,
                last_sync: Some(self.now_iso.to_string()),
            },
        );
    }

    /// 上传本地文件（`if_match` = 基线 ETag，即乐观锁；None 表示新建）。
    fn upload(&mut self, rel: &str, if_match: Option<&str>) -> Result<()> {
        let bytes = read_local_bytes(self.root, rel)?;
        let etag = self.client.put(rel, &bytes, if_match)?;
        self.mark_state(rel, etag);
        self.plan.up += 1;
        Ok(())
    }

    /// 下载远端文件并原子写入本地；若快照后本地被编辑（可能是用户缓冲区），
    /// 不覆盖本地，改为把远端版本写成冲突副本（设计 5.5 ✗ 与编辑并发）。
    fn download(&mut self, rel: &str, remote_etag: &str) -> Result<()> {
        let Some((bytes, etag)) = self.client.get(rel)? else {
            // 计划后远端被删除：本轮跳过，下轮重新比对
            self.plan.skipped += 1;
            return Ok(());
        };
        if self.local_modified_since_snapshot(rel) {
            let copy = unique_conflict_rel(self.root, rel, self.ts);
            write_local_atomic(self.root, &copy, &bytes)?;
            let copy_etag = self.client.put(&copy, &bytes, None)?;
            self.mark_state(&copy, copy_etag);
            self.plan.conflict += 1;
            return Ok(());
        }
        write_local_atomic(self.root, rel, &bytes)?;
        let etag = etag.or_else(|| Some(remote_etag.to_string()));
        self.mark_state(rel, etag);
        self.plan.down += 1;
        Ok(())
    }

    /// 双端修改：新者为主版本，旧者另存 `<名>-conflict-<ts>.md`，两份都保留并提交远端。
    fn conflict(&mut self, rel: &str, remote_etag: &str, remote_mtime: i64) -> Result<()> {
        let Some((remote_bytes, etag)) = self.client.get(rel)? else {
            // 远端在计划后消失 → 本地为主，直接上传
            self.upload(rel, None)?;
            return Ok(());
        };
        let remote_etag = etag.unwrap_or_else(|| remote_etag.to_string());
        let local_bytes = read_local_bytes(self.root, rel)?;
        let local_mtime = local_mtime(self.root, rel).unwrap_or(0);
        // 同步期间本地又被修改时以本地为准，绝不丢用户输入
        let local_newer = self.local_modified_since_snapshot(rel) || local_mtime >= remote_mtime;
        let copy = unique_conflict_rel(self.root, rel, self.ts);
        if local_newer {
            // 本地为主：远端旧版 → 本地冲突副本 + 上传到远端
            write_local_atomic(self.root, &copy, &remote_bytes)?;
            let primary_etag = self.client.put(rel, &local_bytes, Some(&remote_etag))?;
            let copy_etag = self.client.put(&copy, &remote_bytes, None)?;
            self.mark_state(rel, primary_etag);
            self.mark_state(&copy, copy_etag);
        } else {
            // 远端为主：本地旧版 → 冲突副本 + 上传；远端版本覆盖本地主版本
            write_local_atomic(self.root, &copy, &local_bytes)?;
            write_local_atomic(self.root, rel, &remote_bytes)?;
            let copy_etag = self.client.put(&copy, &local_bytes, None)?;
            self.mark_state(rel, Some(remote_etag));
            self.mark_state(&copy, copy_etag);
        }
        self.plan.conflict += 1;
        Ok(())
    }

    fn local_modified_since_snapshot(&self, rel: &str) -> bool {
        match (self.local_snapshot.get(rel).copied(), local_mtime(self.root, rel)) {
            (Some(previous), Some(current)) => previous != current,
            (None, Some(_)) => true, // 清单之后才出现的文件（用户刚编辑/创建）
            _ => false,
        }
    }
}

/// 扫描参与同步的本地文件（笔记 + attachments，排除 `.monote/`、`.trash/` 与隐藏目录）。
fn collect_local_entries(vault_root: &Path) -> Result<Vec<LocalEntry>> {
    let mut found: BTreeMap<String, LocalEntry> = BTreeMap::new();
    let walker = walkdir::WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_type().is_dir() {
                // 跳过 .monote/.trash/.git 等派生或隐藏目录
                return !e.file_name().to_string_lossy().starts_with('.');
            }
            true
        });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(vault_root) {
            Ok(p) => path_to_slash(p),
            Err(_) => continue,
        };
        if !is_sync_path(&rel) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let hash = match hash_file(entry.path()) {
            Ok(h) => h,
            Err(_) => continue,
        };
        found.insert(
            rel.clone(),
            LocalEntry {
                path: rel,
                mtime: meta_mtime_ms(&meta),
                hash,
                size: meta.len(),
            },
        );
    }
    Ok(found.into_values().collect())
}

/// 参与同步的路径判定（设计 6.3：`.monote/`、`.trash/` 以外的全部 `.md` 与 `attachments/`）。
fn is_sync_path(rel: &str) -> bool {
    if rel.is_empty() {
        return false;
    }
    let first = rel.split('/').next().unwrap_or("");
    if first.starts_with('.') {
        return false;
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if name.starts_with('.') || name.contains(".tmp-") {
        return false;
    }
    if rel.starts_with("attachments/") {
        return true;
    }
    matches!(
        Path::new(rel).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("md") | Some("markdown")
    )
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::hash::Hasher as _;
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = twox_hash::XxHash64::with_seed(0);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn read_local_bytes(root: &Path, rel: &str) -> Result<Vec<u8>> {
    let abs = root.join(rel_to_native(rel));
    std::fs::read(&abs).map_err(|e| AppError::io_ctx(format!("读取本地文件失败：{rel}"), &e))
}

/// 下载写入必须原子（临时文件 + rename），避免崩溃留下半截笔记文件。
fn write_local_atomic(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let abs = root.join(rel_to_native(rel));
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::io_ctx(format!("创建目录失败：{}", parent.display()), &e))?;
    }
    let file_name = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::invalid_arg("同步文件路径无效"))?;
    let tmp = abs.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("写入同步文件失败：{rel}"), &e));
    }
    if let Err(e) = std::fs::rename(&tmp, &abs) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("替换同步文件失败：{rel}"), &e));
    }
    Ok(())
}

fn local_mtime(root: &Path, rel: &str) -> Option<i64> {
    let meta = std::fs::metadata(root.join(rel_to_native(rel))).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(meta_mtime_ms(&meta))
}

fn meta_mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn rel_to_native(rel: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for part in rel.split('/') {
        if !part.is_empty() {
            p.push(part);
        }
    }
    p
}

fn path_to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// 冲突副本相对路径：`<stem>-conflict-<ts><ext>`；本地已存在时追加序号避免覆盖。
fn unique_conflict_rel(root: &Path, rel: &str, ts: &str) -> String {
    let (dir, name) = match rel.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, rel),
    };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, ".md".to_string()),
    };
    for n in 0..1000u32 {
        let base = if n == 0 {
            format!("{stem}-conflict-{ts}{ext}")
        } else {
            format!("{stem}-conflict-{ts}-{n}{ext}")
        };
        let cand = match dir {
            Some(d) => format!("{d}/{base}"),
            None => base,
        };
        if !root.join(rel_to_native(&cand)).exists() {
            return cand;
        }
    }
    let fallback = format!("{stem}-conflict-{ts}-{}{ext}", std::process::id());
    match dir {
        Some(d) => format!("{d}/{fallback}"),
        None => fallback,
    }
}

// ===========================================================================
// WebDAV 客户端
// ===========================================================================

/// WebDAV 客户端（阻塞式 reqwest，Tauri 侧放入同步线程调用）。
pub struct WebDavClient {
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub http: reqwest::blocking::Client,
}

impl WebDavClient {
    pub fn new(url: &str, username: &str, password: &str) -> Result<Self> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid_arg("WebDAV 地址不能为空"));
        }
        let parsed = reqwest::Url::parse(trimmed)
            .map_err(|e| AppError::invalid_arg(format!("WebDAV 地址无效：{e}")))?;
        match parsed.scheme() {
            "https" => {}
            "http" => eprintln!(
                "[MoNote] 警告：WebDAV 使用 HTTP 明文传输，凭据与笔记内容可能被窃听；建议改用 HTTPS（NFR-108）"
            ),
            other => {
                return Err(AppError::invalid_arg(format!(
                    "WebDAV 地址协议不受支持：{other}（仅支持 http/https）"
                )))
            }
        }
        let base_url = format!("{}/", parsed.as_str().trim_end_matches('/'));
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(concat!("MoNote/", "1.0.0"))
            .build()
            .map_err(|e| AppError::network("初始化 WebDAV 客户端失败").with_detail(e.to_string()))?;
        Ok(Self {
            base_url,
            username: username.to_string(),
            password: password.to_string(),
            http,
        })
    }

    /// PROPFIND Depth: infinity（失败时降级逐层 Depth:1）。
    pub fn propfind_all(&self) -> Result<Vec<RemoteEntry>> {
        match self.propfind("infinity", &self.base_url) {
            Ok((207, body)) => Ok(dedupe_remote(parse_multistatus(&body, &self.base_url))),
            Ok((status, _)) => {
                eprintln!(
                    "[MoNote] PROPFIND Depth:infinity 返回 HTTP {status}，降级 Depth:1 逐层枚举"
                );
                self.propfind_depth1()
            }
            Err(e) if e.code == code::WEBDAV_AUTH => Err(e),
            Err(e) => {
                eprintln!("[MoNote] PROPFIND Depth:infinity 失败（{e}），降级 Depth:1 逐层枚举");
                self.propfind_depth1()
            }
        }
    }

    /// GET 文件；404 返回 Ok(None)。
    pub fn get(&self, path: &str) -> Result<Option<(Vec<u8>, Option<String>)>> {
        let resp = self
            .http
            .get(self.url_for(path))
            .basic_auth(&self.username, Some(&self.password))
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .send()
            .map_err(|e| AppError::network("WebDAV 下载失败").with_detail(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            let etag = response_etag(&resp);
            let bytes = resp
                .bytes()
                .map_err(|e| AppError::network("读取 WebDAV 响应失败").with_detail(e.to_string()))?
                .to_vec();
            return Ok(Some((bytes, etag)));
        }
        match status.as_u16() {
            404 => Ok(None),
            401 => Err(auth_error()),
            other => Err(AppError::network(format!("WebDAV 下载失败：HTTP {other}")).with_detail(path)),
        }
    }

    /// PUT 文件；返回新 ETag（服务端未返回时 None）。
    ///
    /// - `if_match=Some(etag)`：乐观锁（`If-Match`），远端已变返回 412（4002）；
    /// - `if_match=None`：新建语义（`If-None-Match: *`）；
    /// - 409：父目录缺失 → 先 MKCOL 父目录再重试一次。
    pub fn put(&self, path: &str, bytes: &[u8], if_match: Option<&str>) -> Result<Option<String>> {
        let mut attempt = 0u8;
        loop {
            let mut req = self
                .http
                .put(self.url_for(path))
                .basic_auth(&self.username, Some(&self.password))
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes.to_vec());
            req = match if_match.filter(|e| !e.is_empty()) {
                Some(etag) => req.header("If-Match", etag),
                None => req.header("If-None-Match", "*"),
            };
            let resp = req
                .send()
                .map_err(|e| AppError::network("WebDAV 上传失败").with_detail(e.to_string()))?;
            let status = resp.status();
            if status.is_success() {
                return Ok(response_etag(&resp));
            }
            match status.as_u16() {
                401 => return Err(auth_error()),
                409 if attempt == 0 => {
                    attempt = 1;
                    self.mkcol_parents(path)?;
                    continue;
                }
                412 => {
                    return Err(AppError::new(code::SYNC_CONFLICT, "远端文件已被修改（ETag 不匹配）")
                        .with_detail(path))
                }
                other => {
                    return Err(AppError::network(format!("WebDAV 上传失败：HTTP {other}"))
                        .with_detail(path))
                }
            }
        }
    }

    /// 创建目录（已存在忽略）。
    pub fn mkcol(&self, path: &str) -> Result<()> {
        let mut attempt = 0u8;
        loop {
            let method = dav_method(b"MKCOL")?;
            let resp = self
                .http
                .request(method, self.collection_url(path))
                .basic_auth(&self.username, Some(&self.password))
                .send()
                .map_err(|e| AppError::network("WebDAV 创建目录失败").with_detail(e.to_string()))?;
            let status = resp.status();
            if status.is_success() {
                return Ok(());
            }
            match status.as_u16() {
                401 => return Err(auth_error()),
                405 => return Ok(()), // 已存在（RFC 4918 允许 405）
                409 if attempt == 0 => {
                    attempt = 1;
                    self.mkcol_parents(path)?;
                    continue;
                }
                other => {
                    return Err(AppError::network(format!("WebDAV 创建目录失败：HTTP {other}"))
                        .with_detail(path))
                }
            }
        }
    }

    // ---- 内部辅助 -------------------------------------------------------

    /// 相对路径 → 完整 URL（逐段百分号编码，支持中文文件名）。
    fn url_for(&self, path: &str) -> String {
        format!("{}{}", self.base_url, encode_path(path))
    }

    /// 相对目录路径 → 集合 URL（目录必须有结尾 `/`）。
    fn collection_url(&self, path: &str) -> String {
        let rel = path.trim_matches('/');
        if rel.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}{}/", self.base_url, encode_path(rel))
        }
    }

    /// 逐级创建目录的父目录链（PUT/MKCOL 返回 409 时使用）。
    fn mkcol_parents(&self, rel: &str) -> Result<()> {
        let parts: Vec<&str> = rel.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() <= 1 {
            return Ok(()); // 父目录就是库根（应已存在）
        }
        let mut acc = String::new();
        for part in &parts[..parts.len() - 1] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            self.mkcol(&acc)?;
        }
        Ok(())
    }

    /// 单次 PROPFIND：返回 `(HTTP 状态码, 响应体)`；401 直接映射为 4001。
    fn propfind(&self, depth: &str, url: &str) -> Result<(u16, String)> {
        let method = dav_method(b"PROPFIND")?;
        let resp = self
            .http
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Depth", depth)
            .header(reqwest::header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .header(reqwest::header::ACCEPT, "application/xml")
            .body(PROPFIND_BODY.to_string())
            .send()
            .map_err(|e| AppError::network("WebDAV 连接失败").with_detail(e.to_string()))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Err(auth_error());
        }
        if !status.is_success() {
            return Ok((status.as_u16(), String::new()));
        }
        let body = resp
            .text()
            .map_err(|e| AppError::network("读取 PROPFIND 响应失败").with_detail(e.to_string()))?;
        Ok((status.as_u16(), body))
    }

    /// Depth:1 逐层降级枚举（服务端不支持 infinity 时使用）。
    fn propfind_depth1(&self) -> Result<Vec<RemoteEntry>> {
        let mut files: BTreeMap<String, RemoteEntry> = BTreeMap::new();
        let mut queue: Vec<(String, usize)> = vec![(self.base_url.clone(), 0)];
        let mut requests = 0usize;
        while let Some((collection, depth)) = queue.pop() {
            if depth > MAX_RECURSIVE_DEPTH || requests >= MAX_RECURSIVE_REQUESTS {
                eprintln!("[MoNote] WebDAV 目录层级/数量超过保护上限，提前结束枚举");
                break;
            }
            requests += 1;
            let (status, body) = self.propfind("1", &collection)?;
            if status != 207 {
                continue; // 子目录在枚举间隙被删/不可读：跳过该层，不中断整轮
            }
            for raw in parse_multistatus(&body, &self.base_url) {
                if raw.path.is_empty() {
                    continue;
                }
                if raw.is_dir {
                    queue.push((self.collection_url(&raw.path), depth + 1));
                } else if is_sync_path(&raw.path) {
                    files.entry(raw.path.clone()).or_insert(RemoteEntry {
                        path: raw.path,
                        etag: raw.etag,
                        mtime: raw.mtime,
                        size: raw.size,
                    });
                }
            }
        }
        Ok(files.into_values().collect())
    }
}

fn dedupe_remote(raw: Vec<RawDav>) -> Vec<RemoteEntry> {
    let mut files: BTreeMap<String, RemoteEntry> = BTreeMap::new();
    for item in raw {
        if item.is_dir || item.path.is_empty() || !is_sync_path(&item.path) {
            continue;
        }
        files.entry(item.path.clone()).or_insert(RemoteEntry {
            path: item.path,
            etag: item.etag,
            mtime: item.mtime,
            size: item.size,
        });
    }
    files.into_values().collect()
}

fn auth_error() -> AppError {
    AppError::new(code::WEBDAV_AUTH, "WebDAV 认证失败").with_detail("请检查用户名与密码")
}

fn dav_method(name: &[u8]) -> Result<reqwest::Method> {
    reqwest::Method::from_bytes(name)
        .map_err(|e| AppError::network("构造 WebDAV 请求方法失败").with_detail(e.to_string()))
}

fn response_etag(resp: &reqwest::blocking::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

// ---- 207 Multistatus 解析（无 XML 依赖：正则按元素提取）--------------------

#[derive(Debug, Clone)]
struct RawDav {
    path: String,
    is_dir: bool,
    etag: String,
    mtime: i64,
    size: u64,
}

fn parse_multistatus(body: &str, base_url: &str) -> Vec<RawDav> {
    let Ok(response_re) = regex::Regex::new(
        r"(?is)<(?:[A-Za-z0-9]+:)?response\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?response\s*>",
    ) else {
        return Vec::new();
    };
    let Ok(href_re) =
        regex::Regex::new(r"(?is)<(?:[A-Za-z0-9]+:)?href\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?href\s*>")
    else {
        return Vec::new();
    };
    let Ok(etag_re) = regex::Regex::new(
        r"(?is)<(?:[A-Za-z0-9]+:)?getetag\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?getetag\s*>",
    ) else {
        return Vec::new();
    };
    let Ok(lm_re) = regex::Regex::new(
        r"(?is)<(?:[A-Za-z0-9]+:)?getlastmodified\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?getlastmodified\s*>",
    ) else {
        return Vec::new();
    };
    let Ok(len_re) = regex::Regex::new(
        r"(?is)<(?:[A-Za-z0-9]+:)?getcontentlength\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?getcontentlength\s*>",
    ) else {
        return Vec::new();
    };
    let Ok(rt_re) = regex::Regex::new(
        r"(?is)<(?:[A-Za-z0-9]+:)?resourcetype\b[^>]*>(.*?)</(?:[A-Za-z0-9]+:)?resourcetype\s*>",
    ) else {
        return Vec::new();
    };

    let base_path = base_path_decoded(base_url);
    let mut out = Vec::new();
    for block_caps in response_re.captures_iter(body) {
        let Some(block_match) = block_caps.get(1) else { continue };
        let block = block_match.as_str();
        let Some(href) = href_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
        else {
            continue;
        };
        let Some((path, dir_by_slash)) = href_to_rel(&href, base_url, &base_path) else {
            continue;
        };
        let is_dir = dir_by_slash
            || rt_re
                .captures(block)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_ascii_lowercase().contains("collection"))
                .unwrap_or(false);
        let etag = etag_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| xml_unescape(m.as_str().trim()))
            .unwrap_or_default();
        let mtime = lm_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| parse_http_date(m.as_str().trim()))
            .unwrap_or(0);
        let size = len_re
            .captures(block)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().trim().parse::<u64>().ok())
            .unwrap_or(0);
        out.push(RawDav { path, is_dir, etag, mtime, size });
    }
    out
}

/// 服务器返回的 href → 相对 base 集合的路径；返回 `(路径, 是否目录)`。
fn href_to_rel(href: &str, base_url: &str, base_path_decoded: &str) -> Option<(String, bool)> {
    let raw = xml_unescape(href.trim());
    if raw.is_empty() {
        return None;
    }
    let path_encoded = if raw.starts_with("http://") || raw.starts_with("https://") {
        reqwest::Url::parse(&raw).ok()?.path().to_string()
    } else if raw.starts_with('/') {
        // 服务器绝对路径：补上 base 的 origin 后交给 URL 解析器做编码归一
        let base = reqwest::Url::parse(base_url).ok()?;
        let origin = base.origin().ascii_serialization();
        reqwest::Url::parse(&format!("{origin}{raw}")).ok()?.path().to_string()
    } else {
        let base = reqwest::Url::parse(base_url).ok()?;
        base.join(&raw).ok()?.path().to_string()
    };
    let decoded = percent_decode(&path_encoded);
    let base_trim = base_path_decoded.trim_end_matches('/');
    if decoded.trim_end_matches('/') == base_trim || decoded == base_path_decoded {
        return Some((String::new(), true)); // 集合自身
    }
    let prefix = format!("{base_trim}/");
    let rest = decoded.strip_prefix(&prefix)?;
    let is_dir = rest.ends_with('/');
    Some((rest.trim_matches('/').to_string(), is_dir))
}

fn base_path_decoded(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .map(|u| percent_decode(u.path()))
        .unwrap_or_else(|_| "/".to_string())
}

fn parse_http_date(s: &str) -> i64 {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
        return dt.timestamp_millis();
    }
    let normalized = s.replace(" GMT", " +0000").replace(" UTC", " +0000");
    if normalized != s {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&normalized) {
            return dt.timestamp_millis();
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis();
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%a, %d %b %Y %H:%M:%S") {
        return naive.and_utc().timestamp_millis();
    }
    0
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        rest = &rest[idx..];
        if let Some(end) = rest.find(';') {
            let entity = &rest[1..end];
            let replacement = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = entity.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        None
                    }
                }
            };
            if let Some(c) = replacement {
                out.push(c);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// 路径逐段百分号编码（保留 `/`），用于拼 WebDAV URL。
fn encode_path(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for &b in rel.trim_matches('/').as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0F));
        }
    }
    out
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => char::from(b'0' + n),
        _ => char::from(b'A' + (n - 10)),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ===========================================================================
// 凭据（OS 凭据管理器）
// ===========================================================================

fn cred_entry(username: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(CRED_SERVICE, username)
        .map_err(|e| AppError::new(code::WEBDAV_AUTH, "无法访问系统凭据管理器").with_detail(e.to_string()))
}

/// 凭据写入 OS 凭据管理器。
pub fn set_password(username: &str, password: &str) -> Result<()> {
    if username.trim().is_empty() {
        return Err(AppError::invalid_arg("WebDAV 用户名不能为空"));
    }
    let entry = cred_entry(username)?;
    entry
        .set_password(password)
        .map_err(|e| AppError::new(code::WEBDAV_AUTH, "写入系统凭据管理器失败").with_detail(e.to_string()))
}

/// 从 OS 凭据管理器读取密码。
pub fn get_password(username: &str) -> Result<Option<String>> {
    if username.trim().is_empty() {
        return Ok(None);
    }
    let entry = cred_entry(username)?;
    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::new(code::WEBDAV_AUTH, "读取系统凭据管理器失败")
            .with_detail(e.to_string())),
    }
}

/// 删除 OS 凭据管理器中的密码。
pub fn delete_password(username: &str) -> Result<()> {
    if username.trim().is_empty() {
        return Ok(());
    }
    let entry = cred_entry(username)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::new(code::WEBDAV_AUTH, "删除系统凭据失败").with_detail(e.to_string())),
    }
}

/// 同步配置视图（密码只回传是否已设置）。
pub fn config_view(url: &str, username: &str, auto_on_start: bool, last_run: Option<String>) -> SyncConfigView {
    SyncConfigView {
        configured: !url.is_empty() && !username.is_empty(),
        url: url.to_string(),
        username: username.to_string(),
        password_set: get_password(username).ok().flatten().is_some(),
        auto_on_start,
        last_run,
    }
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "https://dav.example.com/monote/";
    const SYNC_AT: &str = "2026-11-01T10:00:00+08:00";

    fn sync_ms() -> i64 {
        parse_iso_ms(SYNC_AT).expect("valid timestamp")
    }

    fn local(path: &str, mtime: i64, size: u64) -> LocalEntry {
        LocalEntry {
            path: path.to_string(),
            mtime,
            hash: format!("{mtime:x}-{size}"),
            size,
        }
    }

    fn remote(path: &str, etag: &str, mtime: i64, size: u64) -> RemoteEntry {
        RemoteEntry {
            path: path.to_string(),
            etag: etag.to_string(),
            mtime,
            size,
        }
    }

    /// (path, etag) → 带 last_sync=SYNC_AT 的基线状态
    fn baseline(entries: &[(&str, &str)]) -> SyncState {
        let mut state = SyncState::default();
        for (path, etag) in entries {
            state.entries.insert(
                path.to_string(),
                SyncEntry {
                    etag: etag.to_string(),
                    local_rev: 3,
                    last_sync: Some(SYNC_AT.to_string()),
                },
            );
        }
        state
    }

    fn find<'a>(plan: &'a [SyncAction], path: &str) -> &'a SyncAction {
        plan.iter().find(|a| match a {
            SyncAction::Upload { path: p, .. }
            | SyncAction::Download { path: p, .. }
            | SyncAction::Conflict { path: p, .. }
            | SyncAction::Skip { path: p } => p == path,
        })
        .unwrap_or_else(|| panic!("path {path} not in plan"))
    }

    // ---- plan_sync：三分支 + 新增/删除/无基线 -----------------------------

    #[test]
    fn plan_local_only_is_upload_without_if_match() {
        let plan = plan_sync(&[local("a.md", 1000, 10)], &[], &SyncState::default());
        assert_eq!(
            plan,
            vec![SyncAction::Upload { path: "a.md".into(), remote_etag: None }]
        );
    }

    #[test]
    fn plan_remote_only_is_download() {
        let plan = plan_sync(&[], &[remote("b.md", "\"e-b\"", 1000, 20)], &SyncState::default());
        assert_eq!(
            plan,
            vec![SyncAction::Download {
                path: "b.md".into(),
                remote_etag: "\"e-b\"".into(),
                remote_mtime: 1000
            }]
        );
    }

    #[test]
    fn plan_only_local_changed_uploads_with_optimistic_lock() {
        let state = baseline(&[("a.md", "\"e1\"")]);
        // 本地 mtime 晚于 last_sync，ETag 未变
        let plan = plan_sync(&[local("a.md", sync_ms() + 3_600_000, 10)], &[remote("a.md", "\"e1\"", sync_ms() - 3_600_000, 10)], &state);
        assert_eq!(
            find(&plan, "a.md"),
            &SyncAction::Upload {
                path: "a.md".into(),
                remote_etag: Some("\"e1\"".into())
            }
        );
    }

    #[test]
    fn plan_only_remote_changed_downloads() {
        let state = baseline(&[("a.md", "\"e1\"")]);
        // 本地 mtime 早于 last_sync（未改），远端 ETag 变化
        let plan = plan_sync(&[local("a.md", sync_ms() - 3_600_000, 10)], &[remote("a.md", "\"e2\"", sync_ms() - 1_000, 12)], &state);
        assert_eq!(
            find(&plan, "a.md"),
            &SyncAction::Download {
                path: "a.md".into(),
                remote_etag: "\"e2\"".into(),
                remote_mtime: sync_ms() - 1_000
            }
        );
    }

    #[test]
    fn plan_both_changed_is_conflict_newer_wins() {
        let state = baseline(&[("a.md", "\"e1\"")]);
        let newer_local = sync_ms() + 7_200_000;
        let older_remote = sync_ms() + 3_600_000;
        let plan = plan_sync(
            &[local("a.md", newer_local, 10)],
            &[remote("a.md", "\"e2\"", older_remote, 12)],
            &state,
        );
        match find(&plan, "a.md") {
            SyncAction::Conflict { path, local_newer, remote_etag, remote_mtime } => {
                assert_eq!(path, "a.md");
                assert!(*local_newer, "本地 mtime 更新 → 本地为主版本");
                assert_eq!(remote_etag, "\"e2\"");
                assert_eq!(*remote_mtime, older_remote);
            }
            other => panic!("expected conflict, got {other:?}"),
        }

        // 远端更新 → local_newer = false
        let plan2 = plan_sync(
            &[local("a.md", older_remote, 10)],
            &[remote("a.md", "\"e3\"", newer_local, 12)],
            &state,
        );
        match find(&plan2, "a.md") {
            SyncAction::Conflict { local_newer, .. } => assert!(!local_newer),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn plan_unchanged_both_is_skip() {
        let state = baseline(&[("a.md", "\"e1\"")]);
        let plan = plan_sync(
            &[local("a.md", sync_ms() - 3_600_000, 10)],
            &[remote("a.md", "\"e1\"", sync_ms() - 3_600_000, 10)],
            &state,
        );
        assert_eq!(plan, vec![SyncAction::Skip { path: "a.md".into() }]);

        // 基线缺失但大小与 mtime 完全一致：视为同一版本
        let plan2 = plan_sync(
            &[local("new.md", 1234, 42)],
            &[remote("new.md", "\"ex\"", 1234, 42)],
            &SyncState::default(),
        );
        assert_eq!(plan2, vec![SyncAction::Skip { path: "new.md".into() }]);
    }

    #[test]
    fn plan_both_exist_without_baseline_is_conflict() {
        let plan = plan_sync(
            &[local("x.md", 2000, 10)],
            &[remote("x.md", "\"e\"", 1000, 99)],
            &SyncState::default(),
        );
        match find(&plan, "x.md") {
            SyncAction::Conflict { local_newer, .. } => assert!(local_newer),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn plan_remote_change_falls_back_to_mtime_without_etag() {
        // 服务端不返回 ETag（空）：用远端 mtime 与 last_sync 比较
        let state = baseline(&[("a.md", "")]);
        // 服务端不返回 ETag（空）：用远端 mtime 与 last_sync 比较
        let plan = plan_sync(
            &[local("a.md", sync_ms() - 7_200_000, 10)],
            &[remote("a.md", "", sync_ms() + 3_600_000, 10)],
            &state,
        );
        assert!(matches!(find(&plan, "a.md"), SyncAction::Download { .. }));

        let plan_same = plan_sync(
            &[local("a.md", sync_ms() - 7_200_000, 10)],
            &[remote("a.md", "", sync_ms() - 7_200_000, 10)],
            &state,
        );
        assert_eq!(plan_same, vec![SyncAction::Skip { path: "a.md".into() }]);
    }

    #[test]
    fn plan_is_deterministically_sorted() {
        let plan = plan_sync(
            &[local("b.md", 1, 1), local("a.md", 1, 1), local("docs/c.md", 1, 1)],
            &[],
            &SyncState::default(),
        );
        let paths: Vec<&str> = plan
            .iter()
            .map(|a| match a {
                SyncAction::Upload { path, .. } => path.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(paths, vec!["a.md", "b.md", "docs/c.md"]);
    }

    #[test]
    fn plan_stale_state_entry_for_missing_file_is_ignored() {
        // 本地/远端都不存在但状态里有条目：不产生动作
        let state = baseline(&[("gone.md", "\"e\"")]);
        let plan = plan_sync(&[], &[], &state);
        assert!(plan.is_empty());
    }

    // ---- sync-state 读写 ---------------------------------------------------

    #[test]
    fn state_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let mut state = SyncState {
            server: SERVER.into(),
            last_run: Some(SYNC_AT.into()),
            entries: BTreeMap::new(),
        };
        state.entries.insert(
            "docs/中文笔记.md".into(),
            SyncEntry {
                etag: "\"33a4-5c\"".into(),
                local_rev: 12,
                last_sync: Some(SYNC_AT.into()),
            },
        );
        save_state(&root, &state).unwrap();
        assert!(root.join(".monote").join("sync-state.json").exists());
        let loaded = load_state(&root).unwrap();
        assert_eq!(loaded.server, SERVER);
        assert_eq!(loaded.last_run.as_deref(), Some(SYNC_AT));
        assert_eq!(loaded.entries.get("docs/中文笔记.md").unwrap().etag, "\"33a4-5c\"");
        assert_eq!(loaded.entries.get("docs/中文笔记.md").unwrap().local_rev, 12);
    }

    #[test]
    fn state_missing_or_corrupt_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        assert!(load_state(&root).unwrap().entries.is_empty(), "不存在 → 默认");

        std::fs::create_dir_all(root.join(".monote")).unwrap();
        std::fs::write(root.join(".monote/sync-state.json"), "{ broken json").unwrap();
        let state = load_state(&root).unwrap();
        assert!(state.entries.is_empty(), "损坏 → 空状态，不阻断同步");
    }

    // ---- 路径 / URL / 207 解析 --------------------------------------------

    #[test]
    fn percent_encode_decode_roundtrip_chinese() {
        let rel = "docs/中文 笔记 #1.md";
        let encoded = encode_path(rel);
        assert!(!encoded.contains(' '), "空格必须编码：{encoded}");
        assert!(encoded.contains("%E4%B8%AD"), "中文必须编码：{encoded}");
        assert!(encoded.contains('/'), "目录分隔保留");
        assert_eq!(percent_decode(&encoded), rel);
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("bad%2"), "bad%2", "非法百分号序列原样保留");
    }

    #[test]
    fn href_to_rel_handles_absolute_and_relative_hrefs() {
        let base = "https://dav.example.com/monote/";
        let base_path = base_path_decoded(base);
        assert_eq!(base_path, "/monote/");
        assert_eq!(
            href_to_rel("/monote/docs/a.md", base, &base_path),
            Some(("docs/a.md".to_string(), false))
        );
        assert_eq!(
            href_to_rel("https://dav.example.com/monote/docs/a.md", base, &base_path),
            Some(("docs/a.md".to_string(), false))
        );
        assert_eq!(
            href_to_rel("docs/a.md", base, &base_path),
            Some(("docs/a.md".to_string(), false))
        );
        assert_eq!(
            href_to_rel("/monote/", base, &base_path),
            Some((String::new(), true)),
            "集合自身"
        );
        assert_eq!(
            href_to_rel("/monote/docs/", base, &base_path),
            Some(("docs".to_string(), true)),
            "子集合"
        );
        assert!(href_to_rel("/other/a.md", base, &base_path).is_none(), "集合外路径忽略");
    }

    #[test]
    fn parse_multistatus_extracts_files_and_dirs() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/monote/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/monote/docs/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
  </d:response>
  <d:response>
    <d:href>/monote/docs/%E7%AC%94%E8%AE%B0.md</d:href>
    <d:propstat><d:prop>
      <d:getetag>&quot;abc-123&quot;</d:getetag>
      <d:getlastmodified>Tue, 11 Mar 2025 10:00:00 GMT</d:getlastmodified>
      <d:getcontentlength>2048</d:getcontentlength>
      <d:resourcetype/>
    </d:prop></d:propstat>
  </d:response>
</d:multistatus>"#;
        let base = "https://dav.example.com/monote/";
        let raw = parse_multistatus(xml, base);
        assert_eq!(raw.len(), 3);
        let root = &raw[0];
        assert!(root.is_dir && root.path.is_empty());
        let docs = &raw[1];
        assert!(docs.is_dir);
        assert_eq!(docs.path, "docs");
        let file = &raw[2];
        assert!(!file.is_dir, "resourcetype 空 → 文件");
        assert_eq!(file.path, "docs/笔记.md");
        assert_eq!(file.etag, "\"abc-123\"", "XML 实体反转义");
        assert_eq!(file.size, 2048);
        assert!(file.mtime > 0, "getlastmodified 解析为 unix ms");
        // 2025-03-11T10:00:00Z
        assert_eq!(file.mtime, chrono::DateTime::parse_from_rfc3339("2025-03-11T10:00:00Z").unwrap().timestamp_millis());
    }

    #[test]
    fn sync_path_rules() {
        assert!(is_sync_path("a.md"));
        assert!(is_sync_path("docs/子目录/笔记.markdown"));
        assert!(is_sync_path("attachments/IMG-1.png"));
        assert!(!is_sync_path(".monote/index.db"));
        assert!(!is_sync_path(".trash/a.md"));
        assert!(!is_sync_path("docs/image.png"), "非 attachments 下的图片不同步");
        assert!(!is_sync_path("a.tmp-123"));
        assert!(!is_sync_path(""));
    }

    #[test]
    fn conflict_rel_avoids_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let first = unique_conflict_rel(&root, "docs/note.md", "20261102-120000");
        assert_eq!(first, "docs/note-conflict-20261102-120000.md");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join(rel_to_native(&first)), "old").unwrap();
        let second = unique_conflict_rel(&root, "docs/note.md", "20261102-120000");
        assert_eq!(second, "docs/note-conflict-20261102-120000-1.md");
        // 无扩展名文件补 .md（冲突副本作为笔记进入下一轮同步）
        assert_eq!(
            unique_conflict_rel(&root, "README", "t"),
            "README-conflict-t.md"
        );
    }

    #[test]
    fn local_collection_excludes_derived_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::create_dir_all(root.join(".monote")).unwrap();
        std::fs::create_dir_all(root.join(".trash")).unwrap();
        std::fs::write(root.join("docs/a.md"), "# a").unwrap();
        std::fs::write(root.join("attachments/img.png"), [1u8, 2, 3]).unwrap();
        std::fs::write(root.join(".monote/index.db"), "db").unwrap();
        std::fs::write(root.join(".trash/x.md"), "trash").unwrap();
        std::fs::write(root.join("docs/other.txt"), "txt").unwrap();

        let entries = collect_local_entries(&root).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["attachments/img.png", "docs/a.md"]);
        let note = entries.iter().find(|e| e.path == "docs/a.md").unwrap();
        assert_eq!(note.size, 3);
        assert!(note.mtime > 0);
        assert_eq!(note.hash.len(), 16, "xxhash64 十六进制");
    }
}
