//! 统一错误约定（对应设计文档 6.1：错误码分段 + envelope 返回）。
//!
//! 所有 IPC 命令都返回 envelope：
//! - 成功：`{ "ok": true, "data": ... }`
//! - 失败：`{ "ok": false, "error": { "code": 2002, "msg": "...", "detail": "..." } }`

use serde::Serialize;

/// 错误码段（设计文档 6.1）。
pub mod code {
    // 1000-1999 参数/状态错误（调用方可修正）
    pub const PATH_ESCAPE: u32 = 1001;
    pub const NOTE_NOT_FOUND: u32 = 1002;
    pub const NAME_CONFLICT: u32 = 1003;
    pub const HOTKEY_CONFLICT: u32 = 1004;
    pub const INVALID_ARG: u32 = 1005;
    // 2000-2999 本地资源错误
    pub const VAULT_NOT_OPEN: u32 = 2001;
    pub const IO_ERROR: u32 = 2002;
    pub const DISK_FULL: u32 = 2003;
    pub const REV_CONFLICT: u32 = 2004;
    // 3000-3999 索引/解析错误
    pub const INDEX_DEGRADED: u32 = 3001;
    pub const QUERY_SYNTAX: u32 = 3002;
    // 4000-4999 网络错误
    pub const WEBDAV_AUTH: u32 = 4001;
    pub const SYNC_CONFLICT: u32 = 4002;
    pub const NETWORK_UNREACHABLE: u32 = 4003;
    pub const UPDATE_UNAVAILABLE: u32 = 4004;
}

/// 错误体：序列化进 envelope 的 `error` 字段。
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: u32,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 内部错误类型：所有核心模块统一返回它，由 IPC 层转换成 envelope。
#[derive(Debug, Clone)]
pub struct AppError {
    pub code: u32,
    pub msg: String,
    pub detail: Option<String>,
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn new(code: u32, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    // ---- 常用构造器 -----------------------------------------------------

    pub fn path_escape() -> Self {
        Self::new(code::PATH_ESCAPE, "路径越界：只能访问库内文件")
    }

    pub fn not_found(path: &str) -> Self {
        Self::new(code::NOTE_NOT_FOUND, "笔记不存在").with_detail(path)
    }

    pub fn name_conflict(name: &str) -> Self {
        Self::new(code::NAME_CONFLICT, "重名冲突").with_detail(name)
    }

    pub fn invalid_arg(msg: impl Into<String>) -> Self {
        Self::new(code::INVALID_ARG, msg)
    }

    pub fn vault_not_open() -> Self {
        Self::new(code::VAULT_NOT_OPEN, "尚未打开笔记库")
    }

    pub fn rev_conflict(path: &str, expect: u64, actual: u64) -> Self {
        Self::new(code::REV_CONFLICT, "修订号冲突：文件已被其他操作修改")
            .with_detail(format!("{path}: expect={expect}, actual={actual}"))
    }

    pub fn index_degraded(msg: impl Into<String>) -> Self {
        Self::new(code::INDEX_DEGRADED, msg)
    }

    pub fn query_syntax(msg: impl Into<String>) -> Self {
        Self::new(code::QUERY_SYNTAX, msg)
    }

    pub fn network(msg: impl Into<String>) -> Self {
        Self::new(code::NETWORK_UNREACHABLE, msg)
    }

    pub fn update(msg: impl Into<String>) -> Self {
        Self::new(code::UPDATE_UNAVAILABLE, msg)
    }

    /// 将标准 IO 错误映射为 2002/2003 段错误码。
    pub fn io(err: &std::io::Error) -> Self {
        // Windows: ERROR_DISK_FULL = 112；其它平台 ENOSPC = 28
        let msg = err.to_string();
        let disk_full = msg.contains("os error 112")
            || msg.contains("os error 28")
            || msg.contains("No space left on device")
            || msg.contains("磁盘空间不足");
        if disk_full {
            Self::new(code::DISK_FULL, "磁盘空间不足")
        } else {
            Self::new(code::IO_ERROR, "本地读写失败").with_detail(msg)
        }
    }

    pub fn io_ctx(ctx: impl Into<String>, err: &std::io::Error) -> Self {
        let base = Self::io(err);
        let detail = format!("{}（{}）", ctx.into(), base.detail.unwrap_or_default());
        Self { detail: Some(detail), ..base }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.msg)?;
        if let Some(d) = &self.detail {
            write!(f, " ({d})")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::io(&e)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::new(code::INDEX_DEGRADED, "索引数据库错误").with_detail(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::new(code::INVALID_ARG, "JSON 解析失败").with_detail(e.to_string())
    }
}

/// 成功 envelope 构造器。
pub fn ok<T: Serialize>(data: T) -> serde_json::Value {
    serde_json::json!({ "ok": true, "data": data })
}

/// 失败 envelope 构造器。
pub fn err(e: AppError) -> serde_json::Value {
    let body = ErrorBody { code: e.code, msg: e.msg, detail: e.detail };
    serde_json::json!({ "ok": false, "error": body })
}

/// 辅助宏：命令实现体写 `ret!(expr)` 自动转 envelope；错误自动映射。
#[macro_export]
macro_rules! ret {
    ($expr:expr) => {
        match $expr {
            Ok(v) => $crate::error::ok(v),
            Err(e) => $crate::error::err(e),
        }
    };
}
