/**
 * 轻量内联提示（toast）：用于用户操作的就近反馈，不打断编辑。
 * 错误同时写入控制台；后台任务错误另进通知中心（见 notify.js）。
 */
import { el, clear } from './dom.js';

let root = null;
let counter = 0;
const MAX_VISIBLE = 4;

const ICONS = {
  info: 'ℹ',
  success: '✓',
  warn: '⚠',
  error: '✕',
};

function ensureRoot() {
  if (root && document.body.contains(root)) return root;
  root = document.getElementById('toast-root');
  if (!root) {
    root = el('div', { id: 'toast-root', className: 'toast-root', 'aria-live': 'polite' });
    document.body.appendChild(root);
  }
  return root;
}

/**
 * 显示一条 toast。
 * @param {string} message
 * @param {{type?:'info'|'success'|'warn'|'error', title?:string, timeout?:number, action?:{label:string, onClick:()=>void}}} [opts]
 * @returns {{close:()=>void, node:HTMLElement}}
 */
export function show(message, opts = {}) {
  const type = opts.type || 'info';
  const container = ensureRoot();
  const existing = [...container.querySelectorAll('.toast')];
  if (existing.length >= MAX_VISIBLE) existing[0].remove();

  const id = `toast-${(counter += 1)}`;
  const closeBtn = el('button', {
    className: 'toast-close icon-btn',
    type: 'button',
    'aria-label': '关闭提示',
    text: '×',
  });
  const actionBtn = opts.action
    ? el('button', {
        className: 'toast-action btn btn-sm',
        type: 'button',
        text: opts.action.label,
        on: { click: () => { opts.action.onClick(); close(); } },
      })
    : null;

  const node = el('div', {
    className: `toast toast-${type}`,
    id,
    role: type === 'error' ? 'alert' : 'status',
    'aria-live': type === 'error' ? 'assertive' : 'polite',
  }, [
    el('span', { className: 'toast-icon', 'aria-hidden': 'true', text: ICONS[type] || ICONS.info }),
    el('div', { className: 'toast-body' }, [
      opts.title ? el('div', { className: 'toast-title', text: opts.title }) : null,
      el('div', { className: 'toast-message', text: String(message || '') }),
      actionBtn,
    ]),
    closeBtn,
  ]);

  let timer = null;
  const close = () => {
    if (timer) clearTimeout(timer);
    timer = null;
    node.classList.add('toast-leaving');
    setTimeout(() => node.remove(), 160);
  };
  closeBtn.addEventListener('click', close);
  container.appendChild(node);
  const timeout = opts.timeout != null ? opts.timeout : type === 'error' ? 9000 : type === 'warn' ? 6500 : 4200;
  if (timeout > 0) timer = setTimeout(close, timeout);
  return { close, node };
}

/** 快捷方法。 */
export const toast = {
  show,
  info(message, opts = {}) { return show(message, { ...opts, type: 'info' }); },
  success(message, opts = {}) { return show(message, { ...opts, type: 'success' }); },
  warn(message, opts = {}) { return show(message, { ...opts, type: 'warn' }); },
  error(message, opts = {}) { return show(message, { ...opts, type: 'error', timeout: opts.timeout != null ? opts.timeout : 9000 }); },
  clearAll() {
    const container = ensureRoot();
    clear(container);
  },
};

/** 兼容默认导入。 */
export default toast;
