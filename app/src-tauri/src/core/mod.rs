//! 核心服务层（L3）模块树。
//!
//! 模块与设计文档的对应关系：
//! - [`vault`]   M2 库管理（唯一文件写入口、回收站、外部改动检测）
//! - [`links`]   M3 链接解析（双链/反链/标签）
//! - [`index`]   M4 检索（SQLite FTS5 索引）
//! - [`io`]      M5 导入导出
//! - [`backup`]  M6 备份子服务
//! - [`sync`]    M6 同步子服务（WebDAV）
//! - [`config`]  M7 配置
//! - [`notify`]  M7 通知中心
//! - [`update`]  M7 版本更新检查

pub mod backup;
pub mod config;
pub mod index;
pub mod io;
pub mod links;
pub mod notify;
pub mod sync;
pub mod update;
pub mod vault;
