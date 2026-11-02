/**
 * 命令面板（Ctrl+Shift+P，FR-604）：
 * - 命令注册表由 app.js 通过 registerCommands() 注入；
 * - 模糊过滤、分组显示、方向键/回车执行、Esc 关闭；
 * - 每条命令可绑定可自定义快捷键（shortcutId）。
 */
import { ctx } from './ctx.js';
import { el, clear, focusTrap } from './dom.js';
import { fuzzyScore, escapeHtml } from './util.js';
import { toast } from './toast.js';

/** @type {Array<{id:string,title:string,group?:string,keywords?:string,shortcutId?:string,when?:()=>boolean,run:()=>any}>} */
let commands = [];
let overlay = null;
let trap = null;
let input = null;
let listEl = null;
let items = [];
let activeIndex = 0;
let returnFocus = null;

export function init() {
  return { open, close, registerCommands, getCommands: () => [...commands], run };
}

/** 注册/覆盖命令（按 id 去重，后注册覆盖先注册）。 */
export function registerCommands(list) {
  const byId = new Map(commands.map((c) => [c.id, c]));
  for (const cmd of list || []) {
    if (!cmd || !cmd.id || !cmd.title || typeof cmd.run !== 'function') continue;
    byId.set(cmd.id, cmd);
  }
  commands = [...byId.values()];
}

/** 执行命令（也可由其它模块调用，如引导、菜单）。 */
export async function run(id) {
  const cmd = commands.find((c) => c.id === id);
  if (!cmd) {
    toast.warn(`未找到命令：${id}`);
    return false;
  }
  if (cmd.when && !cmd.when()) {
    toast.info(`当前状态下不可用：${cmd.title}`);
    return false;
  }
  try {
    await cmd.run();
    return true;
  } catch (err) {
    console.error(`[MoNote] 命令执行失败：${id}`, err);
    toast.error(`执行「${cmd.title}」失败：${ctx.api ? ctx.api.friendlyError(err) : String(err)}`);
    return false;
  }
}

function getCommands() {
  return [...commands];
}

/** 打开命令面板。 */
export function open() {
  if (overlay) {
    if (input) input.focus();
    return;
  }
  returnFocus = document.activeElement;
  input = el('input', {
    type: 'text',
    className: 'palette-input',
    id: 'command-palette-input',
    role: 'combobox',
    'aria-label': '命令面板：输入命令名称',
    'aria-expanded': 'true',
    'aria-controls': 'command-palette-list',
    'aria-autocomplete': 'list',
    autocomplete: 'off',
    spellcheck: 'false',
    placeholder: '输入命令…（例如：新建、导出、主题、检索、设置）',
  });
  listEl = el('div', { className: 'palette-list', id: 'command-palette-list', role: 'listbox', 'aria-label': '可用命令' });
  const panel = el('div', { className: 'palette-panel', tabindex: '-1' }, [
    el('div', { className: 'palette-input-row' }, [
      el('span', { className: 'palette-icon', 'aria-hidden': 'true', text: '⌘' }),
      input,
    ]),
    listEl,
    el('div', { className: 'palette-footer' }, [
      el('span', { text: '↑↓ 选择' }),
      el('span', { text: '↵ 执行' }),
      el('span', { text: 'Esc 关闭' }),
      el('span', { text: '全部操作均可用键盘完成' }),
    ]),
  ]);
  overlay = el('div', { className: 'palette-overlay', role: 'presentation' }, [panel]);
  document.body.appendChild(overlay);
  requestAnimationFrame(() => overlay.classList.add('visible'));
  trap = focusTrap(panel, { initialFocus: input });
  input.addEventListener('input', () => {
    activeIndex = 0;
    render();
  });
  input.addEventListener('keydown', onKeydown);
  overlay.addEventListener('mousedown', (event) => {
    if (event.target === overlay) close();
  });
  activeIndex = 0;
  render();
  requestAnimationFrame(() => input.focus());
}

/** 关闭命令面板。 */
export function close() {
  if (!overlay) return;
  overlay.remove();
  overlay = null;
  if (trap) {
    trap.release();
    trap = null;
  }
  items = [];
  if (returnFocus && typeof returnFocus.focus === 'function') {
    try {
      returnFocus.focus();
    } catch {
      /* ignore */
    }
  }
  returnFocus = null;
}

function visibleCommands() {
  const query = input ? input.value.trim() : '';
  const enabled = commands.filter((cmd) => !cmd.when || cmd.when());
  if (!query) return enabled;
  const scored = enabled
    .map((cmd) => {
      const haystack = `${cmd.title} ${cmd.keywords || ''} ${cmd.group || ''} ${cmd.id}`;
      const score = fuzzyScore(query, haystack);
      return { cmd, score };
    })
    .filter((entry) => entry.score >= 0)
    .sort((a, b) => b.score - a.score);
  return scored.map((entry) => entry.cmd);
}

function render() {
  if (!listEl) return;
  clear(listEl);
  items = visibleCommands();
  if (!items.length) {
    listEl.appendChild(el('p', { className: 'palette-empty', text: '没有匹配的命令' }));
    if (input) input.removeAttribute('aria-activedescendant');
    return;
  }
  if (activeIndex >= items.length) activeIndex = items.length - 1;
  if (activeIndex < 0) activeIndex = 0;
  let lastGroup = null;
  items.forEach((cmd, index) => {
    const group = cmd.group || '其它';
    if (group !== lastGroup) {
      lastGroup = group;
      listEl.appendChild(el('div', { className: 'palette-group-title', text: group }));
    }
    const shortcut = cmd.shortcutId && ctx.hotkeys ? ctx.hotkeys.comboFor(cmd.shortcutId) : '';
    const node = el('div', {
      className: `palette-item${index === activeIndex ? ' active' : ''}`,
      role: 'option',
      id: `command-palette-option-${index}`,
      'aria-selected': index === activeIndex ? 'true' : 'false',
      dataset: { index: String(index) },
    }, [
      el('div', { className: 'palette-item-main' }, [
        el('div', { className: 'palette-item-title', html: highlightTitle(cmd.title, input.value.trim()) }),
        cmd.keywords ? el('div', { className: 'palette-item-sub', text: cmd.keywords }) : null,
      ]),
      shortcut ? el('kbd', { className: 'palette-kbd', text: shortcut }) : null,
    ]);
    node.addEventListener('mousedown', (event) => {
      event.preventDefault();
      choose(index);
    });
    node.addEventListener('mousemove', () => {
      if (activeIndex !== index) {
        activeIndex = index;
        updateActive();
      }
    });
    listEl.appendChild(node);
  });
  updateActive();
}

function highlightTitle(title, query) {
  if (!query) return escapeHtml(title);
  const lowerTitle = title.toLowerCase();
  const lowerQuery = query.toLowerCase();
  const idx = lowerTitle.indexOf(lowerQuery);
  if (idx < 0) return escapeHtml(title);
  return `${escapeHtml(title.slice(0, idx))}<mark>${escapeHtml(title.slice(idx, idx + query.length))}</mark>${escapeHtml(title.slice(idx + query.length))}`;
}

function updateActive() {
  if (!listEl) return;
  const nodes = [...listEl.querySelectorAll('.palette-item')];
  nodes.forEach((node, index) => {
    const active = index === activeIndex;
    node.classList.toggle('active', active);
    node.setAttribute('aria-selected', String(active));
    if (active) node.scrollIntoView({ block: 'nearest' });
  });
  if (input) {
    if (items.length) input.setAttribute('aria-activedescendant', `command-palette-option-${activeIndex}`);
    else input.removeAttribute('aria-activedescendant');
  }
}

function onKeydown(event) {
  if (event.isComposing) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    if (!items.length) return;
    activeIndex = (activeIndex + 1) % items.length;
    updateActive();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    if (!items.length) return;
    activeIndex = (activeIndex - 1 + items.length) % items.length;
    updateActive();
  } else if (event.key === 'Enter') {
    event.preventDefault();
    choose(activeIndex);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    close();
  } else if (event.key === 'Home') {
    event.preventDefault();
    activeIndex = 0;
    updateActive();
  } else if (event.key === 'End') {
    event.preventDefault();
    activeIndex = items.length - 1;
    updateActive();
  }
}

function choose(index) {
  const cmd = items[index];
  if (!cmd) return;
  close();
  run(cmd.id);
}
