//! M7 通知中心 NotificationCenter（对应设计文档 4.7、FR-504）。
//!
//! 分级 info/warn/error，入列历史，最多保留 100 条；UI 常驻入口 + live-region 语义。
//! 内存态：进程重启后清空，符合"通知历史"轻量定位（AC-038 只要求可在常驻入口回看历史）。

use crate::types::{NotificationItem, NotificationList};

/// 历史保留上限（FR-504/AC-038）。
const MAX_HISTORY: usize = 100;

/// 通知中心（内存态）。
#[derive(Debug, Default, Clone)]
pub struct NotificationCenter {
    pub(crate) items: std::collections::VecDeque<NotificationItem>,
    pub(crate) next_id: u64,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 发布一条通知，返回条目（IPC 层负责同时向前端推送 `notify:new`）。
    ///
    /// - `level` 仅接受 info/warn/error，其他值归一为 info；
    /// - `ts` 为 unix 毫秒；
    /// - 超过 100 条时丢弃最旧条目。
    pub fn publish(&mut self, level: &str, title: impl Into<String>, detail: Option<String>) -> NotificationItem {
        self.next_id = self.next_id.saturating_add(1);
        let level = match level.trim().to_ascii_lowercase().as_str() {
            "warn" | "warning" => "warn",
            "error" => "error",
            _ => "info",
        }
        .to_string();
        let item = NotificationItem {
            id: self.next_id,
            level,
            title: title.into(),
            detail,
            ts: chrono::Local::now().timestamp_millis(),
            read: false,
        };
        self.items.push_back(item.clone());
        while self.items.len() > MAX_HISTORY {
            self.items.pop_front();
        }
        item
    }

    /// 列表（最新在前；`limit=None` 取全部历史）。
    pub fn list(&self, limit: Option<usize>) -> NotificationList {
        let take = limit.unwrap_or(self.items.len()).min(self.items.len());
        let items: Vec<NotificationItem> = self.items.iter().rev().take(take).cloned().collect();
        NotificationList {
            items,
            unread: self.unread(),
        }
    }

    /// 标记已读（`ids=None` 表示全部已读），返回受影响条数。
    pub fn mark_read(&mut self, ids: Option<&[u64]>) -> usize {
        let mut affected = 0usize;
        for item in self.items.iter_mut() {
            if item.read {
                continue;
            }
            let hit = match ids {
                None => true,
                Some(list) => list.contains(&item.id),
            };
            if hit {
                item.read = true;
                affected += 1;
            }
        }
        affected
    }

    /// 未读数。
    pub fn unread(&self) -> usize {
        self.items.iter().filter(|i| !i.read).count()
    }
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_list_and_unread() {
        let mut n = NotificationCenter::new();
        let a = n.publish("info", "备份完成", None);
        let b = n.publish("warn", "同步冲突", Some("2 个文件".into()));
        let c = n.publish("error", "备份失败", Some("磁盘空间不足".into()));
        assert_eq!((a.id, b.id, c.id), (1, 2, 3));
        assert_eq!(b.level, "warn");
        assert_eq!(c.detail.as_deref(), Some("磁盘空间不足"));
        assert!(a.ts > 0);
        assert!(!a.read);

        // 最新在前
        let list = n.list(None);
        assert_eq!(list.items.len(), 3);
        assert_eq!(list.items[0].id, 3);
        assert_eq!(list.items[2].id, 1);
        assert_eq!(list.unread, 3);

        // limit 截断（最新在前）
        let top2 = n.list(Some(2));
        assert_eq!(top2.items.len(), 2);
        assert_eq!(top2.items[0].id, 3);
        assert_eq!(top2.items[1].id, 2);
        assert_eq!(top2.unread, 3, "unread 为全量未读数");
        assert_eq!(n.list(Some(0)).items.len(), 0);
    }

    #[test]
    fn unknown_level_normalized_to_info() {
        let mut n = NotificationCenter::new();
        assert_eq!(n.publish("WARNING", "x", None).level, "warn");
        assert_eq!(n.publish("fatal", "x", None).level, "info");
        assert_eq!(n.publish("", "x", None).level, "info");
    }

    #[test]
    fn mark_read_selected_and_all() {
        let mut n = NotificationCenter::new();
        for i in 0..5 {
            n.publish("info", format!("n{i}"), None);
        }
        assert_eq!(n.unread(), 5);
        // 标记 2 条（含已读的幂等场景）
        assert_eq!(n.mark_read(Some(&[2, 4])), 2);
        assert_eq!(n.unread(), 3);
        assert_eq!(n.mark_read(Some(&[2, 4])), 0, "已读再标记不计入 affected");
        assert_eq!(n.mark_read(Some(&[99])), 0, "不存在的 id 不影响");
        assert_eq!(n.mark_read(None), 3, "None = 全部已读");
        assert_eq!(n.unread(), 0);
        assert!(n.list(None).items.iter().all(|i| i.read));
    }

    #[test]
    fn history_capped_at_100_keeping_newest() {
        let mut n = NotificationCenter::new();
        for i in 0..105 {
            n.publish("info", format!("n{i}"), None);
        }
        let list = n.list(None);
        assert_eq!(list.items.len(), 100, "最多保留 100 条");
        assert_eq!(list.items[0].id, 105, "保留最新一条");
        assert_eq!(list.items[99].id, 6, "最旧的 5 条被丢弃（id 1..5）");
        // 未读数只统计保留的历史
        assert_eq!(n.unread(), 100);
    }

    #[test]
    fn empty_center() {
        let n = NotificationCenter::new();
        let list = n.list(None);
        assert!(list.items.is_empty());
        assert_eq!(list.unread, 0);
        assert_eq!(n.unread(), 0);
    }
}
