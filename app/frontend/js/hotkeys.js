/**
 * 全局快捷键管理（FR-604/FR-606）。
 *
 * - 默认表：CONTRACT 第 5 节 / types.rs default_hotkeys() 的 11 个命令，外加
 *   前端补充的高频操作默认键（大纲、反链、主题、导入导出、备份、同步、面板切换）；
 * - 用户覆盖表来自 AppConfig.hotkeys（通过 hotkey_set 持久化，冲突由后端返回 1004）；
 * - 组合键规范化为 "Ctrl+Shift+P" 形式，便于比较与展示。
 */
import { state } from './state.js';

/** 后端默认快捷键（与 types.rs default_hotkeys() 保持一致）。 */
export const BACKEND_DEFAULTS = {
  'command.palette': 'Ctrl+Shift+P',
  'search.title': 'Ctrl+P',
  'search.full': 'Ctrl+Shift+F',
  'note.new': 'Ctrl+N',
  'note.save': 'Ctrl+S',
  'sidebar.toggle': 'Ctrl+\\',
  'toolbar.toggle': 'Ctrl+Shift+T',
  'preview.toggle': 'Ctrl+Shift+M',
  'trash.open': 'Ctrl+Shift+Backspace',
  'settings.open': 'Ctrl+,',
  'search.next': 'Ctrl+Shift+J',
};

/** 前端补充的默认快捷键（后端未知 id 时使用；用户可改，保存后写入 config.hotkeys）。 */
export const EXTRA_DEFAULTS = {
  'outline.toggle': 'Ctrl+Shift+O',
  'backlinks.toggle': 'Ctrl+Shift+B',
  'theme.toggle': 'Ctrl+Shift+D',
  'vault.import': 'Ctrl+Shift+I',
  'vault.export': 'Ctrl+Shift+E',
  'backup.now': 'Ctrl+Shift+K',
  'sync.now': 'Ctrl+Shift+Y',
  'panel.files': 'Ctrl+1',
  'panel.tags': 'Ctrl+2',
  'panel.trash': 'Ctrl+3',
  'panel.search': 'Ctrl+4',
};

export const DEFAULT_HOTKEYS = { ...BACKEND_DEFAULTS, ...EXTRA_DEFAULTS };

/** 设置页展示用的命令元数据（顺序即展示顺序）。 */
export const HOTKEY_COMMANDS = [
  { id: 'command.palette', label: '打开命令面板', group: '通用' },
  { id: 'search.title', label: '标题快速打开', group: '检索' },
  { id: 'search.full', label: '全文检索', group: '检索' },
  { id: 'search.next', label: '切换大纲面板', group: '视图' },
  { id: 'note.new', label: '新建笔记', group: '笔记' },
  { id: 'note.save', label: '保存当前笔记', group: '笔记' },
  { id: 'sidebar.toggle', label: '折叠/展开侧边栏', group: '视图' },
  { id: 'toolbar.toggle', label: '显示/隐藏格式工具栏', group: '视图' },
  { id: 'preview.toggle', label: '折叠/展开预览区', group: '视图' },
  { id: 'outline.toggle', label: '切换大纲面板', group: '视图' },
  { id: 'backlinks.toggle', label: '切换反向链接面板', group: '视图' },
  { id: 'theme.toggle', label: '切换主题（自动/亮/暗）', group: '视图' },
  { id: 'trash.open', label: '打开回收站面板', group: '面板' },
  { id: 'settings.open', label: '打开设置', group: '通用' },
  { id: 'panel.files', label: '切换到文件面板', group: '面板' },
  { id: 'panel.tags', label: '切换到标签面板', group: '面板' },
  { id: 'panel.trash', label: '切换到回收站面板', group: '面板' },
  { id: 'panel.search', label: '切换到检索入口面板', group: '面板' },
  { id: 'vault.import', label: '导入笔记', group: '文件' },
  { id: 'vault.export', label: '导出当前笔记', group: '文件' },
  { id: 'backup.now', label: '立即备份', group: '备份与同步' },
  { id: 'sync.now', label: '立即同步', group: '备份与同步' },
];

const COMMAND_LABELS = new Map(HOTKEY_COMMANDS.map((c) => [c.id, c.label]));

/** 当前生效的 commandId -> combo。 */
let bindings = { ...DEFAULT_HOTKEYS };
let reverse = new Map();
let onCommand = () => {};
let suspended = false;
let initialized = false;

/** 组合键规范化（用于比较/展示）。 */
export function canonicalizeCombo(combo) {
  const raw = String(combo || '').trim();
  if (!raw) return '';
  const parts = raw.split('+').map((p) => p.trim()).filter(Boolean);
  const mods = new Set();
  let key = '';
  for (const part of parts) {
    const low = part.toLowerCase();
    if (low === 'ctrl' || low === 'control') mods.add('Ctrl');
    else if (low === 'shift') mods.add('Shift');
    else if (low === 'alt' || low === 'option') mods.add('Alt');
    else if (low === 'meta' || low === 'cmd' || low === 'command' || low === 'win') mods.add('Meta');
    else key = part;
  }
  if (key.length === 1) key = key.toUpperCase();
  if (key.toLowerCase() === 'space') key = 'Space';
  const ordered = ['Ctrl', 'Shift', 'Alt', 'Meta'].filter((m) => mods.has(m));
  return [...ordered, key].join('+');
}

/** macOS 上把 Command 视作 Ctrl，保证默认快捷键跨平台可用。 */
const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent || '');

/** 从 KeyboardEvent 生成规范化组合键（仅修饰键按下时返回空串）。 */
export function comboFromEvent(event) {
  if (!event || event.isComposing) return '';
  const modifiers = [];
  if (event.ctrlKey || (IS_MAC && event.metaKey)) modifiers.push('Ctrl');
  if (event.shiftKey) modifiers.push('Shift');
  if (event.altKey) modifiers.push('Alt');
  if (event.metaKey && !IS_MAC) modifiers.push('Meta');
  const key = normalizeEventKey(event);
  if (!key) return '';
  if (!modifiers.length) {
    // 无修饰键的组合键仅在功能键上允许（避免抢占普通输入）
    if (!/^F\d{1,2}$/.test(key)) return '';
  }
  return [...modifiers, key].join('+');
}

function normalizeEventKey(event) {
  const key = event.key;
  if (!key || key === 'Unidentified' || key === 'Process') return '';
  if (['Control', 'Shift', 'Alt', 'Meta', 'OS', 'AltGraph'].includes(key)) return '';
  if (key === ' ') return 'Space';
  if (key.length === 1) {
    if (key === '+') return '+';
    return key.toUpperCase();
  }
  const named = {
    Escape: 'Escape',
    Enter: 'Enter',
    Tab: 'Tab',
    Backspace: 'Backspace',
    Delete: 'Delete',
    Insert: 'Insert',
    Home: 'Home',
    End: 'End',
    PageUp: 'PageUp',
    PageDown: 'PageDown',
    ArrowUp: 'ArrowUp',
    ArrowDown: 'ArrowDown',
    ArrowLeft: 'ArrowLeft',
    ArrowRight: 'ArrowRight',
  };
  if (named[key]) return named[key];
  if (/^F\d{1,2}$/.test(key)) return key;
  return key.length <= 3 ? key.toUpperCase() : '';
}

function rebuildReverse() {
  reverse = new Map();
  for (const [id, combo] of Object.entries(bindings)) {
    const canon = canonicalizeCombo(combo);
    if (canon) reverse.set(canon, id);
  }
}

/** 初始化（只允许一次）。 */
export function init(opts = {}) {
  if (opts.onCommand) onCommand = opts.onCommand;
  if (!initialized) {
    document.addEventListener('keydown', handleKeydown, true);
    initialized = true;
  }
  setFromConfig(state.config && state.config.hotkeys);
}

/** 用 AppConfig.hotkeys 覆盖默认表。 */
export function setFromConfig(hotkeys) {
  bindings = { ...DEFAULT_HOTKEYS };
  if (hotkeys && typeof hotkeys === 'object') {
    for (const [id, combo] of Object.entries(hotkeys)) {
      if (!(id in bindings)) continue;
      bindings[id] = combo == null ? '' : String(combo);
    }
  }
  rebuildReverse();
}

/** 设置某命令组合键（本地即时生效；持久化由调用方通过 hotkey_set 完成）。 */
export function setCombo(commandId, combo) {
  bindings[commandId] = canonicalizeCombo(combo);
  rebuildReverse();
}

/** 恢复默认组合键（本地）。 */
export function resetCombo(commandId) {
  bindings[commandId] = DEFAULT_HOTKEYS[commandId] || '';
  rebuildReverse();
}

/** 查询命令当前组合键。 */
export function comboFor(commandId) {
  return bindings[commandId] || '';
}

/** 命令中文名。 */
export function labelFor(commandId) {
  return COMMAND_LABELS.get(commandId) || commandId;
}

/** 查找冲突（返回 {id,label,combo} 或 null）。 */
export function findConflict(combo, excludeCommandId) {
  const canon = canonicalizeCombo(combo);
  if (!canon) return null;
  for (const [id, value] of Object.entries(bindings)) {
    if (id === excludeCommandId) continue;
    if (canonicalizeCombo(value) === canon) return { id, label: labelFor(id), combo: value };
  }
  return null;
}

/** 全部命令的有效快捷键（设置页渲染用）。 */
export function listBindings() {
  return HOTKEY_COMMANDS.map((cmd) => ({ ...cmd, combo: bindings[cmd.id] || '' }));
}

/** 暂时挂起全局快捷键（设置页录制新快捷键时使用）。 */
export function suspend() {
  suspended = true;
}

export function resume() {
  suspended = false;
}

function handleKeydown(event) {
  if (suspended) return;
  if (event.isComposing) return;
  const combo = comboFromEvent(event);
  if (!combo) return;
  const commandId = reverse.get(combo);
  if (!commandId) return;
  event.preventDefault();
  event.stopPropagation();
  try {
    onCommand(commandId);
  } catch (err) {
    console.error('[MoNote] 快捷键命令执行失败', err);
  }
}

/** 处理一次按键（设置页录制用），返回规范化组合键；忽略纯修饰键。 */
export function captureCombo(event) {
  return comboFromEvent(event);
}

export default {
  init,
  setFromConfig,
  setCombo,
  resetCombo,
  comboFor,
  labelFor,
  findConflict,
  listBindings,
  suspend,
  resume,
  captureCombo,
  canonicalizeCombo,
  HOTKEY_COMMANDS,
  DEFAULT_HOTKEYS,
};
