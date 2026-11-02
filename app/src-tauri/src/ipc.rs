//! IPC 命令层（对应设计文档 6.2）。
//!
//! 每个命令只做三件事：参数解析/默认值、调用 [`crate::state::AppState`] 的编排方法、
//! 把 `Result` 包成统一 envelope（`{ok:true,data}` / `{ok:false,error}`）。
//! 命令名为 Rust 函数名（snake_case）；入参由 Tauri 默认按 camelCase 从 JS 映射。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::error::{err, ok, Result};
use crate::state::AppState;
use crate::types::SearchFilters;

/// 统一 envelope 包装。
fn resp<T: Serialize>(r: Result<T>) -> Value {
    match r {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

// ===========================================================================
// 库与笔记
// ===========================================================================

#[tauri::command]
pub fn vault_open(state: State<'_, AppState>, root_path: String) -> Value {
    resp(state.open_vault(Path::new(&root_path)))
}

#[tauri::command]
pub fn vault_pick(state: State<'_, AppState>) -> Value {
    let Some(dir) = dialog_pick_dir("选择笔记库文件夹") else {
        return resp(Ok::<_, crate::error::AppError>(Value::Null));
    };
    resp(state.open_vault(&dir).map(|v| serde_json::json!(v)))
}

#[tauri::command]
pub fn vault_info(state: State<'_, AppState>) -> Value {
    resp(Ok(state.vault_info()))
}

#[tauri::command]
pub fn vault_list(state: State<'_, AppState>, depth: Option<usize>) -> Value {
    resp(state.list_tree(depth).map(|nodes| serde_json::json!({ "nodes": nodes })))
}

#[tauri::command]
pub fn vault_read(state: State<'_, AppState>, path: String) -> Value {
    resp(state.read_note(&path))
}

#[tauri::command]
pub fn vault_write(
    state: State<'_, AppState>,
    path: String,
    content: String,
    expect_rev: Option<u64>,
) -> Value {
    resp(state.write_note(&path, &content, expect_rev))
}

#[tauri::command]
pub fn vault_rename(
    state: State<'_, AppState>,
    path: String,
    new_path: String,
    rewrite_links: Option<bool>,
) -> Value {
    resp(state.rename(&path, &new_path, rewrite_links.unwrap_or(true)))
}

#[tauri::command]
pub fn vault_trash(state: State<'_, AppState>, path: String) -> Value {
    resp(state.trash(&path))
}

#[tauri::command]
pub fn vault_trash_list(state: State<'_, AppState>) -> Value {
    resp(state.trash_list())
}

#[tauri::command]
pub fn vault_trash_restore(state: State<'_, AppState>, trash_id: String) -> Value {
    resp(state.trash_restore(&trash_id))
}

#[tauri::command]
pub fn vault_empty_trash(state: State<'_, AppState>) -> Value {
    resp(state.empty_trash())
}

#[tauri::command]
pub fn vault_create_note(
    state: State<'_, AppState>,
    name: Option<String>,
    parent: Option<String>,
) -> Value {
    resp(state.create_note(name.as_deref(), parent.as_deref()))
}

#[tauri::command]
pub fn vault_create_folder(state: State<'_, AppState>, path: String) -> Value {
    resp(state.create_folder(&path))
}

#[tauri::command]
pub fn vault_create_empty(state: State<'_, AppState>, name: String) -> Value {
    resp(state.create_empty_note(&name))
}

#[tauri::command]
pub fn vault_save_attachment(
    state: State<'_, AppState>,
    file_name: String,
    data_base64: String,
) -> Value {
    resp(state.save_attachment(&file_name, &data_base64))
}

#[tauri::command]
pub fn vault_image_data(state: State<'_, AppState>, path: String) -> Value {
    resp(state.image_data(&path))
}

// ===========================================================================
// 链接 / 标签
// ===========================================================================

#[tauri::command]
pub fn link_suggest(state: State<'_, AppState>, prefix: Option<String>) -> Value {
    resp(state.suggest(prefix.as_deref().unwrap_or("")).map(|items| serde_json::json!({ "items": items })))
}

#[tauri::command]
pub fn link_backlinks(state: State<'_, AppState>, path: String) -> Value {
    resp(state.backlinks(&path).map(|items| serde_json::json!({ "items": items })))
}

#[tauri::command]
pub fn tag_list(state: State<'_, AppState>) -> Value {
    resp(state.tags().map(|tags| serde_json::json!({ "tags": tags })))
}

// ===========================================================================
// 检索
// ===========================================================================

#[tauri::command]
pub fn search_query(
    state: State<'_, AppState>,
    kind: String,
    term: String,
    filters: Option<SearchFilters>,
    sort: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Value {
    resp(state.search(
        &kind,
        &term,
        filters.unwrap_or_default(),
        sort.as_deref().unwrap_or("relevance"),
        page.unwrap_or(0),
        page_size.unwrap_or(20).min(50),
    ))
}

#[tauri::command]
pub fn index_rebuild(state: State<'_, AppState>) -> Value {
    resp(state.rebuild_index())
}

#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> Value {
    resp(Ok(state.index_status()))
}

// ===========================================================================
// 导入导出
// ===========================================================================

#[tauri::command]
pub fn io_import(
    state: State<'_, AppState>,
    src_path: Option<String>,
    target_dir: Option<String>,
) -> Value {
    let Some(src) = src_path else {
        return err(crate::error::AppError::invalid_arg("未选择导入源"));
    };
    resp(state.import(Path::new(&src), target_dir.as_deref()))
}

#[tauri::command]
pub fn io_export_note(
    state: State<'_, AppState>,
    path: String,
    fmt: String,
    out_path: Option<String>,
    rendered_html: Option<String>,
) -> Value {
    // PDF 走 WebView 打印管线（DD-6），由前端 `window.print()` 处理，不在此命令内。
    if fmt == "pdf" {
        return err(crate::error::AppError::invalid_arg(
            "PDF 导出由前端打印管线处理（window.print）",
        ));
    }
    let out: Option<PathBuf> = match out_path {
        Some(p) => Some(PathBuf::from(p)),
        None => {
            let default_name = format!(
                "{}.{}",
                path.rsplit('/').next().unwrap_or("note").trim_end_matches(".md"),
                if fmt == "html" { "html" } else { "md" }
            );
            match dialog_save_file("导出笔记", &default_name, &[fmt.as_str()]) {
                Some(p) => Some(p),
                None => return err(crate::error::AppError::invalid_arg("已取消导出")),
            }
        }
    };
    resp(state.export_note(&path, &fmt, out.as_deref(), rendered_html.as_deref()))
}

#[tauri::command]
pub fn io_export_vault(state: State<'_, AppState>, out_path: Option<String>) -> Value {
    let out: Option<PathBuf> = match out_path {
        Some(p) => Some(PathBuf::from(p)),
        None => match dialog_save_file("导出整库", "monote-vault.zip", &["zip"]) {
            Some(p) => Some(p),
            None => return err(crate::error::AppError::invalid_arg("已取消导出")),
        },
    };
    resp(state.export_vault(out.as_deref()))
}

// ===========================================================================
// 备份
// ===========================================================================

#[tauri::command]
pub fn backup_now(state: State<'_, AppState>) -> Value {
    resp(state.backup_now())
}

#[tauri::command]
pub fn backup_list(state: State<'_, AppState>) -> Value {
    resp(state.backup_list().map(|items| serde_json::json!({ "items": items })))
}

// ===========================================================================
// 同步
// ===========================================================================

#[tauri::command]
pub fn sync_config(state: State<'_, AppState>) -> Value {
    resp(Ok(state.sync_config()))
}

#[tauri::command]
pub fn sync_configure(
    state: State<'_, AppState>,
    url: String,
    username: String,
    password: Option<String>,
    auto_on_start: Option<bool>,
) -> Value {
    resp(state.sync_configure(&url, &username, password.as_deref(), auto_on_start.unwrap_or(false)))
}

#[tauri::command]
pub fn sync_run(state: State<'_, AppState>) -> Value {
    resp(state.sync_run())
}

// ===========================================================================
// 配置 / 通知 / 更新 / 快捷键 / 关于
// ===========================================================================

#[tauri::command]
pub fn app_get_config(state: State<'_, AppState>) -> Value {
    resp(Ok(state.get_config()))
}

#[tauri::command]
pub fn app_set_config(state: State<'_, AppState>, patch: Value) -> Value {
    resp(state.set_config(&patch))
}

#[tauri::command]
pub fn app_update_check(state: State<'_, AppState>, manual: Option<bool>) -> Value {
    resp(state.update_check(manual.unwrap_or(false)))
}

#[tauri::command]
pub fn notify_list(state: State<'_, AppState>, limit: Option<usize>) -> Value {
    resp(Ok(state.notify_list(limit)))
}

#[tauri::command]
pub fn notify_mark_read(state: State<'_, AppState>, ids: Option<Vec<u64>>) -> Value {
    let affected = state.notify_mark_read(ids);
    resp(Ok(serde_json::json!({ "affected": affected })))
}

#[tauri::command]
pub fn hotkey_set(
    state: State<'_, AppState>,
    command_id: String,
    combo: Option<String>,
    reset: Option<bool>,
) -> Value {
    resp(state.hotkey_set(&command_id, combo.as_deref().unwrap_or(""), reset.unwrap_or(false)))
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> Value {
    resp(Ok(state.app_info()))
}

#[tauri::command]
pub fn app_open_dialog(
    mode: String,
    title: Option<String>,
    filters: Option<Vec<String>>,
    default_name: Option<String>,
) -> Value {
    let title = title.unwrap_or_else(|| "选择文件".into());
    let path = match mode.as_str() {
        "folder" => dialog_pick_dir(&title),
        "save" => {
            let exts: Vec<&str> = filters.as_deref().unwrap_or(&[]).iter().map(|s| s.as_str()).collect();
            dialog_save_file(&title, default_name.as_deref().unwrap_or("export"), &exts)
        }
        _ => {
            let list = filters.unwrap_or_else(|| vec!["md".into(), "markdown".into(), "txt".into()]);
            let exts: Vec<&str> = list.iter().map(|s| s.as_str()).collect();
            dialog_pick_file(&title, &exts)
        }
    };
    resp(Ok(crate::types::DialogResult { path: path.map(|p| p.to_string_lossy().to_string()) }))
}

// ===========================================================================
// 原生对话框（rfd）
// ===========================================================================

pub(crate) fn dialog_pick_file(title: &str, exts: &[&str]) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().set_title(title);
    if !exts.is_empty() {
        dlg = dlg.add_filter("支持的文件", exts);
    }
    dlg.pick_file()
}

pub(crate) fn dialog_pick_dir(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

pub(crate) fn dialog_save_file(title: &str, default_name: &str, exts: &[&str]) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().set_title(title).set_file_name(default_name);
    if !exts.is_empty() {
        dlg = dlg.add_filter("支持的文件", exts);
    }
    dlg.save_file()
}
