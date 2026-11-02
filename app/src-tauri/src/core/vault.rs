//! M2 库管理模块 Vault（对应设计文档 4.2、5.1、5.4、5.7、7.1）。
//!
//! 唯一文件写入口：
//! - 路径校验（防 `../` 逃逸，安全设计 9.4）；
//! - 原子写（临时文件 + fsync + rename，DD-7）；
//! - 回收站 `.trash/`（含 `meta.json`，C-3）；
//! - 目录扫描（供索引对账/外部改动检测）；
//! - 附件读写；
//! - 残留临时文件清理。
//!
//! 本模块**不**理解 Markdown 语义（front-matter/双链由 M3 处理），也**不**直接操作索引（发事件由 M4 消费）。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use chrono::{Local, SecondsFormat, Utc};

use crate::error::{code, AppError, Result};
use crate::types::{TrashEntry, TreeNode};

/// 附件目录名（设计文档 A.4）。
const ATTACHMENTS_DIR: &str = "attachments";
/// 回收站目录名。
const TRASH_DIR: &str = ".trash";
/// 派生数据目录名。
const META_DIR: &str = ".monote";
/// 回收站元数据文件（库内相对路径）。
const TRASH_META_REL: &str = ".trash/meta.json";
/// 回收站保留天数（FR-205 / AC-012）。
const TRASH_RETENTION_DAYS: i64 = 30;
/// 单张附件上限（FR-103：≤ 10 MB）。
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// 目录树读取 front-matter 的头部字节上限。
const TITLE_HEAD_LIMIT: usize = 16 * 1024;
const DAY_MS: i64 = 86_400_000;

/// 库根句柄。
#[derive(Debug, Clone)]
pub struct Vault {
    pub(crate) root: PathBuf,
}

/// 目录扫描得到的原始文件信息。
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// 库内相对路径（`/` 分隔）
    pub rel: String,
    pub abs: PathBuf,
    pub mtime: i64,
    pub size: u64,
}

/// 回收站 meta.json 条目（设计文档 7.1 / C-3）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashMeta {
    pub id: String,
    /// 原库内相对路径
    pub path: String,
    /// 入站时间（ISO 8601 含时区）
    pub deleted_at: String,
    /// 回收站内的实际存储相对路径（相对库根）
    pub stored: String,
    pub is_dir: bool,
    pub size: u64,
}

impl Vault {
    /// 打开（或初始化）一个库：创建 `attachments/`、`.trash/`、`.monote/`，
    /// 校验可写。`root` 不存在时创建。
    pub fn open(root: &Path) -> Result<Vault> {
        if root.as_os_str().is_empty() {
            return Err(AppError::invalid_arg("库路径不能为空"));
        }
        if !root.exists() {
            fs::create_dir_all(root)
                .map_err(|e| AppError::io_ctx(format!("创建库目录失败：{}", root.display()), &e))?;
        }
        if !root.is_dir() {
            return Err(AppError::invalid_arg(format!(
                "库路径不是文件夹：{}",
                root.display()
            )));
        }
        let root = strip_verbatim_prefix(
            root.canonicalize()
                .map_err(|e| AppError::io_ctx(format!("解析库路径失败：{}", root.display()), &e))?,
        );

        for dir in [ATTACHMENTS_DIR, TRASH_DIR, META_DIR] {
            let path = root.join(dir);
            if !path.exists() {
                fs::create_dir_all(&path)
                    .map_err(|e| AppError::io_ctx(format!("初始化库目录失败：{}", path.display()), &e))?;
            }
        }

        let vault = Vault { root };

        // 可写校验：.monote 下写探针文件（X-3：库目录只读应在打开时暴露）
        let probe = vault
            .root
            .join(META_DIR)
            .join(format!(".write-probe-{}-{}", std::process::id(), short_rand()));
        if let Err(e) = fs::write(&probe, b"ok") {
            return Err(AppError::io_ctx("库目录不可写", &e));
        }
        let _ = fs::remove_file(&probe);

        // 回收站元数据初始化（损坏时自动降级为空表并保留现场）
        if !vault.trash_meta_path().exists() {
            vault.save_trash_meta(&Vec::new())?;
        }

        Ok(vault)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Vault".into())
    }

    // =======================================================================
    // 路径
    // =======================================================================

    /// 校验并解析库内相对路径 → 绝对路径（拒绝绝对路径、`..`、符号链接逃逸）。
    pub fn abs_path(&self, rel: &str) -> Result<PathBuf> {
        let normalized = normalize_rel(rel)?;
        if normalized.is_empty() {
            return Ok(self.root.clone());
        }
        let candidate = self.root.join(normalized);
        self.confine(&candidate)
    }

    /// 与 [`Self::abs_path`] 类似，但对**末段文件名保留调用方大小写**（只对父目录做
    /// canonicalize）。用于重命名/新建：Windows 大小写不敏感，canonicalize 会还原成
    /// 磁盘上的旧大小写，导致“仅改大小写”的重命名失效。
    fn abs_path_exact(&self, rel: &str) -> Result<PathBuf> {
        let normalized = normalize_rel(rel)?;
        if normalized.is_empty() {
            return Ok(self.root.clone());
        }
        let candidate = self.root.join(&normalized);
        let parent = candidate
            .parent()
            .ok_or_else(|| AppError::invalid_arg("无效路径"))?;
        let name = candidate
            .file_name()
            .ok_or_else(|| AppError::invalid_arg("无效路径"))?;
        let confined_parent = if parent == self.root {
            self.root.clone()
        } else if parent.as_os_str().is_empty() {
            return Err(AppError::path_escape());
        } else {
            self.confine(parent)?
        };
        Ok(confined_parent.join(name))
    }

    /// 绝对路径 → 库内相对路径（`/` 分隔）。
    pub fn rel_path(&self, abs: &Path) -> Result<String> {
        let full = if abs.is_absolute() {
            abs.to_path_buf()
        } else {
            self.root.join(abs)
        };
        let canonical = strip_verbatim_prefix(full.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::not_found(&full.display().to_string())
            } else {
                AppError::io_ctx("解析路径失败", &e)
            }
        })?);
        let rel = canonical
            .strip_prefix(&self.root)
            .map_err(|_| AppError::path_escape())?;
        Ok(path_to_rel(rel))
    }

    /// 把候选绝对路径限制在库根内：对最近的已存在祖先做 canonicalize（解析符号链接），
    /// 确认仍在库根内后再拼回不存在的尾部组件。
    fn confine(&self, candidate: &Path) -> Result<PathBuf> {
        let mut existing = candidate;
        let mut suffix: Vec<&OsStr> = Vec::new();
        while !existing.exists() {
            let Some(name) = existing.file_name() else {
                return Err(AppError::path_escape());
            };
            suffix.push(name);
            match existing.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => existing = parent,
                _ => return Err(AppError::path_escape()),
            }
        }
        let canonical = strip_verbatim_prefix(
            existing
                .canonicalize()
                .map_err(|e| AppError::io_ctx("解析库内路径失败", &e))?,
        );
        if !canonical.starts_with(&self.root) {
            return Err(AppError::path_escape());
        }
        let mut result = canonical;
        for part in suffix.iter().rev() {
            result.push(part);
        }
        Ok(result)
    }

    // =======================================================================
    // 目录树 / 读
    // =======================================================================

    /// 目录树（`depth=None` 全量）。文件夹在前、名称排序；忽略 `.monote/`、`.trash/`、`.git/`
    /// 与全部隐藏文件/目录。`depth` 表示最多展开的层级（1 = 仅顶层）。
    pub fn list_tree(&self, depth: Option<usize>) -> Result<Vec<TreeNode>> {
        let remaining = depth.map(|d| d.max(1));
        self.build_tree(&self.root, "", remaining)
    }

    fn build_tree(
        &self,
        dir: &Path,
        base_rel: &str,
        depth_remaining: Option<usize>,
    ) -> Result<Vec<TreeNode>> {
        let mut folders: Vec<(String, TreeNode)> = Vec::new();
        let mut notes: Vec<(String, TreeNode)> = Vec::new();
        let entries = fs::read_dir(dir)
            .map_err(|e| AppError::io_ctx(format!("读取目录失败：{}", dir.display()), &e))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue; // 隐藏文件/目录（含 .monote/.trash/.git）
            }
            let child_rel = if base_rel.is_empty() {
                name.clone()
            } else {
                format!("{base_rel}/{name}")
            };
            let abs = entry.path();
            if file_type.is_dir() {
                let (children, truncated) = match depth_remaining {
                    Some(1) => (Some(Vec::new()), has_visible_entries(&abs)),
                    Some(n) => (Some(self.build_tree(&abs, &child_rel, Some(n - 1))?), false),
                    None => (Some(self.build_tree(&abs, &child_rel, None)?), false),
                };
                folders.push((
                    name.to_lowercase(),
                    TreeNode {
                        path: child_rel,
                        name,
                        kind: "folder".into(),
                        title: None,
                        mtime: None,
                        children,
                        truncated,
                    },
                ));
            } else if file_type.is_file() && is_note_name(&name) {
                let title = self.note_title_fast(&abs, &child_rel);
                let mtime = mtime_ms(&abs).unwrap_or(0);
                notes.push((
                    name.to_lowercase(),
                    TreeNode {
                        path: child_rel,
                        name,
                        kind: "note".into(),
                        title: Some(title),
                        mtime: Some(mtime),
                        children: None,
                        truncated: false,
                    },
                ));
            }
        }
        folders.sort_by(|a, b| a.0.cmp(&b.0));
        notes.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out: Vec<TreeNode> = folders.into_iter().map(|(_, node)| node).collect();
        out.extend(notes.into_iter().map(|(_, node)| node));
        Ok(out)
    }

    /// 读取笔记标题：只读文件头部（front-matter 在文件开头），避免目录树加载全库正文。
    fn note_title_fast(&self, abs: &Path, rel: &str) -> String {
        let head = match fs::File::open(abs) {
            Ok(mut file) => {
                let mut buf = vec![0u8; TITLE_HEAD_LIMIT];
                match std::io::Read::read(&mut file, &mut buf) {
                    Ok(n) => String::from_utf8_lossy(&buf[..n]).into_owned(),
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
        };
        let meta = crate::core::links::read_meta(&head);
        crate::core::links::display_title(meta.title.as_deref(), rel)
    }

    /// 读取笔记原文与 mtime。
    pub fn read_note_raw(&self, rel: &str) -> Result<(String, i64)> {
        let abs = self.abs_path(rel)?;
        let metadata = fs::metadata(&abs).map_err(|e| path_io_err(rel, "读取笔记失败", &e))?;
        if !metadata.is_file() {
            return Err(AppError::not_found(rel));
        }
        let bytes = fs::read(&abs).map_err(|e| path_io_err(rel, "读取笔记失败", &e))?;
        let content = decode_text(&bytes);
        let mtime = mtime_ms(&abs)?;
        Ok((content, mtime))
    }

    /// 原子写入（临时文件 + fsync + rename），返回写入后的 mtime（unix ms）。
    /// 其余模块**必须**经由此函数写库内文件。
    pub fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<i64> {
        let abs = self.abs_path(rel)?;
        let parent = abs
            .parent()
            .ok_or_else(|| AppError::invalid_arg("无效的写入路径"))?
            .to_path_buf();
        if !parent.exists() {
            fs::create_dir_all(&parent)
                .map_err(|e| AppError::io_ctx(format!("创建目录失败：{}", parent.display()), &e))?;
        }
        if abs.is_dir() {
            return Err(AppError::invalid_arg("目标路径是文件夹"));
        }
        let file_name = abs
            .file_name()
            .ok_or_else(|| AppError::invalid_arg("无效的写入路径"))?;
        let tmp = parent.join(format!(
            "{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            short_rand()
        ));
        let write_result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &abs)
        })();
        if let Err(e) = write_result {
            let _ = fs::remove_file(&tmp);
            return Err(AppError::io_ctx(format!("写入失败：{rel}"), &e));
        }
        mtime_ms(&abs)
    }

    // =======================================================================
    // 新建 / 重命名
    // =======================================================================

    /// 新建笔记（内容为空或已给定；不覆盖已存在文件，冲突返回 1003）。
    pub fn create_note(&self, rel: &str, content: &str) -> Result<()> {
        let abs = self.abs_path(rel)?;
        if abs.exists() {
            return Err(AppError::name_conflict(rel));
        }
        let parent = abs
            .parent()
            .ok_or_else(|| AppError::invalid_arg("无效的笔记路径"))?
            .to_path_buf();
        if !parent.exists() {
            fs::create_dir_all(&parent)
                .map_err(|e| AppError::io_ctx(format!("创建目录失败：{}", parent.display()), &e))?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&abs)
        {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .map_err(|e| path_io_err(rel, "写入笔记失败", &e))?;
                file.sync_all()
                    .map_err(|e| path_io_err(rel, "写入笔记失败", &e))?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(AppError::name_conflict(rel))
            }
            Err(e) => Err(path_io_err(rel, "创建笔记失败", &e)),
        }
    }

    /// 新建文件夹（幂等）。
    pub fn create_folder(&self, rel: &str) -> Result<()> {
        if normalize_rel(rel)?.is_empty() {
            return Ok(()); // 库根已存在，幂等
        }
        let abs = self.abs_path(rel)?;
        if abs.exists() {
            return if abs.is_dir() {
                Ok(())
            } else {
                Err(AppError::name_conflict(rel))
            };
        }
        fs::create_dir_all(&abs)
            .map_err(|e| AppError::io_ctx(format!("创建文件夹失败：{rel}"), &e))
    }

    /// 重命名/移动文件或文件夹（目标已存在返回 1003；文件夹移动需保证目标父目录存在）。
    pub fn rename_path(&self, rel: &str, new_rel: &str) -> Result<()> {
        let src = self.abs_path(rel)?;
        if !src.exists() {
            return Err(AppError::not_found(rel));
        }
        if src == self.root {
            return Err(AppError::invalid_arg("不能重命名库根目录"));
        }
        let dst = self.abs_path_exact(new_rel)?;
        if src == dst {
            return Ok(());
        }
        // 防止把文件夹移入自身内部
        if src.is_dir() && dst.starts_with(&src) {
            return Err(AppError::invalid_arg("不能把文件夹移动到自身内部"));
        }
        let same_file = src
            .canonicalize()
            .ok()
            .zip(dst.canonicalize().ok())
            .map_or(false, |(a, b)| a == b);
        if dst.exists() && !same_file {
            return Err(AppError::name_conflict(new_rel));
        }
        if let Some(parent) = dst.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    AppError::io_ctx(format!("创建目标目录失败：{}", parent.display()), &e)
                })?;
            }
        }
        if same_file {
            // 大小写重命名（Windows 大小写不敏感）：先经临时名再落到目标名
            let parent = dst
                .parent()
                .ok_or_else(|| AppError::invalid_arg("无效的目标路径"))?
                .to_path_buf();
            let tmp = parent.join(format!(".rename-{}-{}", std::process::id(), short_rand()));
            rename_with_retry(&src, &tmp)
                .map_err(|e| path_io_err(rel, "重命名失败", &e))?;
            if let Err(e) = rename_with_retry(&tmp, &dst) {
                let _ = rename_with_retry(&tmp, &src);
                return Err(path_io_err(new_rel, "重命名失败", &e));
            }
            return Ok(());
        }
        rename_with_retry(&src, &dst).map_err(|e| path_io_err(rel, "重命名失败", &e))
    }

    // =======================================================================
    // 回收站
    // =======================================================================

    /// 移入回收站（笔记或文件夹）。文件夹递归移动其内全部文件并逐条登记 meta。
    /// 返回本次新增的 trashId 列表（文件夹删除时首条为主条目）。
    pub fn trash_path(&self, rel: &str) -> Result<Vec<String>> {
        let src = self.abs_path(rel)?;
        if src == self.root {
            return Err(AppError::invalid_arg("不能删除库根目录"));
        }
        if !src.exists() {
            return Err(AppError::not_found(rel));
        }
        let is_dir = src.is_dir();
        let original_rel = self.rel_path(&src)?;
        if original_rel.is_empty() {
            return Err(AppError::invalid_arg("不能删除库根目录"));
        }
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| AppError::invalid_arg("无效的删除路径"))?;
        let deleted_at = now_iso();
        let main_id = uuid::Uuid::new_v4().to_string();
        let stored_rel = format!("{TRASH_DIR}/{main_id}/{name}");

        // 先枚举文件夹内容，移动后路径不再可见
        let mut entries: Vec<TrashMeta> = Vec::new();
        if is_dir {
            for (inside_rel, size) in enumerate_folder_files(&src) {
                entries.push(TrashMeta {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: format!("{original_rel}/{inside_rel}"),
                    deleted_at: deleted_at.clone(),
                    stored: format!("{stored_rel}/{inside_rel}"),
                    is_dir: false,
                    size,
                });
            }
        }
        let main_size = if is_dir {
            0
        } else {
            fs::metadata(&src).map(|m| m.len()).unwrap_or(0)
        };
        entries.insert(
            0,
            TrashMeta {
                id: main_id.clone(),
                path: original_rel.clone(),
                deleted_at,
                stored: stored_rel,
                is_dir,
                size: main_size,
            },
        );

        let item_dir = self.root.join(TRASH_DIR).join(&main_id);
        fs::create_dir_all(&item_dir).map_err(|e| {
            AppError::io_ctx(format!("创建回收站目录失败：{}", item_dir.display()), &e)
        })?;
        let stored_abs = item_dir.join(&name);
        if let Err(e) = rename_with_retry(&src, &stored_abs) {
            let _ = fs::remove_dir_all(&item_dir);
            return Err(path_io_err(rel, "移入回收站失败", &e));
        }

        // 登记 meta；失败时回滚移动，避免文件“丢失”在回收站里
        let mut meta = self.load_trash_meta()?;
        meta.extend(entries.iter().cloned());
        if let Err(e) = self.save_trash_meta(&meta) {
            let _ = rename_with_retry(&stored_abs, &src);
            let _ = fs::remove_dir_all(&item_dir);
            return Err(e);
        }
        Ok(entries.into_iter().map(|entry| entry.id).collect())
    }

    /// 回收站列表（含剩余天数计算，FR-205）。只展示顶层条目（文件夹内部的逐条登记不重复展示）。
    pub fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        let meta = self.load_trash_meta()?;
        let dirs: Vec<&TrashMeta> = meta.iter().filter(|entry| entry.is_dir).collect();
        let mut items: Vec<TrashEntry> = Vec::new();
        for entry in &meta {
            if entry.stored.is_empty() || entry.path.is_empty() {
                continue;
            }
            let Ok(stored_abs) = self.abs_path(&entry.stored) else {
                continue;
            };
            if !stored_abs.exists() {
                continue; // 存储已不在（外部清理）：跳过而不阻塞列表
            }
            let is_child = dirs.iter().any(|dir| {
                dir.stored != entry.stored && entry.stored.starts_with(&format!("{}/", dir.stored))
            });
            if is_child {
                continue;
            }
            let leaf = entry
                .path
                .rsplit('/')
                .next()
                .unwrap_or(entry.path.as_str());
            // 与前端回收站展示一致：文件显示去扩展名的名称，文件夹显示目录名
            let name = if entry.is_dir {
                leaf.to_string()
            } else {
                Path::new(leaf)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_else(|| leaf.to_string())
            };
            items.push(TrashEntry {
                id: entry.id.clone(),
                original_path: entry.path.clone(),
                name,
                deleted_at: entry.deleted_at.clone(),
                remaining_days: remaining_days(&entry.deleted_at),
                is_dir: entry.is_dir,
                size: entry.size,
            });
        }
        items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        Ok(items)
    }

    /// 还原一条回收站条目：路径已存在时自动重命名为 `<原名>-还原-<时间戳>`。
    /// 返回 `(还原后的相对路径, 实际重命名后的路径 Option)`。
    pub fn restore_trash(&self, id: &str) -> Result<(String, Option<String>)> {
        let mut meta = self.load_trash_meta()?;
        let pos = meta
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| AppError::new(code::NOTE_NOT_FOUND, "回收站条目不存在").with_detail(id))?;
        let entry = meta[pos].clone();
        let stored_abs = self.abs_path(&entry.stored)?;
        if !stored_abs.exists() {
            meta.remove(pos);
            let _ = self.save_trash_meta(&meta);
            return Err(AppError::not_found(&entry.path));
        }
        let original_abs = self.abs_path(&entry.path)?;
        let (final_abs, was_renamed) = if original_abs.exists() {
            (self.unique_restore_path(&original_abs, entry.is_dir)?, true)
        } else {
            (original_abs, false)
        };
        if let Some(parent) = final_abs.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    AppError::io_ctx(format!("创建还原目录失败：{}", parent.display()), &e)
                })?;
            }
        }
        if let Err(e) = rename_with_retry(&stored_abs, &final_abs) {
            return Err(path_io_err(&entry.path, "还原失败", &e));
        }
        let final_rel = self.rel_path(&final_abs)?;

        // 入口条目与其全部子条目一并移出 meta
        let child_prefix = format!("{}/", entry.stored);
        meta.retain(|item| {
            item.id != entry.id
                && !(item.stored != entry.stored && item.stored.starts_with(&child_prefix))
        });
        self.save_trash_meta(&meta)?;

        // 清掉回收站里的空目录（子条目仍在时 remove_dir 会失败，正好保留）
        if let Some(item_root) = self.trash_item_root(&entry.stored) {
            let _ = fs::remove_dir(&item_root);
        }
        let renamed = if was_renamed {
            Some(final_rel.clone())
        } else {
            None
        };
        Ok((final_rel, renamed))
    }

    /// 清空回收站（永久删除），返回清除条目数。
    pub fn empty_trash(&self) -> Result<u32> {
        let trash_dir = self.root.join(TRASH_DIR);
        let mut cleared = 0u32;
        if trash_dir.exists() {
            let entries = fs::read_dir(&trash_dir)
                .map_err(|e| AppError::io_ctx("读取回收站失败", &e))?;
            for entry in entries.flatten() {
                if entry.file_name() == OsStr::new("meta.json") {
                    continue;
                }
                let path = entry.path();
                let removed = if path.is_dir() {
                    fs::remove_dir_all(&path).is_ok()
                } else {
                    fs::remove_file(&path).is_ok()
                };
                if removed {
                    cleared += 1;
                }
            }
        }
        self.save_trash_meta(&Vec::new())?;
        Ok(cleared)
    }

    /// 启动时清除超过 30 天的回收站条目（FR-205）。
    pub fn purge_expired_trash(&self) -> Result<u32> {
        let meta = self.load_trash_meta()?;
        if meta.is_empty() {
            return Ok(0);
        }
        let dirs: Vec<TrashMeta> = meta.iter().filter(|entry| entry.is_dir).cloned().collect();
        let now = Utc::now();
        let mut remove_ids: HashSet<String> = HashSet::new();
        let mut storage_roots: Vec<PathBuf> = Vec::new();
        let mut purged = 0u32;

        for entry in &meta {
            let is_child = dirs.iter().any(|dir| {
                dir.stored != entry.stored && entry.stored.starts_with(&format!("{}/", dir.stored))
            });
            if is_child {
                continue;
            }
            let stored_abs = self.abs_path(&entry.stored).ok();
            let existed = stored_abs.as_ref().map(|p| p.exists()).unwrap_or(false);
            let expired = parse_iso(&entry.deleted_at)
                .map(|deleted| (now - deleted.with_timezone(&Utc)).num_seconds()
                    >= TRASH_RETENTION_DAYS * 86_400)
                .unwrap_or(false);
            if existed && !expired {
                continue;
            }
            if expired {
                purged += 1;
            }
            remove_ids.insert(entry.id.clone());
            if let Some(root) = self.trash_item_root(&entry.stored) {
                storage_roots.push(root);
            }
            // 该顶层条目内部的子条目一并移除
            let prefix = format!("{}/", entry.stored);
            for child in &meta {
                if child.stored != entry.stored && child.stored.starts_with(&prefix) {
                    remove_ids.insert(child.id.clone());
                }
            }
        }

        if remove_ids.is_empty() {
            return Ok(0);
        }
        for root in storage_roots {
            if root.starts_with(self.root.join(TRASH_DIR)) {
                let _ = fs::remove_dir_all(&root);
            }
        }
        let kept: Vec<TrashMeta> = meta
            .into_iter()
            .filter(|entry| !remove_ids.contains(&entry.id))
            .collect();
        self.save_trash_meta(&kept)?;
        Ok(purged)
    }

    fn trash_meta_path(&self) -> PathBuf {
        self.root.join(TRASH_DIR).join("meta.json")
    }

    /// 读取 `.trash/meta.json`；不存在返回空表，损坏时保留现场（重命名为 `.corrupt-<ts>`）并返回空表。
    fn load_trash_meta(&self) -> Result<Vec<TrashMeta>> {
        let path = self.trash_meta_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(AppError::io_ctx("读取回收站元数据失败", &e)),
        };
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Vec::new());
        }
        match serde_json::from_slice::<Vec<TrashMeta>>(&bytes) {
            Ok(meta) => Ok(meta),
            Err(_) => {
                let backup = self
                    .root
                    .join(TRASH_DIR)
                    .join(format!("meta.json.corrupt-{}", short_rand()));
                let _ = fs::rename(&path, &backup);
                Ok(Vec::new())
            }
        }
    }

    fn save_trash_meta(&self, meta: &[TrashMeta]) -> Result<()> {
        let trash_dir = self.root.join(TRASH_DIR);
        if !trash_dir.exists() {
            fs::create_dir_all(&trash_dir)
                .map_err(|e| AppError::io_ctx("创建回收站目录失败", &e))?;
        }
        let bytes = serde_json::to_vec_pretty(meta)
            .map_err(|e| AppError::new(code::INVALID_ARG, "序列化回收站元数据失败").with_detail(e.to_string()))?;
        self.write_atomic(TRASH_META_REL, &bytes)?;
        Ok(())
    }

    /// 由 stored 相对路径推出该条目的回收站存储根（`.trash/<id>`）。
    fn trash_item_root(&self, stored: &str) -> Option<PathBuf> {
        let mut parts = stored.split('/');
        let first = parts.next()?;
        let second = parts.next()?;
        if first != TRASH_DIR || second.is_empty() || second == "." || second == ".." {
            return None;
        }
        Some(self.root.join(first).join(second))
    }

    /// 生成不冲突的 `<原名>-还原-<时间戳>[.ext]`（X-14）。
    fn unique_restore_path(&self, original: &Path, is_dir: bool) -> Result<PathBuf> {
        let parent = original
            .parent()
            .ok_or_else(|| AppError::invalid_arg("无效的还原路径"))?;
        let name = original
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| AppError::invalid_arg("无效的还原路径"))?;
        let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
        for attempt in 0..1000u32 {
            let candidate_name = if is_dir {
                if attempt == 0 {
                    format!("{name}-还原-{ts}")
                } else {
                    format!("{name}-还原-{ts}-{attempt}")
                }
            } else {
                let (stem, ext) = split_name_ext(&name);
                if attempt == 0 {
                    format!("{stem}-还原-{ts}{ext}")
                } else {
                    format!("{stem}-还原-{ts}-{attempt}{ext}")
                }
            };
            let candidate = parent.join(candidate_name);
            if !candidate.exists() {
                return Ok(candidate);
            }
        }
        Err(AppError::name_conflict(&original.display().to_string()))
    }

    // =======================================================================
    // 扫描（索引对账 / 同步 / 备份）
    // =======================================================================

    /// 扫描库内全部笔记（`*.md`，忽略隐藏目录与 `.monote/`、`.trash/`）。
    pub fn scan_notes(&self) -> Result<Vec<ScannedFile>> {
        self.scan_files(|rel, name| is_note_name(name) && !rel_has_reserved_dir(rel))
    }

    /// 扫描参与同步/备份的文件（笔记 + attachments，排除 `.monote`）。
    pub fn scan_sync_files(&self) -> Result<Vec<ScannedFile>> {
        self.scan_files(|rel, _| !rel_has_reserved_dir(rel))
    }

    /// 递归扫描库内普通文件（跳过隐藏文件/目录；不跟随符号链接）。
    fn scan_files(&self, filter: impl Fn(&str, &str) -> bool) -> Result<Vec<ScannedFile>> {
        let mut out: Vec<ScannedFile> = Vec::new();
        let walker = walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden_name(entry.file_name()));
        for entry in walker {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&self.root) else {
                continue;
            };
            let rel = path_to_rel(relative);
            if !filter(&rel, &name) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            out.push(ScannedFile {
                rel,
                abs: entry.path().to_path_buf(),
                mtime: metadata
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                size: metadata.len(),
            });
        }
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(out)
    }

    // =======================================================================
    // 附件 / 任意文件
    // =======================================================================

    /// 保存剪贴板图片到 `attachments/`，返回库内相对路径。
    /// 命名 `IMG-<ts>-<rand>.<ext>`，文件名安全化（FR-103）。
    pub fn save_attachment(&self, filename: &str, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() {
            return Err(AppError::invalid_arg("附件内容为空"));
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AppError::invalid_arg("附件超过 10 MB 上限"));
        }
        let ext = attachment_ext(filename, bytes);
        let dir = self.root.join(ATTACHMENTS_DIR);
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .map_err(|e| AppError::io_ctx("创建附件目录失败", &e))?;
        }
        let ts = Utc::now().timestamp_millis();
        for _ in 0..10 {
            let name = format!("IMG-{ts}-{}.{ext}", short_rand());
            let rel = format!("{ATTACHMENTS_DIR}/{name}");
            if self.abs_path(&rel)?.exists() {
                continue;
            }
            self.write_atomic(&rel, bytes)?;
            return Ok(rel);
        }
        Err(AppError::name_conflict("attachments"))
    }

    /// 读取任意库内文件字节（用于附件/图片内联）。
    pub fn read_file_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        let abs = self.abs_path(rel)?;
        let metadata = fs::metadata(&abs).map_err(|e| path_io_err(rel, "读取文件失败", &e))?;
        if !metadata.is_file() {
            return Err(AppError::not_found(rel));
        }
        fs::read(&abs).map_err(|e| path_io_err(rel, "读取文件失败", &e))
    }

    /// 清理崩溃残留的 `*.tmp-*` 文件（启动时调用，X-1）。返回清理数量。
    pub fn cleanup_temp_files(&self) -> Result<u32> {
        let mut removed = 0u32;
        let walker = walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter();
        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if !is_temp_file_name(&name) {
                continue;
            }
            if fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    // =======================================================================
    // 名称解析（双链补全 / 缺失链接重建）
    // =======================================================================

    /// 笔记名解析表：`(展示标题, 相对路径, mtime)`，按 mtime 降序（FR-203 补全排序）。
    /// 展示标题取 front-matter title，缺省取文件名去扩展名（与 [`crate::core::links::display_title`] 一致）。
    pub fn name_index(&self) -> Result<Vec<(String, String, i64)>> {
        let notes = self.scan_notes()?;
        let mut items: Vec<(String, String, i64)> = notes
            .into_iter()
            .map(|file| {
                let title = self.note_title_fast(&file.abs, &file.rel);
                (title, file.rel, file.mtime)
            })
            .collect();
        items.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        Ok(items)
    }

    /// 是否存在同名笔记（含 `.trash/` 内同名，供 FR-203 缺失链接重建提示）。
    /// 库内笔记返回其库内相对路径；回收站条目返回其 `.trash/` 存储路径，便于调用方识别。
    pub fn find_by_name(&self, name: &str) -> Result<Vec<String>> {
        let target = crate::core::links::normalize_target(name).to_lowercase();
        if target.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<String> = Vec::new();
        for file in self.scan_notes()? {
            let stem = Path::new(&file.rel)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if stem == target {
                out.push(file.rel);
            }
        }
        for entry in self.load_trash_meta().unwrap_or_default() {
            let stem = Path::new(&entry.path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if stem == target && !entry.stored.is_empty() && !out.contains(&entry.stored) {
                out.push(entry.stored);
            }
        }
        Ok(out)
    }
}

// ===========================================================================
// 辅助函数
// ===========================================================================

/// 去掉 Windows `canonicalize()` 产生的 `\\?\` 前缀（便于 UI 展示与配置持久化）：
/// - `\\?\C:\dir` → `C:\dir`
/// - `\\?\UNC\server\share` → `\\server\share`
///
/// 非 Windows 平台原样返回。
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            if let Some(unc) = rest.strip_prefix("UNC\\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            if rest.as_bytes().len() >= 2 && rest.as_bytes()[1] == b':' {
                return PathBuf::from(rest);
            }
        }
    }
    path
}

/// 规范化并校验库内相对路径：拒绝绝对路径、盘符、UNC、`..`、ADS/保留设备名与
/// 以点/空格结尾的组件；返回 `/` 分隔的规范化相对路径（空串表示库根）。
fn normalize_rel(rel: &str) -> Result<String> {
    if rel.contains('\0') {
        return Err(AppError::invalid_arg("路径包含非法字符"));
    }
    let unified = rel.replace('\\', "/");
    if unified.starts_with('/') {
        return Err(AppError::path_escape());
    }
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(AppError::path_escape());
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(AppError::path_escape());
        }
        if part.contains(':') {
            return Err(AppError::path_escape());
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return Err(AppError::invalid_arg("路径组件不能以空格或点结尾"));
        }
        if is_windows_reserved_name(Path::new(part).file_stem().unwrap_or(OsStr::new(part))) {
            return Err(AppError::invalid_arg("路径包含系统保留名"));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn is_windows_reserved_name(stem: &OsStr) -> bool {
    let stem = stem.to_string_lossy().to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

fn path_to_rel(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// 判断路径是否落在 `.monote/` 或 `.trash/` 下（防御纵深；隐藏目录通常已被遍历剪枝）。
fn rel_has_reserved_dir(rel: &str) -> bool {
    rel.split('/')
        .any(|part| part == META_DIR || part == TRASH_DIR)
}

fn is_note_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn has_visible_entries(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return false;
            }
            match entry.file_type() {
                Ok(file_type) => {
                    file_type.is_dir() || (file_type.is_file() && is_note_name(&name))
                }
                Err(_) => false,
            }
        }),
        Err(_) => false,
    }
}

fn mtime_ms(path: &Path) -> Result<i64> {
    let metadata = fs::metadata(path)
        .map_err(|e| AppError::io_ctx(format!("读取文件属性失败：{}", path.display()), &e))?;
    Ok(metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0))
}

fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn path_io_err(rel: &str, context: &str, err: &std::io::Error) -> AppError {
    if err.kind() == std::io::ErrorKind::NotFound {
        AppError::not_found(rel)
    } else {
        AppError::io_ctx(format!("{context}：{rel}"), err)
    }
}

/// 重命名重试：Windows 下文件被短暂占用（EBUSY/EACCES）时重试 3 次（X-9）。
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..3 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "rename failed")))
}

fn now_iso() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn parse_iso(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok()
}

/// 距 30 天自动清除的剩余天数：正数向上取整，0 = 今日到期，负数 = 已过期。
fn remaining_days(deleted_at: &str) -> i64 {
    let Some(deleted) = parse_iso(deleted_at) else {
        return 0;
    };
    let expire_ms = deleted.timestamp_millis() + TRASH_RETENTION_DAYS * DAY_MS;
    let remaining = expire_ms - Utc::now().timestamp_millis();
    if remaining > 0 {
        (remaining + DAY_MS - 1) / DAY_MS
    } else {
        -((-remaining + DAY_MS - 1) / DAY_MS)
    }
}

fn split_name_ext(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(idx) if idx > 0 && idx + 1 < name.len() => {
            (name[..idx].to_string(), name[idx..].to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

fn enumerate_folder_files(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, prefix)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let inside = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if file_type.is_dir() {
                stack.push((path, inside));
            } else if file_type.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((inside, size));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn short_rand() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

/// 判断文件名是否为本模块原子写产生的临时文件：`<name>.tmp-<pid>-<rand8>`。
fn is_temp_file_name(name: &str) -> bool {
    let Some(idx) = name.rfind(".tmp-") else {
        return false;
    };
    let tail = &name[idx + ".tmp-".len()..];
    let mut parts = tail.splitn(2, '-');
    let Some(pid) = parts.next() else { return false };
    let Some(rand) = parts.next() else { return false };
    !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && rand.len() == 8
        && rand.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 附件扩展名：优先取文件名扩展名（安全化），否则按内容魔数嗅探。
fn attachment_ext(filename: &str, bytes: &[u8]) -> String {
    let from_name = Path::new(filename)
        .extension()
        .map(|ext| {
            ext.to_string_lossy()
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(10)
                .collect::<String>()
        })
        .unwrap_or_default();
    if !from_name.is_empty() {
        return from_name;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "png".into();
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return "jpg".into();
    }
    if bytes.starts_with(b"GIF8") {
        return "gif".into();
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "webp".into();
    }
    if bytes.starts_with(b"BM") {
        return "bmp".into();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    if head.contains("<svg") {
        return "svg".into();
    }
    "bin".into()
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        (dir, vault)
    }

    fn find_node<'a>(nodes: &'a [TreeNode], path: &str) -> Option<&'a TreeNode> {
        for node in nodes {
            if node.path == path {
                return Some(node);
            }
            if let Some(children) = &node.children {
                if let Some(found) = find_node(children, path) {
                    return Some(found);
                }
            }
        }
        None
    }

    // ---- 打开 / 路径校验 --------------------------------------------------

    #[test]
    fn open_creates_dirs_and_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("我的库");
        let vault = Vault::open(&root).unwrap();
        assert!(vault.root().join("attachments").is_dir());
        assert!(vault.root().join(".trash").is_dir());
        assert!(vault.root().join(".monote").is_dir());
        assert!(vault.root().join(".trash").join("meta.json").is_file());
        assert_eq!(vault.name(), "我的库");
        // Windows canonicalize 的 \\?\ 前缀应被剥离（避免 UI/配置里出现奇怪前缀）
        #[cfg(windows)]
        assert!(!vault.root().to_string_lossy().starts_with(r"\\?\"));
        // 二次打开幂等
        let again = Vault::open(&root).unwrap();
        assert_eq!(again.root(), vault.root());
    }

    #[test]
    fn abs_path_rejects_escape() {
        let (_dir, vault) = temp_vault();
        assert_eq!(
            vault.abs_path("../越界.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.abs_path("a/../../b.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.abs_path("..\\越界.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.abs_path("/etc/passwd").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.abs_path("C:\\Windows\\win.ini").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.abs_path("\\\\server\\share\\a.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
        // 保留设备名
        assert!(vault.abs_path("NUL.md").is_err());
        // 正常路径：反斜杠被统一成 '/'
        assert_eq!(vault.abs_path("").unwrap(), vault.root());
        assert_eq!(
            vault.abs_path("sub\\笔记.md").unwrap(),
            vault.root().join("sub").join("笔记.md")
        );
        assert_eq!(
            vault.abs_path("sub/./笔记.md").unwrap(),
            vault.root().join("sub").join("笔记.md")
        );
    }

    #[test]
    fn rel_path_roundtrip_and_escape() {
        let (dir, vault) = temp_vault();
        vault.create_note("a/b.md", "x").unwrap();
        let abs = vault.root().join("a").join("b.md");
        assert_eq!(vault.rel_path(&abs).unwrap(), "a/b.md");
        assert_eq!(vault.rel_path(vault.root()).unwrap(), "");
        // 不存在的路径 → 1002
        assert_eq!(
            vault
                .rel_path(&vault.root().join("不存在.md"))
                .unwrap_err()
                .code,
            code::NOTE_NOT_FOUND
        );
        // 库外路径 → 1001
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(
            vault.rel_path(outside.path()).unwrap_err().code,
            code::PATH_ESCAPE
        );
        drop(dir);
    }

    // ---- 原子写 / 读 ------------------------------------------------------

    #[test]
    fn write_atomic_roundtrip_and_no_temp_left() {
        let (_dir, vault) = temp_vault();
        let mtime = vault
            .write_atomic("笔记.md", "内容\n第二行".as_bytes())
            .unwrap();
        assert!(mtime > 0);
        let (content, read_mtime) = vault.read_note_raw("笔记.md").unwrap();
        assert_eq!(content, "内容\n第二行");
        assert_eq!(read_mtime, mtime);
        assert_eq!(vault.cleanup_temp_files().unwrap(), 0);
        // 无残留临时文件
        let leftovers: Vec<_> = fs::read_dir(vault.root())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        // 覆盖写
        vault.write_atomic("笔记.md", "新内容".as_bytes()).unwrap();
        assert_eq!(vault.read_note_raw("笔记.md").unwrap().0, "新内容");
    }

    #[test]
    fn write_atomic_creates_parents_and_cleans_stale_tmp() {
        let (_dir, vault) = temp_vault();
        vault.write_atomic("a/b/c.md", b"data").unwrap();
        assert_eq!(vault.read_note_raw("a/b/c.md").unwrap().0, "data");
        // 模拟崩溃残留
        let stale = vault.root().join("a/b").join("c.md.tmp-999-abcdef12");
        fs::write(&stale, b"half").unwrap();
        assert_eq!(vault.cleanup_temp_files().unwrap(), 1);
        assert!(!stale.exists());
        // 非本模块格式的文件不清理
        let normal = vault.root().join("a").join("keep.tmp-data");
        fs::write(&normal, b"x").unwrap();
        assert_eq!(vault.cleanup_temp_files().unwrap(), 0);
        assert!(normal.exists());
    }

    #[test]
    fn write_atomic_rejects_directory_target() {
        let (_dir, vault) = temp_vault();
        vault.create_folder("dir").unwrap();
        assert!(vault.write_atomic("dir", b"x").is_err());
        assert_eq!(
            vault.read_note_raw("../x.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
    }

    // ---- 新建 / 重命名 ----------------------------------------------------

    #[test]
    fn create_note_and_folder_conflicts() {
        let (_dir, vault) = temp_vault();
        vault.create_note("a.md", "内容").unwrap();
        assert_eq!(
            vault.create_note("a.md", "覆盖").unwrap_err().code,
            code::NAME_CONFLICT
        );
        assert_eq!(vault.read_note_raw("a.md").unwrap().0, "内容");
        // 文件夹幂等
        vault.create_folder("docs").unwrap();
        vault.create_folder("docs").unwrap();
        vault.create_folder("docs/子目录").unwrap();
        // 文件/目录同名冲突
        assert_eq!(
            vault.create_folder("a.md").unwrap_err().code,
            code::NAME_CONFLICT
        );
        assert_eq!(
            vault.create_note("docs", "").unwrap_err().code,
            code::NAME_CONFLICT
        );
        // 嵌套创建笔记自动建父目录
        vault.create_note("docs/深/层.md", "x").unwrap();
        assert_eq!(vault.read_note_raw("docs/深/层.md").unwrap().0, "x");
    }

    #[test]
    fn rename_file_folder_and_conflict() {
        let (_dir, vault) = temp_vault();
        vault.create_note("旧名.md", "内容").unwrap();
        vault.rename_path("旧名.md", "新名.md").unwrap();
        assert!(!vault.abs_path("旧名.md").unwrap().exists());
        assert_eq!(vault.read_note_raw("新名.md").unwrap().0, "内容");

        vault.create_note("占用.md", "占").unwrap();
        assert_eq!(
            vault.rename_path("新名.md", "占用.md").unwrap_err().code,
            code::NAME_CONFLICT
        );

        vault.create_folder("目录").unwrap();
        vault.create_note("目录/a.md", "a").unwrap();
        vault.rename_path("目录", "移动后").unwrap();
        assert_eq!(vault.read_note_raw("移动后/a.md").unwrap().0, "a");

        // 移入自身子目录
        vault.create_folder("外层").unwrap();
        assert_eq!(
            vault.rename_path("外层", "外层/内层").unwrap_err().code,
            code::INVALID_ARG
        );
        // 不存在
        assert_eq!(
            vault.rename_path("不存在.md", "x.md").unwrap_err().code,
            code::NOTE_NOT_FOUND
        );
    }

    #[test]
    fn read_note_raw_strips_bom_and_handles_invalid_utf8() {
        let (_dir, vault) = temp_vault();
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("# 带 BOM".as_bytes());
        vault.write_atomic("bom.md", &bom).unwrap();
        assert_eq!(vault.read_note_raw("bom.md").unwrap().0, "# 带 BOM");
        // 非法 UTF-8：宽松解码，保证编辑期不因外部文件崩溃
        vault.write_atomic("gbk.md", b"\xD6\xD0\xCE\xC4").unwrap();
        assert!(!vault.read_note_raw("gbk.md").unwrap().0.is_empty());
    }

    #[test]
    fn trash_errors() {
        let (_dir, vault) = temp_vault();
        assert_eq!(
            vault.trash_path("不存在.md").unwrap_err().code,
            code::NOTE_NOT_FOUND
        );
        assert_eq!(
            vault.trash_path("../越界.md").unwrap_err().code,
            code::PATH_ESCAPE
        );
        assert_eq!(
            vault.trash_path("").unwrap_err().code,
            code::INVALID_ARG
        );
        assert_eq!(
            vault.restore_trash("不存在的-id").unwrap_err().code,
            code::NOTE_NOT_FOUND
        );
    }

    #[test]
    #[cfg(windows)]
    fn rename_case_only_on_windows() {
        let (_dir, vault) = temp_vault();
        vault.create_note("Case.md", "x").unwrap();
        vault.rename_path("Case.md", "case.md").unwrap();
        let names: Vec<String> = fs::read_dir(vault.root())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"case.md".to_string()), "实际：{names:?}");
    }

    // ---- 回收站 -----------------------------------------------------------

    #[test]
    fn trash_file_list_restore_and_meta_schema() {
        let (_dir, vault) = temp_vault();
        vault.create_note("笔记.md", "内容").unwrap();
        let ids = vault.trash_path("笔记.md").unwrap();
        assert_eq!(ids.len(), 1);
        assert!(!vault.abs_path("笔记.md").unwrap().exists());

        let list = vault.list_trash().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].original_path, "笔记.md");
        assert_eq!(list[0].name, "笔记");
        assert_eq!(list[0].remaining_days, 30);
        assert!(!list[0].is_dir);
        assert_eq!(list[0].size, "内容".len() as u64);

        // meta.json 逐条字段（C-3）
        let meta_raw = fs::read_to_string(vault.root().join(".trash").join("meta.json")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_raw).unwrap();
        assert_eq!(meta[0]["id"].as_str().unwrap(), ids[0]);
        assert_eq!(meta[0]["path"].as_str().unwrap(), "笔记.md");
        assert_eq!(
            meta[0]["stored"].as_str().unwrap(),
            format!(".trash/{}/笔记.md", ids[0])
        );
        assert!(!meta[0]["isDir"].as_bool().unwrap());
        assert!(chrono::DateTime::parse_from_rfc3339(
            meta[0]["deletedAt"].as_str().unwrap()
        )
        .is_ok());

        let (path, renamed) = vault.restore_trash(&ids[0]).unwrap();
        assert_eq!(path, "笔记.md");
        assert!(renamed.is_none());
        assert_eq!(vault.read_note_raw("笔记.md").unwrap().0, "内容");
        assert!(vault.list_trash().unwrap().is_empty());
    }

    #[test]
    fn trash_folder_recursive_and_restore() {
        let (_dir, vault) = temp_vault();
        vault.create_folder("项目").unwrap();
        vault.create_note("项目/a.md", "a").unwrap();
        vault.create_folder("项目/子").unwrap();
        vault.create_note("项目/子/b.md", "bb").unwrap();

        let ids = vault.trash_path("项目").unwrap();
        assert_eq!(ids.len(), 3, "主条目 + 两个文件条目");
        assert!(!vault.abs_path("项目").unwrap().exists());

        // 列表只展示顶层文件夹条目
        let list = vault.list_trash().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_dir);
        assert_eq!(list[0].original_path, "项目");
        assert_eq!(list[0].size, 0);

        // meta 逐条登记了文件
        let meta = vault.load_trash_meta().unwrap();
        assert_eq!(meta.len(), 3);
        assert!(meta
            .iter()
            .any(|entry| entry.path == "项目/子/b.md" && !entry.is_dir && entry.size == 2));

        let (path, renamed) = vault.restore_trash(&ids[0]).unwrap();
        assert_eq!(path, "项目");
        assert!(renamed.is_none());
        assert_eq!(vault.read_note_raw("项目/子/b.md").unwrap().0, "bb");
        assert!(vault.list_trash().unwrap().is_empty());
    }

    #[test]
    fn restore_conflict_renames_with_suffix() {
        let (_dir, vault) = temp_vault();
        vault.create_note("冲突.md", "旧内容").unwrap();
        let ids = vault.trash_path("冲突.md").unwrap();
        vault.create_note("冲突.md", "新内容").unwrap();

        let (path, renamed) = vault.restore_trash(&ids[0]).unwrap();
        assert!(path.starts_with("冲突-还原-"), "实际路径：{path}");
        assert!(path.ends_with(".md"));
        assert_eq!(renamed.as_deref(), Some(path.as_str()));
        // 原路径保持新内容，还原内容进重命名文件（X-14）
        assert_eq!(vault.read_note_raw("冲突.md").unwrap().0, "新内容");
        assert_eq!(vault.read_note_raw(&path).unwrap().0, "旧内容");
        assert!(vault.list_trash().unwrap().is_empty());
    }

    #[test]
    fn purge_expired_trash_removes_only_old_entries() {
        let (_dir, vault) = temp_vault();
        vault.create_note("旧笔记.md", "旧").unwrap();
        let old_ids = vault.trash_path("旧笔记.md").unwrap();
        vault.create_note("新笔记.md", "新").unwrap();
        let new_ids = vault.trash_path("新笔记.md").unwrap();

        // 把旧条目时间改成 31 天前
        let mut meta = vault.load_trash_meta().unwrap();
        let mut old = (Utc::now() - chrono::Duration::days(31))
            .to_rfc3339_opts(SecondsFormat::Secs, false);
        for entry in &mut meta {
            if entry.id == old_ids[0] {
                entry.deleted_at = old.clone();
            }
        }
        vault.save_trash_meta(&meta).unwrap();
        // 29 天前 → 剩余 1 天
        old = (Utc::now() - chrono::Duration::days(29))
            .to_rfc3339_opts(SecondsFormat::Secs, false);
        let mut meta = vault.load_trash_meta().unwrap();
        for entry in &mut meta {
            if entry.id == new_ids[0] {
                entry.deleted_at = old.clone();
            }
        }
        vault.save_trash_meta(&meta).unwrap();

        assert_eq!(vault.purge_expired_trash().unwrap(), 1);
        let list = vault.list_trash().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].original_path, "新笔记.md");
        assert_eq!(list[0].remaining_days, 1);
        // 过期条目的存储已删除
        assert!(!vault
            .root()
            .join(".trash")
            .join(&old_ids[0])
            .exists());
    }

    #[test]
    fn empty_trash_clears_everything() {
        let (_dir, vault) = temp_vault();
        vault.create_note("一.md", "1").unwrap();
        vault.create_note("二.md", "2").unwrap();
        vault.trash_path("一.md").unwrap();
        vault.trash_path("二.md").unwrap();
        assert_eq!(vault.list_trash().unwrap().len(), 2);

        assert_eq!(vault.empty_trash().unwrap(), 2);
        assert!(vault.list_trash().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(vault.root().join(".trash").join("meta.json"))
                .unwrap()
                .trim(),
            "[]"
        );
        // .trash 目录本身保留
        assert!(vault.root().join(".trash").is_dir());
    }

    #[test]
    fn trash_meta_corrupt_is_quarantined() {
        let (_dir, vault) = temp_vault();
        fs::write(vault.root().join(".trash").join("meta.json"), b"{not json").unwrap();
        assert!(vault.list_trash().unwrap().is_empty());
        // 损坏文件被保留为 .corrupt-*，不会阻塞后续写入
        vault.create_note("a.md", "x").unwrap();
        vault.trash_path("a.md").unwrap();
        assert_eq!(vault.list_trash().unwrap().len(), 1);
    }

    // ---- 目录树 -----------------------------------------------------------

    #[test]
    fn list_tree_structure_and_titles() {
        let (_dir, vault) = temp_vault();
        vault.create_folder("b目录").unwrap();
        vault
            .create_note("b目录/内页.md", "---\ntitle: 内页标题\n---\n正文")
            .unwrap();
        vault.create_note("a笔记.md", "无元数据").unwrap();
        vault.create_note("z.md", "").unwrap();
        fs::write(vault.root().join(".hidden.md"), "隐藏").unwrap();
        vault.create_folder(".private").unwrap();

        let tree = vault.list_tree(None).unwrap();
        // 顶层：attachments、b目录（文件夹）+ a笔记、z（笔记）
        assert_eq!(tree.len(), 4);
        assert_eq!(tree[0].kind, "folder");
        assert!(tree.iter().all(|node| !node.path.starts_with('.')));
        assert!(find_node(&tree, ".hidden.md").is_none());
        assert!(find_node(&tree, ".private").is_none());

        let inner = find_node(&tree, "b目录").unwrap();
        assert!(inner.truncated == false);
        assert_eq!(inner.children.as_ref().unwrap().len(), 1);
        let note = find_node(&tree, "b目录/内页.md").unwrap();
        assert_eq!(note.kind, "note");
        assert_eq!(note.title.as_deref(), Some("内页标题"));
        assert!(note.mtime.unwrap() > 0);
        // 无 front-matter → 文件名去扩展名
        assert_eq!(
            find_node(&tree, "a笔记.md").unwrap().title.as_deref(),
            Some("a笔记")
        );

        // depth=1：文件夹不展开，标记 truncated
        let shallow = vault.list_tree(Some(1)).unwrap();
        let b = find_node(&shallow, "b目录").unwrap();
        assert!(b.children.as_ref().unwrap().is_empty());
        assert!(b.truncated);
        let attachments = find_node(&shallow, "attachments").unwrap();
        assert!(!attachments.truncated);
    }

    // ---- 扫描 -------------------------------------------------------------

    #[test]
    fn scan_notes_and_sync_files() {
        let (_dir, vault) = temp_vault();
        vault.create_note("根.md", "").unwrap();
        vault.create_note("sub/子.md", "").unwrap();
        fs::write(vault.root().join("sub").join("image.png"), b"png").unwrap();
        fs::write(vault.root().join(".monote").join("x.md"), "x").unwrap();
        fs::write(vault.root().join(".hidden.md"), "h").unwrap();
        vault.create_note("临时.md", "").unwrap();
        vault.trash_path("临时.md").unwrap();

        let notes = vault.scan_notes().unwrap();
        let rels: Vec<&str> = notes.iter().map(|file| file.rel.as_str()).collect();
        assert_eq!(rels, vec!["sub/子.md", "根.md"]);
        assert!(notes.iter().all(|file| file.size == 0));

        let sync_rels: Vec<String> = vault
            .scan_sync_files()
            .unwrap()
            .into_iter()
            .map(|file| file.rel)
            .collect();
        assert!(sync_rels.contains(&"sub/image.png".to_string()));
        assert!(sync_rels.contains(&"根.md".to_string()));
        assert!(!sync_rels.iter().any(|rel| rel.starts_with(".monote")));
        assert!(!sync_rels.iter().any(|rel| rel.starts_with(".trash")));
        assert!(!sync_rels.iter().any(|rel| rel.starts_with('.')));
    }

    // ---- 附件 -------------------------------------------------------------

    #[test]
    fn save_attachment_naming_and_read() {
        let (_dir, vault) = temp_vault();
        let png = b"\x89PNG\r\n\x1a\nsome-bytes".to_vec();
        let rel = vault.save_attachment("photo.PNG", &png).unwrap();
        assert!(rel.starts_with("attachments/IMG-"), "实际：{rel}");
        assert!(rel.ends_with(".png"));
        assert_eq!(vault.read_file_bytes(&rel).unwrap(), png);

        // 无扩展名 → 魔数嗅探
        let rel = vault.save_attachment("image", &png).unwrap();
        assert!(rel.ends_with(".png"));

        // 文件名安全化 + 默认 bin
        let rel = vault
            .save_attachment("奇怪<名称>?.dat", b"plain")
            .unwrap();
        assert!(rel.ends_with(".dat"), "实际：{rel}");

        // 超限
        let big = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        assert_eq!(
            vault.save_attachment("big.png", &big).unwrap_err().code,
            code::INVALID_ARG
        );
    }

    #[test]
    fn read_file_bytes_errors() {
        let (_dir, vault) = temp_vault();
        assert_eq!(
            vault.read_file_bytes("不存在.png").unwrap_err().code,
            code::NOTE_NOT_FOUND
        );
        assert_eq!(
            vault.read_file_bytes("../x.png").unwrap_err().code,
            code::PATH_ESCAPE
        );
        vault.create_folder("dir").unwrap();
        assert_eq!(
            vault.read_file_bytes("dir").unwrap_err().code,
            code::NOTE_NOT_FOUND
        );
    }

    // ---- 名称索引 ---------------------------------------------------------

    #[test]
    fn name_index_and_find_by_name() {
        let (_dir, vault) = temp_vault();
        vault.create_note("甲.md", "").unwrap();
        vault.create_note("dir/乙.md", "").unwrap();
        vault
            .create_note("dir/丙.md", "---\ntitle: 自定义标题\n---\n正文")
            .unwrap();

        let index = vault.name_index().unwrap();
        assert_eq!(index.len(), 3);
        assert!(index
            .iter()
            .any(|(name, path, mtime)| name == "乙" && path == "dir/乙.md" && *mtime > 0));
        // 展示标题优先取 front-matter title
        assert!(index
            .iter()
            .any(|(name, path, _)| name == "自定义标题" && path == "dir/丙.md"));

        assert_eq!(
            vault.find_by_name("乙.mdown").unwrap(),
            Vec::<String>::new()
        );
        assert!(vault.find_by_name("乙.md").unwrap().contains(&"dir/乙.md".to_string()));
        assert!(vault.find_by_name("不存在").unwrap().is_empty());

        // 回收站内同名也返回（FR-203 重建提示）；回收站条目为 `.trash/` 存储路径
        let trashed = vault.trash_path("甲.md").unwrap();
        let found = vault.find_by_name("甲").unwrap();
        assert!(
            found.iter().any(|p| p.starts_with(".trash/")),
            "回收站同名应返回 .trash 路径：{found:?}"
        );
        assert!(!found.contains(&"甲.md".to_string()));
        assert!(found.iter().any(|p| p.contains(&trashed[0])));
    }

    // ---- 过期计算 ---------------------------------------------------------

    #[test]
    fn remaining_days_calculation() {
        let now = Utc::now();
        let fresh = now.to_rfc3339_opts(SecondsFormat::Secs, false);
        assert_eq!(remaining_days(&fresh), 30);
        let almost = (now - chrono::Duration::days(30) + chrono::Duration::hours(1))
            .to_rfc3339_opts(SecondsFormat::Secs, false);
        assert_eq!(remaining_days(&almost), 1);
        let expired = (now - chrono::Duration::days(31))
            .to_rfc3339_opts(SecondsFormat::Secs, false);
        assert!(remaining_days(&expired) < 0);
        assert_eq!(remaining_days("not-a-date"), 0);
    }

    #[test]
    fn temp_file_name_matching() {
        assert!(is_temp_file_name("笔记.md.tmp-1234-abcdef12"));
        assert!(is_temp_file_name("x.tmp-0-0123abcd"));
        assert!(!is_temp_file_name("笔记.md"));
        assert!(!is_temp_file_name("a.tmp-data"));
        assert!(!is_temp_file_name("a.tmp-1234"));
        assert!(!is_temp_file_name("a.tmp-abc-abcdef12"));
    }
}
