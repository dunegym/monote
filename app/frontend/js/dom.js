/**
 * 轻量 DOM 工具：元素创建、焦点陷阱、事件委托、可见性判断。
 * 不依赖任何框架。
 */

/** 查询单个元素。 */
export function qs(selector, root = document) {
  return root.querySelector(selector);
}

/** 查询多个元素（返回数组）。 */
export function qsa(selector, root = document) {
  return [...root.querySelectorAll(selector)];
}

/**
 * 创建元素。
 * @param {string} tag
 * @param {Record<string, any>} [attrs]
 * @param {Array<Node|string|null|undefined>} [children]
 * @returns {HTMLElement}
 */
export function el(tag, attrs = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value == null || value === false) continue;
    if (key === 'className' || key === 'class') node.className = String(value);
    else if (key === 'text') node.textContent = String(value);
    else if (key === 'html') node.innerHTML = String(value);
    else if (key === 'dataset') Object.assign(node.dataset, value);
    else if (key === 'style' && typeof value === 'object') Object.assign(node.style, value);
    else if (key === 'on' && typeof value === 'object') {
      for (const [evt, handler] of Object.entries(value)) node.addEventListener(evt, handler);
    } else if (key.startsWith('aria-') || key === 'role' || key === 'tabindex' || key === 'title') {
      node.setAttribute(key, String(value));
    } else if (key in node) {
      try {
        node[key] = value;
      } catch {
        node.setAttribute(key, String(value));
      }
    } else {
      node.setAttribute(key, value === true ? '' : String(value));
    }
  }
  for (const child of children) {
    if (child == null) continue;
    node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
  }
  return node;
}

/** 清空元素。 */
export function clear(node) {
  if (!node) return;
  while (node.firstChild) node.removeChild(node.firstChild);
}

/** 判断元素是否可见（供焦点陷阱过滤隐藏控件）。 */
export function isVisible(node) {
  if (!node || node.disabled) return false;
  if (node.hidden) return false;
  const style = window.getComputedStyle(node);
  if (style.display === 'none' || style.visibility === 'hidden') return false;
  const rect = node.getBoundingClientRect();
  return rect.width > 0 || rect.height > 0 || node === document.activeElement;
}

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'area[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  'iframe',
  '[tabindex]:not([tabindex="-1"])',
  '[contenteditable="true"]',
].join(',');

/** 返回容器内可聚焦元素（按 DOM 顺序）。 */
export function getFocusable(container) {
  return qsa(FOCUSABLE_SELECTOR, container).filter(isVisible);
}

/**
 * 焦点陷阱：把 Tab 焦点限制在容器内，关闭时释放并还原焦点。
 * @param {HTMLElement} container 需有 tabindex="-1" 作为兜底焦点
 * @param {{initialFocus?: HTMLElement|null}} [opts]
 */
export function focusTrap(container, opts = {}) {
  const previouslyFocused = document.activeElement;
  const onKeydown = (event) => {
    if (event.key !== 'Tab') return;
    const list = getFocusable(container);
    if (!list.length) {
      event.preventDefault();
      container.focus();
      return;
    }
    const first = list[0];
    const last = list[list.length - 1];
    const active = document.activeElement;
    const inside = container.contains(active);
    if (event.shiftKey) {
      if (!inside || active === first) {
        event.preventDefault();
        last.focus();
      }
    } else if (!inside || active === last) {
      event.preventDefault();
      first.focus();
    }
  };
  container.addEventListener('keydown', onKeydown);
  const target = opts.initialFocus && isVisible(opts.initialFocus) ? opts.initialFocus : getFocusable(container)[0] || container;
  requestAnimationFrame(() => {
    try {
      target.focus();
    } catch {
      /* ignore */
    }
  });
  return {
    release() {
      container.removeEventListener('keydown', onKeydown);
      if (previouslyFocused && typeof previouslyFocused.focus === 'function') {
        try {
          previouslyFocused.focus({ preventScroll: true });
        } catch {
          /* ignore */
        }
      }
    },
  };
}

/** 事件委托。返回取消函数。 */
export function delegate(root, eventName, selector, handler) {
  const listener = (event) => {
    const target = event.target instanceof Element ? event.target.closest(selector) : null;
    if (target && root.contains(target)) handler(event, target);
  };
  root.addEventListener(eventName, listener);
  return () => root.removeEventListener(eventName, listener);
}

/** 下一帧执行。 */
export function nextFrame(fn) {
  requestAnimationFrame(() => requestAnimationFrame(fn));
}

/** 滚动到元素（平滑）。 */
export function scrollIntoViewSafe(node, options = { behavior: 'smooth', block: 'center' }) {
  if (!node || typeof node.scrollIntoView !== 'function') return;
  try {
    node.scrollIntoView(options);
  } catch {
    node.scrollIntoView();
  }
}

/** 焦点可见的简单判断（用于调试）。 */
export function activeElementDeep() {
  let active = document.activeElement;
  while (active && active.shadowRoot && active.shadowRoot.activeElement) {
    active = active.shadowRoot.activeElement;
  }
  return active;
}
