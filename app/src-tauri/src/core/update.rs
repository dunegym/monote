//! M7 版本更新检查 UpdateChecker（对应设计文档 4.7、5.8、6.4、FR-605）。
//!
//! 约束：
//! - 默认每 7 天检查一次（手动触发不受节流限制）；
//! - 仅发送 `app/os/ver` 三个字段，不上传任何笔记内容（NFR-108）；
//! - 失败静默跳过；手动触发失败返回错误提示；
//! - v1 仅提示新版本，不自动下载（DD-6 同源范围外）。

use std::time::Duration;

use crate::error::{AppError, Result};
use crate::types::{UpdateConfig, UpdateInfo};

/// 当前版本（来自 Cargo.toml / tauri.conf.json）。
pub const CURRENT_VERSION: &str = "1.0.0";

/// 自动检查周期（FR-605：7 天）。
const CHECK_INTERVAL_DAYS: i64 = 7;
/// 网络超时（设计 6.4：10 s）。
const TIMEOUT_SECS: u64 = 10;

/// 平台标识：win-x64 / mac-universal / linux-x64。
pub fn os_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "win-x64"
    } else if cfg!(target_os = "macos") {
        "mac-universal"
    } else {
        "linux-x64"
    }
}

/// 是否到达自动检查周期（>7 天）。
///
/// `None`（从未检查）与时间戳损坏时返回 true（视为到期）。
pub fn should_check(last_check: Option<&str>, now: chrono::DateTime<chrono::Local>) -> bool {
    let last = match last_check {
        None => return true,
        Some(s) => match parse_iso_local(s) {
            Some(dt) => dt,
            None => return true,
        },
    };
    now.signed_duration_since(last) > chrono::Duration::days(CHECK_INTERVAL_DAYS)
}

/// 执行一次更新检查。`manual=true` 时不受 7 天节流限制且失败返回错误；
/// 自动检查失败时返回 `checked=false` 的静默结果。
///
/// 本函数**不写入任何本地文件**（是否持久化 `last_check` 由调用方决定）。
pub fn check_update(cfg: &UpdateConfig, manual: bool, now: chrono::DateTime<chrono::Local>) -> Result<UpdateInfo> {
    // 自动检查的两道闸门：设置关闭 / 7 天节流未到期。
    if !manual {
        if !cfg.auto_check {
            return Ok(silent_result(cfg.last_check.clone()));
        }
        if !should_check(cfg.last_check.as_deref(), now) {
            return Ok(silent_result(cfg.last_check.clone()));
        }
    }

    let manifest_url = cfg.manifest_url.trim();
    if manifest_url.is_empty() {
        return if manual {
            Err(AppError::update("未配置更新服务器地址"))
        } else {
            Ok(silent_result(cfg.last_check.clone()))
        };
    }

    match fetch_manifest(manifest_url) {
        Ok((latest, notes, url)) => {
            let has_update = is_newer(&latest, CURRENT_VERSION);
            Ok(UpdateInfo {
                current: CURRENT_VERSION.to_string(),
                latest: Some(latest),
                has_update,
                notes,
                url,
                checked_at: Some(now.to_rfc3339()),
                checked: true,
            })
        }
        Err(e) => {
            if manual {
                // FR-605：手动触发失败提示"检查失败，请检查网络后重试"。
                Err(AppError::update("检查失败，请检查网络后重试").with_detail(e.to_string()))
            } else {
                Ok(silent_result(cfg.last_check.clone()))
            }
        }
    }
}

/// 解析更新清单 JSON：`{latest, notes, url}`。
///
/// `latest` 必填且非空；`notes`/`url` 可选。格式错误返回 4004（本次检查忽略）。
pub fn parse_manifest(text: &str) -> Result<(String, Option<String>, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| AppError::update("更新清单不是合法 JSON").with_detail(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| AppError::update("更新清单格式错误（根节点应为 JSON 对象）"))?;
    let latest = obj
        .get("latest")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::update("更新清单缺少 latest 版本号"))?
        .to_string();
    let notes = obj.get("notes").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let url = obj.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    Ok((latest, notes, url))
}

// ===========================================================================
// 私有辅助
// ===========================================================================

/// 静默结果：未真正发起网络检查（节流/关闭/失败）。
fn silent_result(last_check: Option<String>) -> UpdateInfo {
    UpdateInfo {
        current: CURRENT_VERSION.to_string(),
        latest: None,
        has_update: false,
        notes: None,
        url: None,
        checked_at: last_check,
        checked: false,
    }
}

/// 发起清单请求：仅带 `app/os/ver` 三个查询参数（隐私边界，NFR-108）。
fn fetch_manifest(manifest_url: &str) -> Result<(String, Option<String>, Option<String>)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .user_agent(concat!("MoNote/", "1.0.0"))
        .build()
        .map_err(|e| AppError::network("初始化更新检查失败").with_detail(e.to_string()))?;
    let sep = if manifest_url.contains('?') { '&' } else { '?' };
    let url = format!(
        "{manifest_url}{sep}app=monote&os={}&ver={CURRENT_VERSION}",
        os_tag()
    );
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|e| AppError::network("更新检查请求失败").with_detail(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::update(format!("更新服务器返回 HTTP {}", status.as_u16())));
    }
    let text = resp
        .text()
        .map_err(|e| AppError::network("读取更新清单失败").with_detail(e.to_string()))?;
    parse_manifest(&text)
}

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
    None
}

/// 语义版本比较：`latest` 是否比 `current` 新（忽略 `v` 前缀与预发布/构建元数据）。
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()?
        .trim();
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::code;
    use chrono::TimeZone;

    fn fixed_now() -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).single().expect("valid local time")
    }

    #[test]
    fn should_check_threshold_is_7_days() {
        let now = fixed_now();
        assert!(should_check(None, now), "从未检查 → 应检查");
        let six = (now - chrono::Duration::days(6)).to_rfc3339();
        assert!(!should_check(Some(&six), now), "6 天前 → 未到期");
        let eight = (now - chrono::Duration::days(8)).to_rfc3339();
        assert!(should_check(Some(&eight), now), "8 天前 → 到期");
        let exactly = (now - chrono::Duration::days(7)).to_rfc3339();
        assert!(!should_check(Some(&exactly), now), "恰好 7 天不算超过");
        assert!(should_check(Some("broken-timestamp"), now), "损坏时间戳 → 视为到期");
        let future = (now + chrono::Duration::hours(2)).to_rfc3339();
        assert!(!should_check(Some(&future), now), "未来时间戳 → 不检查");
    }

    #[test]
    fn manifest_parsing_valid() {
        let (latest, notes, url) = parse_manifest(
            r#"{"latest":"1.2.0","notes":"修复若干问题","url":"https://update.monote.app/dl/1.2.0"}"#,
        )
        .unwrap();
        assert_eq!(latest, "1.2.0");
        assert_eq!(notes.as_deref(), Some("修复若干问题"));
        assert_eq!(url.as_deref(), Some("https://update.monote.app/dl/1.2.0"));
    }

    #[test]
    fn manifest_parsing_optional_fields_and_errors() {
        let (latest, notes, url) = parse_manifest(r#"{"latest":"1.0.1"}"#).unwrap();
        assert_eq!(latest, "1.0.1");
        assert!(notes.is_none());
        assert!(url.is_none());

        // 缺少 latest / latest 非字符串 / 非对象 / 非 JSON 均为 4004
        for bad in [
            r#"{"notes":"x"}"#,
            r#"{"latest":123}"#,
            r#"{"latest":""}"#,
            r#"[1,2,3]"#,
            "not json at all",
        ] {
            let err = parse_manifest(bad).unwrap_err();
            assert_eq!(err.code, code::UPDATE_UNAVAILABLE, "bad={bad}");
        }
    }

    #[test]
    fn semantic_version_comparison() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.2.0", "1.0.0"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(is_newer("v1.1.0", "1.0.0"));
        assert!(is_newer("1.0.0-beta", "0.9.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        assert!(!is_newer("1.0.0-beta", "1.0.0"), "同版本预发布不算更新");
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn throttled_auto_check_returns_silent_without_network() {
        let now = fixed_now();
        let cfg = UpdateConfig {
            auto_check: true,
            last_check: Some((now - chrono::Duration::hours(1)).to_rfc3339()),
            // 不可达地址：若真的发起网络请求，自动检查也会静默；用耗时进一步确认未发请求
            manifest_url: "http://127.0.0.1:1/manifest.json".into(),
        };
        let started = std::time::Instant::now();
        let info = check_update(&cfg, false, now).unwrap();
        assert!(!info.checked, "节流期内不发起检查");
        assert!(info.latest.is_none());
        assert!(!info.has_update);
        assert_eq!(info.checked_at, cfg.last_check, "checkedAt 保留上次检查时间");
        assert_eq!(info.current, CURRENT_VERSION);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "节流路径不应访问网络");
    }

    #[test]
    fn auto_check_disabled_returns_silent() {
        let now = fixed_now();
        let cfg = UpdateConfig {
            auto_check: false,
            last_check: None,
            manifest_url: "http://127.0.0.1:1/manifest.json".into(),
        };
        let info = check_update(&cfg, false, now).unwrap();
        assert!(!info.checked, "关闭自动检查后不发请求");
        // 手动触发不受 auto_check 关闭影响（仍会尝试网络并失败）
        let err = check_update(&cfg, true, now).unwrap_err();
        assert_eq!(err.code, code::UPDATE_UNAVAILABLE);
    }

    #[test]
    fn auto_check_network_failure_is_silent() {
        let now = fixed_now();
        let cfg = UpdateConfig {
            auto_check: true,
            last_check: None,
            manifest_url: "http://127.0.0.1:1/manifest.json".into(),
        };
        let info = check_update(&cfg, false, now).unwrap();
        assert!(!info.checked, "自动检查失败静默跳过");
        assert!(!info.has_update);
    }

    #[test]
    fn manual_check_network_failure_returns_update_error() {
        let now = fixed_now();
        let cfg = UpdateConfig {
            auto_check: false,
            last_check: None,
            manifest_url: "http://127.0.0.1:1/manifest.json".into(),
        };
        let err = check_update(&cfg, true, now).unwrap_err();
        assert_eq!(err.code, code::UPDATE_UNAVAILABLE);
        assert!(err.msg.contains("检查失败"), "msg={}", err.msg);
    }

    #[test]
    fn manual_check_empty_manifest_url_is_error() {
        let now = fixed_now();
        let cfg = UpdateConfig {
            auto_check: true,
            last_check: None,
            manifest_url: String::new(),
        };
        assert_eq!(check_update(&cfg, true, now).unwrap_err().code, code::UPDATE_UNAVAILABLE);
    }

    #[test]
    fn os_tag_is_known() {
        assert!(matches!(os_tag(), "win-x64" | "mac-universal" | "linux-x64"));
        assert_eq!(os_tag(), "win-x64", "当前测试环境为 Windows");
    }
}
