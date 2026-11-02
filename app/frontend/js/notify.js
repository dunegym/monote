/**
 * 通知中心（FR-504）：
 * - 右上角铃铛 + 未读角标；面板显示历史（最多 100 条，notify_list）；
 * - 监听 notify:new 实时插入；info/warn/error 样式区分；
 * - 后台任务错误由前端 pushLocal 写入同一面板；
 * - 支持全部已读、单条已读、Esc/点击外部关闭。
 */
import { ctx } from './ctx.js';
import { state } from './state.js';
import { qs, el, clear } from './dom.js';
import { toast } from './toast.js';
import { formatRelative } from './util.js';

let bell = null;
let badge = null;
let panel = null;
let listEl = null;
let markAllBtn = null;
let initialized = false;
let localItems = [];
let nextLocalId = -1;
let open = false;
let loading = false;

const LEVEL_LABELS = { info: '提示', warn: '警告', error: '错误' };

export function init() {
  bell = qs('#btn-notify');
  badge = qs('#notify-badge');
  panel = qs('#notify-panel');
  if (initialized || !bell || !panel) return getApi();
  initialized = true;
  bell.addEventListener('click', (event) => {
    event.stopPropagation();
    if (open) closePanel();
    else openPanel();
  });
  document.addEventListener('mousedown', (event) => {
    if (open && panel && !panel.contains(event.target) && !bell.contains(event.target)) closePanel();
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && open) {
      event.preventDefault();
      closePanel();
    }
  });
  registerEventForwarding();
  refresh();
  return getApi();
}

function getApi() {
  return {
    refresh,
    openPanel,
    closePanel,
    togglePanel: () => (open ? closePanel() : openPanel()),
    pushLocal,
    getUnread: () => state.notify.unread,
  };
}

function registerEventForwarding() {
  // 由 app.js 统一监听 notify:new 后调用 handleIncoming；这里保留占位。
}

/** 处理来自后端的 notify:new 事件。 */
export function handleIncoming(item) {
  if (!item || item.id == null) return;
  const exists = state.notify.items.some((n) => n.id === item.id);
  if (exists) return;
  const list = [item, ...state.notify.items].slice(0, 100);
  state.notify = { items: list, unread: list.filter((n) => !n.read).length };
  updateBadge();
  if (open) renderList();
  if (item.level === 'warn' || item.level === 'error') {
    toast[item.level === 'error' ? 'error' : 'warn'](item.title || '收到新通知', {
      title: LEVEL_LABELS[item.level] || '通知',
    });
  }
}

/** 前端本地通知（后台任务错误等），不持久化。 */
export function pushLocal({ level = 'info', title, detail }) {
  const item = {
    id: nextLocalId--,
    level,
    title: title || '通知',
    detail: detail || '',
    ts: Date.now(),
    read: false,
    local: true,
  };
  localItems = [item, ...localItems].slice(0, 100);
  state.notify = {
    items: mergeItems(),
    unread: mergeItems().filter((n) => !n.read).length,
  };
  updateBadge();
  if (open) renderList();
  if (level === 'error') {
    toast.error(title || '后台任务失败', { title: '通知' });
  }
  return item;
}

function mergeItems() {
  return [...localItems, ...state.notify.items].slice(0, 100);
}

/** 重新读取通知历史。 */
export async function refresh() {
  loading = true;
  renderList();
  try {
    const res = await ctx.api.call('notify_list', { limit: 100 });
    const items = res && Array.isArray(res.items) ? res.items : [];
    const unread = res && typeof res.unread === 'number' ? res.unread : items.filter((n) => !n.read).length;
    state.notify = { items, unread };
    loading = false;
    updateBadge();
    if (open) renderList();
  } catch (err) {
    loading = false;
    if (open) renderList(err);
    console.warn('[MoNote] 通知列表加载失败', err);
  }
}

function updateBadge() {
  if (!badge) return;
  const unread = state.notify.unread;
  if (unread > 0) {
    badge.hidden = false;
    badge.textContent = unread > 99 ? '99+' : String(unread);
    badge.setAttribute('aria-label', `${unread} 条未读通知`);
  } else {
    badge.hidden = true;
    badge.textContent = '';
    badge.removeAttribute('aria-label');
  }
}

function openPanel() {
  if (!panel || !bell) return;
  open = true;
  panel.hidden = false;
  bell.setAttribute('aria-expanded', 'true');
  renderList();
  const firstFocus = panel.querySelector('.notify-close, .notify-mark-all, .notify-item');
  if (firstFocus) firstFocus.focus();
}

function closePanel() {
  if (!panel) return;
  open = false;
  panel.hidden = true;
  if (bell) {
    bell.setAttribute('aria-expanded', 'false');
    bell.focus();
  }
}

function renderList(error) {
  if (!panel) return;
  clear(panel);
  const head = el('div', { className: 'notify-head' }, [
    el('h3', { text: '通知中心' }),
    el('span', { className: 'muted', text: state.notify.unread ? `${state.notify.unread} 条未读` : '全部已读' }),
    el('button', {
      type: 'button',
      className: 'btn btn-sm notify-mark-all',
      text: '全部已读',
      'aria-label': '标记全部通知为已读',
      hidden: state.notify.unread === 0,
    }),
    el('button', {
      type: 'button',
      className: 'icon-btn notify-close',
      text: '×',
      'aria-label': '关闭通知中心',
    }),
  ]);
  panel.appendChild(head);
  head.querySelector('.notify-close').addEventListener('click', closePanel);
  const markAll = head.querySelector('.notify-mark-all');
  if (markAll) markAll.addEventListener('click', markAllRead);

  const list = el('div', {
    className: 'notify-list',
    role: 'log',
    'aria-live': 'polite',
    'aria-relevant': 'additions',
    'aria-label': '通知历史',
  });
  panel.appendChild(list);
  listEl = list;

  if (loading && !state.notify.items.length) {
    list.appendChild(el('p', { className: 'notify-empty', text: '正在加载通知…' }));
    return;
  }
  if (error) {
    list.appendChild(el('p', { className: 'notify-empty', text: `通知加载失败：${ctx.api.friendlyError(error)}` }));
    return;
  }
  const items = mergeItems();
  if (!items.length) {
    list.appendChild(el('p', { className: 'notify-empty', text: '暂无通知。备份、同步、索引等后台任务的结果会出现在这里。' }));
    return;
  }
  for (const item of items) {
    const level = ['info', 'warn', 'error'].includes(item.level) ? item.level : 'info';
    const node = el('div', {
      className: `notify-item level-${level} ${item.read ? 'read' : 'unread'}`,
      role: 'button',
      tabindex: '0',
      'aria-label': `${LEVEL_LABELS[level]}：${item.title}${item.read ? '' : '（未读）'}`,
      title: item.read ? '' : '点击标记为已读',
      dataset: { notifyId: String(item.id) },
    }, [
      el('span', { className: 'notify-dot', 'aria-hidden': 'true' }),
      el('div', { className: 'notify-main' }, [
        el('div', { className: 'notify-title', text: item.title }),
        item.detail ? el('div', { className: 'notify-detail', text: item.detail }) : null,
        el('div', { className: 'notify-time', text: `${LEVEL_LABELS[level]} · ${formatRelative(item.ts)}` }),
      ]),
    ]);
    node.addEventListener('click', () => markRead(item));
    node.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        markRead(item);
      }
    });
    list.appendChild(node);
  }
}

async function markRead(item) {
  if (item.read) return;
  if (item.local) {
    item.read = true;
    localItems = localItems.map((n) => (n.id === item.id ? { ...n, read: true } : n));
  } else {
    try {
      await ctx.api.call('notify_mark_read', { ids: [item.id] });
    } catch (err) {
      toast.error(`标记已读失败：${ctx.api.friendlyError(err)}`);
      return;
    }
    state.notify.items = state.notify.items.map((n) => (n.id === item.id ? { ...n, read: true } : n));
  }
  const items = mergeItems();
  state.notify = { items, unread: items.filter((n) => !n.read).length };
  updateBadge();
  renderList();
}

async function markAllRead() {
  const unreadBackend = state.notify.items.filter((n) => !n.read);
  localItems = localItems.map((n) => ({ ...n, read: true }));
  if (unreadBackend.length) {
    try {
      await ctx.api.call('notify_mark_read', {});
    } catch (err) {
      toast.error(`标记已读失败：${ctx.api.friendlyError(err)}`);
    }
  }
  state.notify.items = state.notify.items.map((n) => ({ ...n, read: true }));
  const items = mergeItems();
  state.notify = { items, unread: items.filter((n) => !n.read).length };
  updateBadge();
  renderList();
}

export default { init, refresh, pushLocal, handleIncoming, openPanel, closePanel };
