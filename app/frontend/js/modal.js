/**
 * 模态框管理：通用对话框、确认框、输入框。
 * 职责：焦点陷阱、Esc 关闭、焦点还原、层叠管理、body 滚动锁定。
 */
import { el, qs, focusTrap, clear } from './dom.js';

let overlayRoot = null;
/** @type {Array<{overlay:HTMLElement, close:(result?:any)=>void}>} */
const stack = [];

function ensureRoot() {
  if (overlayRoot && document.body.contains(overlayRoot)) return overlayRoot;
  overlayRoot = document.getElementById('overlay-root');
  if (!overlayRoot) {
    overlayRoot = el('div', { id: 'overlay-root', className: 'overlay-root' });
    document.body.appendChild(overlayRoot);
  }
  return overlayRoot;
}

function syncBodyState() {
  document.body.classList.toggle('modal-open', stack.length > 0);
}

function onDocumentKeydown(event) {
  if (event.key !== 'Escape' || !stack.length) return;
  // 命令面板 / 快速打开等轻量浮层位于模态框之上，由它们自己处理 Esc
  if (document.querySelector('.palette-overlay')) return;
  const top = stack[stack.length - 1];
  event.preventDefault();
  event.stopPropagation();
  top.close();
}

document.addEventListener('keydown', onDocumentKeydown, true);

/**
 * 打开模态框。
 * @param {{
 *   title?: string,
 *   subtitle?: string,
 *   content?: Node|string,
 *   footer?: Node|Array<Node>|null,
 *   size?: 'sm'|'md'|'lg'|'xl'|'full',
 *   className?: string,
 *   closable?: boolean,
 *   closeOnBackdrop?: boolean,
 *   initialFocus?: HTMLElement|null,
 *   onClose?: () => void,
 *   ariaLabel?: string,
 * }} options
 * @returns {{el:HTMLElement, body:HTMLElement, overlay:HTMLElement, close:()=>void, closed:boolean, setTitle:(t:string)=>void}}
 */
export function openModal(options = {}) {
  const root = ensureRoot();
  const size = options.size || 'md';
  const closable = options.closable !== false;
  const closeOnBackdrop = options.closeOnBackdrop !== false;
  const titleId = `modal-title-${Math.random().toString(36).slice(2, 9)}`;

  const titleNode = el('h2', {
    className: 'modal-title',
    id: titleId,
    text: options.title == null ? '' : String(options.title),
  });
  const subtitleNode = options.subtitle
    ? el('p', { className: 'modal-subtitle', text: String(options.subtitle) })
    : null;
  const closeBtn = el('button', {
    type: 'button',
    className: 'icon-btn modal-close',
    'aria-label': '关闭对话框',
    title: '关闭 (Esc)',
    text: '×',
  });
  const header = el('header', { className: 'modal-header' }, [
    el('div', { className: 'modal-header-text' }, [titleNode, subtitleNode]),
    closeBtn,
  ]);

  const body = el('div', { className: 'modal-body' });
  if (options.content != null) {
    if (typeof options.content === 'string') body.innerHTML = options.content;
    else body.appendChild(options.content);
  }

  let footerEl = null;
  if (options.footer) {
    footerEl = el('footer', { className: 'modal-footer' });
    const items = Array.isArray(options.footer) ? options.footer : [options.footer];
    for (const item of items) if (item) footerEl.appendChild(item);
  }

  const dialog = el('div', {
    className: `modal modal-${size}${options.className ? ` ${options.className}` : ''}`,
    role: 'dialog',
    'aria-modal': 'true',
    'aria-labelledby': options.title == null ? null : titleId,
    'aria-label': options.title == null ? options.ariaLabel || '对话框' : null,
    tabindex: '-1',
  }, [header, body, footerEl]);

  const overlay = el('div', { className: 'modal-overlay' }, [dialog]);
  root.appendChild(overlay);
  // 入场动画在下一帧加 class，避免闪烁
  requestAnimationFrame(() => overlay.classList.add('modal-overlay-visible'));

  const trap = focusTrap(dialog, { initialFocus: options.initialFocus || null });
  let closed = false;
  const entry = { overlay, close: (result) => close(result) };
  stack.push(entry);
  syncBodyState();

  function close() {
    if (closed) return;
    closed = true;
    trap.release();
    const idx = stack.indexOf(entry);
    if (idx >= 0) stack.splice(idx, 1);
    overlay.classList.remove('modal-overlay-visible');
    overlay.classList.add('modal-overlay-leaving');
    setTimeout(() => overlay.remove(), 140);
    syncBodyState();
    if (typeof options.onClose === 'function') {
      try {
        options.onClose();
      } catch (err) {
        console.error('[MoNote] modal onClose 异常', err);
      }
    }
  }

  closeBtn.addEventListener('click', () => close());
  if (closable) closeBtn.removeAttribute('hidden');
  else closeBtn.hidden = true;

  overlay.addEventListener('mousedown', (event) => {
    if (event.target === overlay && closeOnBackdrop && closable) close();
  });

  return {
    el: dialog,
    body,
    overlay,
    close,
    get closed() {
      return closed;
    },
    setTitle(text) {
      titleNode.textContent = String(text || '');
    },
    focus() {
      const target = qs('[data-autofocus]', dialog) || dialog;
      target.focus();
    },
  };
}

/** 关闭最上层模态框。 */
export function closeTopModal() {
  const top = stack[stack.length - 1];
  if (top) top.close();
}

/** 关闭全部模态框。 */
export function closeAllModals() {
  while (stack.length) stack[stack.length - 1].close();
}

/** 当前是否有模态框打开。 */
export function isModalOpen() {
  return stack.length > 0;
}

/** 按钮工厂（供对话框底部使用）。 */
export function button(label, opts = {}) {
  const btn = el('button', {
    type: opts.type || 'button',
    className: `btn ${opts.variant ? `btn-${opts.variant}` : ''}${opts.className ? ` ${opts.className}` : ''}`.trim(),
    text: label,
    'aria-label': opts.ariaLabel || null,
  });
  if (opts.onClick) btn.addEventListener('click', opts.onClick);
  if (opts.autofocus) btn.setAttribute('data-autofocus', '');
  return btn;
}

/**
 * 确认对话框。破坏性操作（danger=true）默认焦点在「取消」，避免误操作。
 * @param {{title?:string,message:string,detail?:string,confirmLabel?:string,cancelLabel?:string,danger?:boolean,extra?:Node}} opts
 * @returns {Promise<boolean>}
 */
export function confirmDialog(opts = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
      modal.close();
    };
    const cancelBtn = button(opts.cancelLabel || '取消', { onClick: () => finish(false) });
    const confirmBtn = button(opts.confirmLabel || '确定', {
      variant: opts.danger ? 'danger' : 'primary',
      onClick: () => finish(true),
    });
    const content = el('div', { className: 'confirm-content' }, [
      el('p', { className: 'confirm-message', text: opts.message || '' }),
      opts.detail ? el('pre', { className: 'confirm-detail', text: opts.detail }) : null,
      opts.extra || null,
    ]);
    const modal = openModal({
      title: opts.title || '请确认',
      content,
      footer: [cancelBtn, confirmBtn],
      size: 'sm',
      initialFocus: opts.danger ? cancelBtn : confirmBtn,
      onClose: () => {
        if (!settled) {
          settled = true;
          resolve(false);
        }
      },
    });
    // 让 Enter 不默认触发危险的确认：danger 时确认按钮不是默认焦点即可。
  });
}

/**
 * 输入对话框。
 * @param {{title?:string,label?:string,value?:string,placeholder?:string,help?:string,
 *          confirmLabel?:string,validate?:(v:string)=>(string|null),multiline?:boolean}} opts
 * @returns {Promise<string|null>}
 */
export function promptDialog(opts = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
      modal.close();
    };
    const inputId = `prompt-input-${Math.random().toString(36).slice(2, 8)}`;
    const label = el('label', { className: 'field-label', for: inputId, text: opts.label || '名称' });
    const input = opts.multiline
      ? el('textarea', { className: 'input prompt-input', id: inputId, rows: 4, placeholder: opts.placeholder || '' })
      : el('input', { className: 'input prompt-input', id: inputId, type: 'text', placeholder: opts.placeholder || '', value: opts.value || '' });
    input.value = opts.value || '';
    const error = el('p', { className: 'field-error', role: 'alert', hidden: true });
    const help = opts.help ? el('p', { className: 'field-help', text: opts.help }) : null;

    const submit = () => {
      const value = input.value.trim();
      const problem = opts.validate ? opts.validate(value) : value ? null : '内容不能为空';
      if (problem) {
        error.textContent = problem;
        error.hidden = false;
        input.focus();
        return;
      }
      finish(value);
    };
    const cancelBtn = button(opts.cancelLabel || '取消', { onClick: () => finish(null) });
    const okBtn = button(opts.confirmLabel || '确定', { variant: 'primary', onClick: submit });
    const form = el('form', {
      className: 'prompt-form',
      on: {
        submit: (event) => {
          event.preventDefault();
          submit();
        },
      },
    }, [label, input, help, error]);

    const modal = openModal({
      title: opts.title || '输入',
      content: form,
      footer: [cancelBtn, okBtn],
      size: 'sm',
      initialFocus: input,
      onClose: () => {
        if (!settled) {
          settled = true;
          resolve(null);
        }
      },
    });
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229) {
        event.preventDefault();
        submit();
      }
    });
    requestAnimationFrame(() => {
      input.focus();
      if (typeof input.select === 'function') input.select();
    });
    void modal;
  });
}

/**
 * 提示对话框（仅确定）。
 * @param {{title?:string,message:string,detail?:string}} opts
 * @returns {Promise<void>}
 */
export function alertDialog(opts = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      resolve();
      modal.close();
    };
    const okBtn = button(opts.okLabel || '知道了', { variant: 'primary', onClick: finish });
    const content = el('div', { className: 'confirm-content' }, [
      el('p', { className: 'confirm-message', text: opts.message || '' }),
      opts.detail ? el('pre', { className: 'confirm-detail', text: opts.detail }) : null,
    ]);
    const modal = openModal({
      title: opts.title || '提示',
      content,
      footer: [okBtn],
      size: 'sm',
      initialFocus: okBtn,
      onClose: () => {
        if (!settled) {
          settled = true;
          resolve();
        }
      },
    });
  });
}

/** 清空静态内容（模块热替换/测试场景使用）。 */
export function clearOverlay() {
  closeAllModals();
  if (overlayRoot) clear(overlayRoot);
}
