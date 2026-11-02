//! MoNote（墨记）—— 本地优先的个人 Markdown 笔记与知识管理工具。
//!
//! 架构（对应《01-软件设计文档》第 3 章）：
//! - L1/L2 前端（`frontend/`，WebView 内运行）
//! - L3 服务层：本 crate 的 `core` 模块（M2 Vault / M3 Link / M4 Search / M5 IO /
//!   M6 Backup & Sync / M7 AppCore）
//! - L4 数据层：库内 `.md` 权威副本 + `.monote/` 派生索引

pub mod core;
pub mod error;
pub mod ipc;
pub mod state;
pub mod types;
pub mod watcher;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let mut state = state::AppState::new();
            state.set_app(app.handle().clone());
            // 启动流程：恢复上次库（后台线程）+ 后台备份/更新检查
            state.start();
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 库与笔记
            ipc::vault_open,
            ipc::vault_pick,
            ipc::vault_info,
            ipc::vault_list,
            ipc::vault_read,
            ipc::vault_write,
            ipc::vault_rename,
            ipc::vault_trash,
            ipc::vault_trash_list,
            ipc::vault_trash_restore,
            ipc::vault_empty_trash,
            ipc::vault_create_note,
            ipc::vault_create_folder,
            ipc::vault_create_empty,
            ipc::vault_save_attachment,
            ipc::vault_image_data,
            // 链接 / 标签
            ipc::link_suggest,
            ipc::link_backlinks,
            ipc::tag_list,
            // 检索
            ipc::search_query,
            ipc::index_rebuild,
            ipc::index_status,
            // 导入导出
            ipc::io_import,
            ipc::io_export_note,
            ipc::io_export_vault,
            // 备份
            ipc::backup_now,
            ipc::backup_list,
            // 同步
            ipc::sync_config,
            ipc::sync_configure,
            ipc::sync_run,
            // 应用基础
            ipc::app_get_config,
            ipc::app_set_config,
            ipc::app_update_check,
            ipc::notify_list,
            ipc::notify_mark_read,
            ipc::hotkey_set,
            ipc::app_info,
            ipc::app_open_dialog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running MoNote (tauri application)");
}
