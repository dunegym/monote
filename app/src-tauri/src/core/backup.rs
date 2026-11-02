//! M6 备份子服务 BKP（对应设计文档 4.6、5.6）。
//!
//! 规则（FR-502）：
//! - 启动时若距上次备份 > 24h 自动备份；
//! - 文件名 `monote-backup-YYYYMMDD-HHmmss.zip`，内容为库全量快照（含 `.trash/`，排除 `.monote/`）；
//! - 仅保留最近 `keep`（默认 5）份；
//! - 失败经通知中心 error 提示，不影响主功能。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::core::io::zip_snapshot;
use crate::error::{AppError, Result};
use crate::types::{BackupEntry, BackupResult};

/// 备份文件名前缀。
const BACKUP_PREFIX: &str = "monote-backup-";
/// 自动备份间隔（FR-502：24 小时）。
const BACKUP_INTERVAL_HOURS: i64 = 24;

/// 执行一次备份（打包 + 轮转保留策略）。
///
/// 流程（F-06）：创建备份目录 → 库快照 ZIP → 写入 `monote-backup-<ts>.zip` →
/// 轮转（保留最近 `keep` 份）→ 返回结果。打包失败会清理半成品 zip（磁盘不足场景）。
pub fn run_backup(vault_root: &Path, backup_dir: &Path, keep: u32) -> Result<BackupResult> {
    if !vault_root.is_dir() {
        return Err(AppError::invalid_arg(format!("库目录不存在：{}", vault_root.display())));
    }
    std::fs::create_dir_all(backup_dir)
        .map_err(|e| AppError::io_ctx(format!("创建备份目录失败：{}", backup_dir.display()), &e))?;

    let now = chrono::Local::now();
    let out = unique_backup_path(backup_dir, now);

    // 打包（复用 M5 ZipPackager）。失败清理半成品，避免磁盘不足时留下坏 zip。
    let size = match zip_snapshot(vault_root, &out) {
        Ok(size) => size,
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            return Err(e);
        }
    };

    // 轮转失败不影响本次备份成功（设计 5.6：轮转属清理动作，可下轮重试）。
    if let Err(e) = rotate(backup_dir, keep) {
        eprintln!("[MoNote] 备份轮转失败：{e}");
    }
    let kept = list_backups(backup_dir)?.len() as u32;
    Ok(BackupResult {
        file: absolute_display(&out),
        size,
        kept,
    })
}

/// 列出备份目录内全部备份（按 mtime 降序）。
///
/// 目录不存在时返回空列表（首次启动/备份目录尚未创建，属正常状态而非错误）。
pub fn list_backups(backup_dir: &Path) -> Result<Vec<BackupEntry>> {
    let read = match std::fs::read_dir(backup_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(AppError::io_ctx(
                format!("读取备份目录失败：{}", backup_dir.display()),
                &e,
            ))
        }
    };
    let mut items = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_backup_name(&name) {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        items.push(BackupEntry {
            file: absolute_display(&path),
            size: meta.len(),
            mtime: mtime_ms(&meta),
        });
    }
    items.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| b.file.cmp(&a.file)));
    Ok(items)
}

/// 是否应触发自动备份（上次备份为 None 或早于 24 小时）。
///
/// ISO 8601（含时区）解析失败时保守返回 true（宁可多备份，不丢数据）。
pub fn should_backup(last_backup_at: Option<&str>, now: chrono::DateTime<chrono::Local>) -> bool {
    let last = match last_backup_at {
        None => return true,
        Some(s) => match parse_iso_local(s) {
            Some(dt) => dt,
            None => return true,
        },
    };
    now.signed_duration_since(last) > chrono::Duration::hours(BACKUP_INTERVAL_HOURS)
}

/// 轮转保留策略：保留最近 `keep` 份，删除其余；返回删除数量。
pub fn rotate(backup_dir: &Path, keep: u32) -> Result<u32> {
    let backups = list_backups(backup_dir)?;
    let mut deleted = 0u32;
    for entry in backups.into_iter().skip(keep as usize) {
        match std::fs::remove_file(Path::new(&entry.file)) {
            Ok(()) => deleted += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!("[MoNote] 删除旧备份失败（{}）：{e}", entry.file);
            }
        }
    }
    Ok(deleted)
}

/// 默认备份文件名（`monote-backup-YYYYMMDD-HHmmss.zip`）。
pub fn backup_file_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!("{BACKUP_PREFIX}{}.zip", now.format("%Y%m%d-%H%M%S"))
}

// ===========================================================================
// 私有辅助
// ===========================================================================

fn is_backup_name(name: &str) -> bool {
    name.starts_with(BACKUP_PREFIX)
        && name.to_ascii_lowercase().ends_with(".zip")
        && name.len() > BACKUP_PREFIX.len() + 4
}

/// 同一秒内重复触发备份时避免覆盖：追加 `-N` 序号。
fn unique_backup_path(backup_dir: &Path, now: chrono::DateTime<chrono::Local>) -> PathBuf {
    let name = backup_file_name(now);
    let first = backup_dir.join(&name);
    if !first.exists() {
        return first;
    }
    let stem = name.trim_end_matches(".zip");
    for n in 1..1000u32 {
        let cand = backup_dir.join(format!("{stem}-{n}.zip"));
        if !cand.exists() {
            return cand;
        }
    }
    first
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 解析 ISO 8601（含时区）；兼容无时区的本地时间串。
fn parse_iso_local(s: &str) -> Option<chrono::DateTime<chrono::Local>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Local));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return naive.and_local_timezone(chrono::Local).single();
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return naive.and_local_timezone(chrono::Local).single();
    }
    None
}

/// 备份文件的绝对路径展示（清理 Windows `\\?\` 前缀，JSON 里可读）。
fn absolute_display(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        }
    });
    let s = abs.to_string_lossy().to_string();
    s.strip_prefix(r"\\?\").map(|v| v.to_string()).unwrap_or(s)
}

/// 文件系统 mtime → 秒级 SystemTime（测试辅助，亦供轮转/命名测试使用）。
#[cfg(test)]
fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).expect("open for set_mtime");
    f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs)).expect("set mtime");
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local_now() -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(2026, 11, 2, 19, 30, 5).single().expect("valid local time")
    }

    #[test]
    fn backup_file_name_has_expected_format() {
        assert_eq!(backup_file_name(local_now()), "monote-backup-20261102-193005.zip");
        // 文件名符合 FR-502 规定格式（可排序、可解析）
        let re = regex::Regex::new(r"^monote-backup-\d{8}-\d{6}\.zip$").unwrap();
        assert!(re.is_match(&backup_file_name(chrono::Local::now())));
    }

    #[test]
    fn should_backup_threshold_is_24h() {
        let now = local_now();
        assert!(should_backup(None, now), "从未备份过 → 应备份");
        let recent = (now - chrono::Duration::hours(23)).to_rfc3339();
        assert!(!should_backup(Some(&recent), now), "23h 前备份 → 不备份");
        let exactly = (now - chrono::Duration::hours(24)).to_rfc3339();
        assert!(!should_backup(Some(&exactly), now), "恰好 24h 不算超过");
        let old = (now - chrono::Duration::hours(25)).to_rfc3339();
        assert!(should_backup(Some(&old), now), "25h 前备份 → 应备份");
        assert!(should_backup(Some("not-a-timestamp"), now), "损坏时间戳 → 保守备份");
        let future = (now + chrono::Duration::hours(3)).to_rfc3339();
        assert!(!should_backup(Some(&future), now), "未来时间戳 → 不备份");
    }

    #[test]
    fn list_and_rotate_keep_recent_five() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        for i in 0..7u64 {
            let name = format!("monote-backup-2026010{}-000000.zip", i + 1);
            let p = backup_dir.join(&name);
            std::fs::write(&p, format!("zip-{i}")).unwrap();
            set_mtime(&p, 1_700_000_000 + i);
        }
        // 无关文件不应被当作备份
        std::fs::write(backup_dir.join("notes.md"), "x").unwrap();
        std::fs::write(backup_dir.join("monote-backup-corrupt.txt"), "x").unwrap();

        let listed = list_backups(&backup_dir).unwrap();
        assert_eq!(listed.len(), 7, "只统计 monote-backup-*.zip");
        assert!(listed[0].mtime >= listed[6].mtime, "按 mtime 降序");

        let deleted = rotate(&backup_dir, 5).unwrap();
        assert_eq!(deleted, 2, "7 份保留 5 份 → 删 2");
        let left = list_backups(&backup_dir).unwrap();
        assert_eq!(left.len(), 5);
        assert!(left.iter().any(|b| b.file.ends_with("monote-backup-20260106-000000.zip")));
        assert!(!left.iter().any(|b| b.file.ends_with("monote-backup-20260101-000000.zip")));
        // 再次轮转无删除
        assert_eq!(rotate(&backup_dir, 5).unwrap(), 0);
    }

    #[test]
    fn list_backups_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir");
        assert!(list_backups(&missing).unwrap().is_empty());
    }

    #[test]
    fn run_backup_creates_zip_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let vault_root = dir.path().join("vault");
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(vault_root.join(".trash")).unwrap();
        std::fs::create_dir_all(vault_root.join(".monote")).unwrap();
        std::fs::write(vault_root.join("a.md"), "# 备份").unwrap();
        std::fs::write(vault_root.join(".trash/meta.json"), "[]").unwrap();
        std::fs::write(vault_root.join(".monote/index.db"), "db").unwrap();

        let res = run_backup(&vault_root, &backup_dir, 5).unwrap();
        assert!(res.size > 0);
        assert_eq!(res.kept, 1);
        assert!(Path::new(&res.file).is_absolute(), "备份文件应为绝对路径");
        assert!(res.file.contains("monote-backup-"));
        assert!(Path::new(&res.file).exists());
        let names = list_backups(&backup_dir).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].size, res.size);
    }
}
