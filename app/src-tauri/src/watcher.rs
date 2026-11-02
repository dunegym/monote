//! 文件系统 watcher（对应设计文档 4.2 Watcher、5.4 F-04、DD-9）。
//!
//! OS 原生事件 → 去抖 300ms → ExternalGuard 白名单过滤本应用写回 → 增量重索引 →
//! 事件总线推送 `notes:external` / `vault:changed`。
//!
//! watcher 线程只投递消息与做轻量处理；不持有 UI 锁做长时间 IO。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};
use parking_lot::Mutex;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::core::links;
use crate::state::Inner;

/// 启动 watcher（库根递归监听）。返回的 watcher 句柄需由调用方持有，drop 即停止。
pub fn start(
    app: AppHandle,
    inner: Arc<Mutex<Inner>>,
    root: PathBuf,
) -> Option<notify::RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .ok()?;
    if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
        return None;
    }

    std::thread::Builder::new()
        .name("monote-watcher".into())
        .spawn(move || {
            loop {
                let first = match rx.recv() {
                    Ok(r) => r,
                    Err(_) => break, // watcher/state dropped
                };
                let mut paths: Vec<PathBuf> = Vec::new();
                let mut collect = |ev: Event| {
                    for p in ev.paths {
                        paths.push(p);
                    }
                };
                if let Ok(ev) = first {
                    collect(ev);
                }
                // 去抖：300ms 静默窗口（设计文档 4.2 / F-04）
                while let Ok(next) = rx.recv_timeout(Duration::from_millis(300)) {
                    if let Ok(ev) = next {
                        collect(ev);
                    }
                }
                if paths.is_empty() {
                    continue;
                }
                let (changed, external) = process_batch(&inner, &paths);
                if changed {
                    let _ = app.emit("vault:changed", json!({ "reason": "external" }));
                    for (path, kind) in external {
                        let _ = app.emit("notes:external", json!({ "path": path, "kind": kind }));
                    }
                }
            }
        })
        .ok()?;

    Some(watcher)
}

/// 处理一批文件事件，返回 `(库内容是否有变化, 外部变更列表)`。
fn process_batch(inner: &Arc<Mutex<Inner>>, paths: &[PathBuf]) -> (bool, Vec<(String, String)>) {
    let mut guard = inner.lock();
    let Some(vault) = guard.vault.clone() else {
        return (false, Vec::new());
    };
    let mut changed = false;
    let mut external: Vec<(String, String)> = Vec::new();
    for p in paths {
        let rel = match vault.rel_path(p) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel = rel.replace('\\', "/");
        // 派生数据/回收站/临时文件不参与外部变更检测
        if rel.starts_with(".monote/") || rel.starts_with(".monote") || rel.starts_with(".trash") {
            continue;
        }
        if rel.contains(".tmp-") || rel.ends_with('~') || rel.ends_with(".swp") {
            continue;
        }
        if rel.ends_with(".md") || rel.ends_with(".markdown") {
            match vault.read_note_raw(&rel) {
                Ok((content, mtime)) => {
                    let hash = links::content_hash(&content);
                    // 本应用写回：白名单命中 → 忽略（ExternalGuard）
                    if guard.self_writes.get(&rel) == Some(&hash) {
                        guard.self_writes.remove(&rel);
                        continue;
                    }
                    guard.self_writes.remove(&rel);
                    let parsed = links::parse_note(&rel, &content, mtime, content.len() as u64);
                    if let Some(index) = guard.index.as_mut() {
                        let _ = index.reindex_note(&parsed);
                    }
                    changed = true;
                    external.push((rel, "modified".into()));
                }
                Err(_) => {
                    // 文件被外部删除：软删索引
                    if let Some(index) = guard.index.as_mut() {
                        let _ = index.set_deleted(&rel, true);
                    }
                    changed = true;
                    external.push((rel, "deleted".into()));
                }
            }
        } else if rel.starts_with("attachments/") {
            changed = true;
            external.push((rel, "modified".into()));
        }
    }
    (changed, external)
}
