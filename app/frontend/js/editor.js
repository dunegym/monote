/**
 * 编辑区：Markdown textarea + 格式工具栏 + 自动保存 + 图片粘贴 + 双链补全 + 属性/反链面板。
 *
 * 保存策略（对应设计 5.1 / FR-105）：
 * - 输入停顿 config.editor.autosaveDebounceMs（默认 2s）自动保存；
 * - 切换笔记前 actions.openNote() 会调用 flush()；
 * - 窗口失焦 / 页面隐藏 / 卸载前尽力保存；
 * - Ctrl+S 走全局快捷键 → saveNow({manual:true})；
 * - 修订号冲突（2004）弹窗让用户选择重新加载 / 覆盖保存。
 */
import { ctx } from './ctx.js';
import { state, setSaveState, setState, emit } from './state.js';
import {
  debounce,
  clamp,
  formatDateTime,
  splitFrontMatter,
  setFrontMatterFields,
  titleOf,
  tagsOf,
  textStats,
} from './util.js';
import { qs, el } from './dom.js';
import { promptDialog, openModal, button } from './modal.js';
import { toast } from './toast.js';

const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const SUPPORTED_IMAGE_TYPES = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/jpg': 'jpg', 'image/webp': 'webp', 'image/gif': 'gif' };
const AUTOSAVE_RETRY_MS = 30000;

// ---------------------------------------------------------------------------
// 模块状态
// ---------------------------------------------------------------------------
let textarea = null;
let toolbar = null;
let propsPanel = null;
let backlinksPanel = null;
let backlinksList = null;
let suggestBox = null;
let initialized = false;

/** @type {null | {path:string,title:string,content:string,rev:number,mtime:number,tags:string[],created?:string,updated?:string}} */
let current = null;
let lastSavedContent = '';
let rev = 0;
let dirty = false;
let saving = false;
let saveQueued = false;
let autosaveTimer = null;
let retryTimer = null;
let saveIdleResolvers = [];
let suppressInput = false;

// 双链补全
let suggestItems = [];
let suggestIndex = 0;
let suggestStart = 0;
let suggestTimer = null;
let suggestToken = 0;
let suggestOpen = false;

// 反链
let backlinksToken = 0;
let backlinksOpen = false;

const previewDebounced = debounce(() => {
  if (!current) return;
  const body = textarea.value;
  ctx.preview.render(body, { path: current.path, title: current.title });
}, 150);

const wordsDebounced = debounce(() => updateWords(), 120);
const backlinksDebounced = debounce(() => refreshBacklinks(), 400);

// ---------------------------------------------------------------------------
// 工具栏定义
// ---------------------------------------------------------------------------
const TOOLBAR_ITEMS = [
  { id: 'bold', title: '加粗 (Ctrl+B)', html: '<strong>B</strong>', aria: '加粗' },
  { id: 'italic', title: '斜体 (Ctrl+I)', html: '<em>I</em>', aria: '斜体' },
  { id: 'strike', title: '删除线', html: '<span style="text-decoration:line-through">S</span>', aria: '删除线' },
  { divider: true },
  { id: 'h1', title: '一级标题', html: 'H1', aria: '一级标题' },
  { id: 'h2', title: '二级标题', html: 'H2', aria: '二级标题' },
  { id: 'h3', title: '三级标题', html: 'H3', aria: '三级标题' },
  { divider: true },
  { id: 'ul', title: '无序列表', html: '• 列表', aria: '无序列表' },
  { id: 'ol', title: '有序列表', html: '1. 列表', aria: '有序列表' },
  { id: 'task', title: '任务列表', html: '☐ 任务', aria: '任务列表' },
  { id: 'quote', title: '引用', html: '❝ 引用', aria: '引用' },
  { divider: true },
  { id: 'code', title: '行内代码', html: '&lt;/&gt;', aria: '行内代码' },
  { id: 'codeblock', title: '代码块', html: '⌗ 代码块', aria: '代码块' },
  { id: 'link', title: '插入链接', html: '🔗 链接', aria: '插入链接' },
  { id: 'table', title: '插入表格', html: '▦ 表格', aria: '插入表格' },
  { id: 'math', title: '插入公式', html: '∑ 公式', aria: '插入公式' },
  { id: 'hr', title: '分隔线', html: '—', aria: '插入分隔线' },
];

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------
export function init() {
  if (initialized) return getPublicApi();
  textarea = qs('#editor');
  toolbar = qs('#format-toolbar');
  propsPanel = qs('#properties-panel');
  backlinksPanel = qs('#backlinks-panel');
  backlinksList = qs('#backlinks-list');
  suggestBox = qs('#link-suggest');
  if (!textarea) {
    console.error('[MoNote] 缺少 #editor 元素');
    return getPublicApi();
  }

  buildToolbar();
  bindToolbar();
  bindEditorEvents();
  bindProperties();
  bindBacklinks();
  bindWindowFlush();
  document.body.classList.add('no-note');
  clearNote();
  initialized = true;
  return getPublicApi();
}

function getPublicApi() {
  return {
    setNote,
    clearNote,
    getValue: () => (textarea ? textarea.value : ''),
    saveNow,
    flush,
    format,
    insertText: (text) => insertAtCursor(text),
    revealLine,
    focus: () => textarea && textarea.focus(),
    refreshBacklinks,
    handleExternalChange,
    isDirty: () => dirty,
    getPath: () => (current ? current.path : null),
    showProperties,
    closeSuggest,
  };
}

// ---------------------------------------------------------------------------
// 工具栏
// ---------------------------------------------------------------------------
function buildToolbar() {
  if (!toolbar) return;
  toolbar.innerHTML = '';
  for (const item of TOOLBAR_ITEMS) {
    if (item.divider) {
      toolbar.appendChild(el('span', { className: 'toolbar-divider', 'aria-hidden': 'true' }));
      continue;
    }
    const btn = el('button', {
      type: 'button',
      className: 'toolbar-btn',
      title: item.title,
      'aria-label': item.aria,
      dataset: { format: item.id },
      html: item.html,
    });
    toolbar.appendChild(btn);
  }
}

function bindToolbar() {
  toolbar.addEventListener('click', (event) => {
    const btn = event.target.closest('[data-format]');
    if (!btn) return;
    format(btn.dataset.format);
  });
}

/** 执行格式命令（工具栏/命令面板共用）。 */
export function format(action) {
  if (!textarea) return;
  if (!current) {
    toast.info('请先打开或新建一篇笔记');
    return;
  }
  switch (action) {
    case 'bold': wrapSelection('**', '**', '加粗文字'); break;
    case 'italic': wrapSelection('*', '*', '斜体文字'); break;
    case 'strike': wrapSelection('~~', '~~', '删除线文字'); break;
    case 'h1': toggleHeading(1); break;
    case 'h2': toggleHeading(2); break;
    case 'h3': toggleHeading(3); break;
    case 'ul': toggleLinePrefix('ul'); break;
    case 'ol': toggleLinePrefix('ol'); break;
    case 'task': toggleLinePrefix('task'); break;
    case 'quote': toggleLinePrefix('quote'); break;
    case 'code': wrapSelection('`', '`', 'code'); break;
    case 'codeblock': insertCodeBlock(); break;
    case 'link': insertLink(); break;
    case 'table': insertTable(); break;
    case 'math': insertMath(); break;
    case 'hr': insertBlock('\n---\n'); break;
    default: break;
  }
  textarea.focus();
}

function replaceRange(start, end, text, selStart, selEnd) {
  const ta = textarea;
  ta.focus();
  ta.setSelectionRange(start, end);
  let ok = false;
  try {
    ok = document.execCommand('insertText', false, text);
  } catch {
    ok = false;
  }
  if (!ok) {
    ta.setRangeText(text, start, end, 'end');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
  }
  const selA = selStart == null ? start + text.length : selStart;
  const selB = selEnd == null ? selA : selEnd;
  requestAnimationFrame(() => {
    try {
      ta.setSelectionRange(selA, selB);
    } catch {
      /* ignore */
    }
  });
}

function wrapSelection(before, after, placeholder) {
  const ta = textarea;
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const selected = ta.value.slice(start, end);
  const content = selected || placeholder;
  const text = `${before}${content}${after}`;
  replaceRange(start, end, text, start + before.length, start + before.length + content.length);
}

function selectedLineRange() {
  const ta = textarea;
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const value = ta.value;
  const lineStart = value.lastIndexOf('\n', start - 1) + 1;
  let lineEnd = value.indexOf('\n', end);
  if (lineEnd < 0) lineEnd = value.length;
  return { start, end, lineStart, lineEnd, value };
}

function toggleHeading(level) {
  const { lineStart, lineEnd, value } = selectedLineRange();
  const block = value.slice(lineStart, lineEnd);
  const lines = block.split('\n');
  const prefix = `${'#'.repeat(level)} `;
  const nonEmpty = lines.filter((l) => l.trim());
  const allSame = nonEmpty.length > 0 && nonEmpty.every((l) => l.trimStart().startsWith(prefix));
  const next = lines.map((line) => {
    const stripped = line.replace(/^\s*#{1,6}\s+/, '');
    return allSame ? stripped : `${prefix}${stripped}`;
  }).join('\n');
  replaceRange(lineStart, lineEnd, next, lineStart, lineStart + next.length);
}

function toggleLinePrefix(kind) {
  const { lineStart, lineEnd, value } = selectedLineRange();
  const block = value.slice(lineStart, lineEnd);
  const lines = block.split('\n');
  const nonEmpty = lines.filter((l) => l.trim());

  const stripPrefix = (line) => line
    .replace(/^\s*>+\s?/, '')
    .replace(/^\s*[-*+]\s+\[[ xX]\]\s+/, '')
    .replace(/^\s*[-*+]\s+/, '')
    .replace(/^\s*\d+\.\s+/, '');

  if (kind === 'quote') {
    const all = nonEmpty.length > 0 && nonEmpty.every((l) => /^\s*>\s?/.test(l));
    const next = lines.map((l) => (all ? l.replace(/^\s*>\s?/, '') : (l.trim() ? `> ${stripPrefix(l)}` : l))).join('\n');
    replaceRange(lineStart, lineEnd, next, lineStart, lineStart + next.length);
    return;
  }
  if (kind === 'ul' || kind === 'task') {
    const test = kind === 'ul' ? /^\s*[-*+]\s+/.test : /^\s*[-*+]\s+\[[ xX]\]\s+/.test;
    const all = nonEmpty.length > 0 && nonEmpty.every((l) => test(l));
    const next = lines.map((line) => {
      if (!line.trim()) return line;
      const body = stripPrefix(line);
      if (all) return body;
      return kind === 'ul' ? `- ${body}` : `- [ ] ${body}`;
    }).join('\n');
    replaceRange(lineStart, lineEnd, next, lineStart, lineStart + next.length);
    return;
  }
  if (kind === 'ol') {
    const all = nonEmpty.length > 0 && nonEmpty.every((l) => /^\s*\d+\.\s+/.test(l));
    let n = 0;
    const next = lines.map((line) => {
      if (!line.trim()) return line;
      const body = stripPrefix(line);
      if (all) return body;
      n += 1;
      return `${n}. ${body}`;
    }).join('\n');
    replaceRange(lineStart, lineEnd, next, lineStart, lineStart + next.length);
  }
}

function insertBlock(text) {
  const ta = textarea;
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const value = ta.value;
  const needsBefore = start > 0 && value[start - 1] !== '\n' ? '\n' : '';
  const needsAfter = end < value.length && value[end] !== '\n' ? '\n' : '';
  const insert = `${needsBefore}${text}${needsAfter}`;
  replaceRange(start, end, insert, start + insert.length, start + insert.length);
  dirty = true;
}

function insertCodeBlock() {
  const ta = textarea;
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const selected = ta.value.slice(start, end);
  const text = selected
    ? `\`\`\`\n${selected}\n\`\`\`\n`
    : '```\n\n```\n';
  const contentStart = start + 4;
  replaceRange(start, end, text, contentStart, contentStart + selected.length);
}

async function insertLink() {
  const ta = textarea;
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const selected = ta.value.slice(start, end);
  let text = selected;
  if (!text) {
    text = await promptDialog({
      title: '插入链接',
      label: '链接文字',
      value: '链接文字',
      confirmLabel: '下一步',
      validate: (v) => (v ? null : '请输入链接文字'),
    });
    if (!text) return;
  }
  const url = await promptDialog({
    title: '插入链接',
    label: '链接地址（URL）',
    value: 'https://',
    confirmLabel: '插入',
    help: '示例：https://example.com 或 notes/某篇笔记.md',
    validate: (v) => (v ? null : '请输入链接地址'),
  });
  if (!url) return;
  const insert = `[${text}](${url})`;
  replaceRange(start, end, insert, start + insert.length, start + insert.length);
}

function insertTable() {
  const template = '\n| 列 1 | 列 2 | 列 3 |\n| --- | --- | --- |\n| 内容 | 内容 | 内容 |\n';
  insertBlock(template);
}

function insertMath() {
  const ta = textarea;
  const selected = ta.value.slice(ta.selectionStart, ta.selectionEnd);
  if (selected.includes('\n')) {
    wrapSelection('$$\n', '\n$$', 'E = mc^2');
  } else {
    wrapSelection('$', '$', selected || 'E=mc^2');
  }
}

// ---------------------------------------------------------------------------
// 编辑器事件
// ---------------------------------------------------------------------------
function bindEditorEvents() {
  textarea.addEventListener('input', () => {
    if (suppressInput) return;
    markDirty();
  });
  textarea.addEventListener('keydown', onKeydown);
  textarea.addEventListener('paste', onPaste);
  textarea.addEventListener('dragover', (event) => {
    if (event.dataTransfer && [...event.dataTransfer.types].includes('Files')) {
      event.preventDefault();
      textarea.classList.add('drag-over');
    }
  });
  textarea.addEventListener('dragleave', () => textarea.classList.remove('drag-over'));
  textarea.addEventListener('drop', (event) => {
    textarea.classList.remove('drag-over');
    const files = [...(event.dataTransfer ? event.dataTransfer.files : [])];
    const image = files.find((f) => f && f.type && f.type.startsWith('image/'));
    if (image) {
      event.preventDefault();
      handleImageFile(image);
    }
  });
  textarea.addEventListener('scroll', () => closeSuggest());
  textarea.addEventListener('blur', () => {
    setTimeout(() => {
      if (document.activeElement !== textarea && !suggestBox.contains(document.activeElement)) closeSuggest();
    }, 120);
  });
  textarea.addEventListener('click', () => refreshSuggestIfOpen());
}

function markDirty() {
  dirty = true;
  setSaveState('unsaved', true);
  schedulePreview();
  scheduleAutosave();
  wordsDebounced();
  detectSuggest();
}

function schedulePreview() {
  previewDebounced();
}

function scheduleAutosave() {
  if (autosaveTimer) clearTimeout(autosaveTimer);
  const delay = clamp(Number(state.config && state.config.editor && state.config.editor.autosaveDebounceMs) || 2000, 300, 60000);
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    saveNow({ auto: true }).catch(() => {});
  }, delay);
}

function updateWords() {
  if (!current || !textarea) return;
  const fm = splitFrontMatter(textarea.value);
  const stats = textStats(fm.body);
  const wordEl = qs('#status-words');
  if (wordEl) {
    wordEl.textContent = `${stats.words} 字`;
    wordEl.title = `字数 ${stats.words}（不含空白） · 字符 ${stats.chars} · ${stats.lines} 行`;
  }
  const docEl = qs('#status-doc');
  if (docEl) docEl.textContent = current.title || current.path || '未打开笔记';
}

function onKeydown(event) {
  if (event.isComposing || event.keyCode === 229) return;

  // 双链补全键盘操作
  if (suggestOpen) {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      moveSuggest(1);
      return;
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault();
      moveSuggest(-1);
      return;
    }
    if (event.key === 'Enter' || event.key === 'Tab') {
      event.preventDefault();
      chooseSuggest(suggestIndex);
      return;
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      closeSuggest();
      return;
    }
  }

  if ((event.ctrlKey || event.metaKey) && !event.altKey) {
    const key = event.key.toLowerCase();
    if (key === 'b') {
      event.preventDefault();
      format('bold');
      return;
    }
    if (key === 'i') {
      event.preventDefault();
      format('italic');
      return;
    }
  }

  if (event.key === 'Tab' && !event.shiftKey) {
    event.preventDefault();
    replaceRange(textarea.selectionStart, textarea.selectionEnd, '  ', textarea.selectionStart + 2, textarea.selectionStart + 2);
    return;
  }

  if (event.key === 'Enter' && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
    if (continueList()) return;
  }
}

function continueList() {
  const ta = textarea;
  const pos = ta.selectionStart;
  if (pos !== ta.selectionEnd) return false;
  const value = ta.value;
  const lineStart = value.lastIndexOf('\n', pos - 1) + 1;
  const line = value.slice(lineStart, pos);
  const m = /^(\s*)([-*+]|\d+\.)([ \t]+)(\[[ xX]\][ \t]+)?$/.exec(line);
  if (!m) return false;
  const indent = m[1];
  const marker = m[2];
  const task = m[4] ? '[ ] ' : '';
  if (line.trim() === marker || line.trim() === `${marker} ${task}`.trim()) {
    // 空列表项 → 结束列表
    replaceRange(lineStart, pos, '', lineStart, lineStart);
    return true;
  }
  const nextMarker = /^\d+\.$/.test(marker) ? `${parseInt(marker, 10) + 1}.` : marker;
  const insert = `\n${indent}${nextMarker} ${task}`;
  replaceRange(pos, pos, insert, pos + insert.length, pos + insert.length);
  return true;
}

// ---------------------------------------------------------------------------
// 图片粘贴
// ---------------------------------------------------------------------------
function onPaste(event) {
  const items = event.clipboardData ? [...event.clipboardData.items] : [];
  const imageItem = items.find((item) => item.kind === 'file' && item.type && item.type.startsWith('image/'));
  if (!imageItem) return; // 普通文本粘贴走默认行为
  event.preventDefault();
  const file = imageItem.getAsFile();
  if (file) handleImageFile(file);
}

async function handleImageFile(file) {
  if (!SUPPORTED_IMAGE_TYPES[file.type]) {
    toast.warn('图片格式不支持，仅支持 PNG / JPEG / WebP / GIF');
    return;
  }
  if (file.size > MAX_IMAGE_BYTES) {
    toast.warn('图片超过 10 MB 限制，未插入');
    return;
  }
  let base64 = '';
  try {
    base64 = await readFileAsBase64(file);
  } catch (err) {
    toast.error('读取剪贴板图片失败');
    return;
  }
  const ext = SUPPORTED_IMAGE_TYPES[file.type] || 'png';
  const fileName = `IMG-${Date.now()}.${ext}`;
  try {
    const res = await ctx.api.call('vault_save_attachment', { fileName, dataBase64: base64 });
    if (!res || !res.path) throw new Error('附件保存未返回路径');
    insertAtCursor(`![](<${res.path}>)`);
    toast.success('图片已保存到 attachments/ 并插入链接');
  } catch (err) {
    toast.error(`图片保存失败：${ctx.api.friendlyError(err)}`);
  }
}

function readFileAsBase64(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result || '');
      const idx = result.indexOf(',');
      resolve(idx >= 0 ? result.slice(idx + 1) : result);
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

// ---------------------------------------------------------------------------
// 双链自动补全
// ---------------------------------------------------------------------------
function detectSuggest() {
  const ta = textarea;
  if (!ta || ta.selectionStart !== ta.selectionEnd) {
    closeSuggest();
    return;
  }
  const pos = ta.selectionStart;
  const before = ta.value.slice(Math.max(0, pos - 400), pos);
  const m = /\[\[([^\[\]\n|]{0,80})$/.exec(before);
  if (!m) {
    closeSuggest();
    return;
  }
  const prefix = m[1];
  suggestStart = pos - prefix.length - 2;
  if (suggestTimer) clearTimeout(suggestTimer);
  suggestTimer = setTimeout(() => {
    suggestTimer = null;
    querySuggest(prefix);
  }, 120);
}

function refreshSuggestIfOpen() {
  if (!suggestOpen) return;
  const pos = textarea.selectionStart;
  const before = textarea.value.slice(Math.max(0, pos - 400), pos);
  const m = /\[\[([^\[\]\n|]{0,80})$/.exec(before);
  if (m) {
    suggestStart = pos - m[1].length - 2;
    querySuggest(m[1]);
  } else {
    closeSuggest();
  }
}

async function querySuggest(prefix) {
  const token = (suggestToken += 1);
  try {
    const res = await ctx.api.call('link_suggest', { prefix });
    if (token !== suggestToken) return;
    if (!textarea || document.activeElement !== textarea) return;
    suggestItems = (res && Array.isArray(res.items) ? res.items : []).slice(0, 10);
    if (!suggestItems.length) {
      renderSuggestEmpty(prefix);
      openSuggest();
      return;
    }
    suggestIndex = 0;
    renderSuggest();
    openSuggest();
  } catch (err) {
    console.warn('[MoNote] 双链补全失败', err);
    closeSuggest();
  }
}

function renderSuggestEmpty(prefix) {
  suggestBox.innerHTML = '';
  suggestBox.appendChild(el('div', {
    className: 'link-suggest-empty',
    text: prefix ? `没有匹配「${prefix}」的笔记，直接输入即可；预览中点击缺失链接可创建。` : '还没有笔记，可先在预览中创建缺失链接。',
  }));
}

function renderSuggest() {
  suggestBox.innerHTML = '';
  suggestItems.forEach((item, index) => {
    const node = el('div', {
      className: `link-suggest-item${index === suggestIndex ? ' active' : ''}`,
      role: 'option',
      id: `link-suggest-option-${index}`,
      'aria-selected': index === suggestIndex ? 'true' : 'false',
      dataset: { index: String(index) },
      title: item.path,
    }, [
      el('span', { className: 'link-suggest-name', text: item.name }),
      el('span', { className: 'link-suggest-path', text: item.path }),
    ]);
    node.addEventListener('mousedown', (event) => {
      event.preventDefault();
      chooseSuggest(index);
    });
    suggestBox.appendChild(node);
  });
}

function openSuggest() {
  suggestOpen = true;
  suggestBox.hidden = false;
  positionSuggest();
}

function closeSuggest() {
  if (suggestTimer) {
    clearTimeout(suggestTimer);
    suggestTimer = null;
  }
  suggestToken += 1;
  suggestOpen = false;
  suggestItems = [];
  if (suggestBox) {
    suggestBox.hidden = true;
    suggestBox.innerHTML = '';
  }
}

function moveSuggest(delta) {
  if (!suggestItems.length) return;
  suggestIndex = (suggestIndex + delta + suggestItems.length) % suggestItems.length;
  renderSuggest();
  positionSuggest();
  const active = suggestBox.querySelector('.link-suggest-item.active');
  if (active) active.scrollIntoView({ block: 'nearest' });
}

function chooseSuggest(index) {
  const item = suggestItems[index];
  if (!item) {
    closeSuggest();
    return;
  }
  const pos = textarea.selectionStart;
  const insert = `[[${item.name}]]`;
  closeSuggest();
  replaceRange(suggestStart, pos, insert, suggestStart + insert.length, suggestStart + insert.length);
}

function positionSuggest() {
  if (!suggestOpen || !suggestBox) return;
  const coords = caretCoords();
  const parent = suggestBox.offsetParent || suggestBox.parentElement;
  const parentWidth = parent ? parent.clientWidth : window.innerWidth;
  const parentHeight = parent ? parent.clientHeight : window.innerHeight;
  const boxWidth = suggestBox.offsetWidth || 260;
  const boxHeight = suggestBox.offsetHeight || 200;
  const left = clamp(coords.left, 4, Math.max(4, parentWidth - boxWidth - 8));
  let top = coords.top;
  if (top + boxHeight > parentHeight - 8) top = Math.max(4, coords.top - boxHeight - 26);
  suggestBox.style.left = `${Math.round(left)}px`;
  suggestBox.style.top = `${Math.round(top)}px`;
}

function caretCoords() {
  const ta = textarea;
  const style = window.getComputedStyle(ta);
  const mirror = document.createElement('div');
  const props = [
    'fontFamily', 'fontSize', 'fontWeight', 'lineHeight', 'letterSpacing', 'textTransform',
    'textIndent', 'whiteSpace', 'wordWrap', 'wordBreak', 'overflowWrap', 'boxSizing', 'tabSize',
  ];
  for (const prop of props) mirror.style[prop] = style[prop];
  mirror.style.position = 'absolute';
  mirror.style.visibility = 'hidden';
  mirror.style.pointerEvents = 'none';
  mirror.style.top = '0';
  mirror.style.left = '-9999px';
  mirror.style.width = `${ta.clientWidth}px`;
  mirror.style.padding = style.padding;
  mirror.style.border = style.border;
  mirror.style.whiteSpace = 'pre-wrap';
  mirror.style.overflowWrap = 'anywhere';
  mirror.textContent = ta.value.slice(0, ta.selectionStart);
  const marker = document.createElement('span');
  marker.textContent = '\u200b';
  mirror.appendChild(marker);
  document.body.appendChild(mirror);
  const top = marker.offsetTop - ta.scrollTop + ta.offsetTop;
  const left = marker.offsetLeft - ta.scrollLeft + ta.offsetLeft;
  mirror.remove();
  return {
    top: Math.max(4, Math.min(top, ta.offsetTop + ta.clientHeight - 10)),
    left: Math.max(4, Math.min(left, ta.offsetLeft + ta.clientWidth - 40)),
  };
}

// ---------------------------------------------------------------------------
// 属性面板
// ---------------------------------------------------------------------------
function bindProperties() {
  const toggle = qs('#btn-properties');
  const apply = qs('#btn-props-apply');
  const close = qs('#btn-props-close');
  const applyAndClose = () => {
    applyProperties();
    closeProperties();
  };
  if (toggle) {
    toggle.addEventListener('click', () => {
      const open = propsPanel.hidden;
      if (open) showProperties();
      else closeProperties();
    });
  }
  if (apply) apply.addEventListener('click', applyAndClose);
  if (close) close.addEventListener('click', closeProperties);
  const titleInput = qs('#prop-title');
  const tagsInput = qs('#prop-tags');
  for (const input of [titleInput, tagsInput]) {
    if (!input) continue;
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && !event.isComposing) {
        event.preventDefault();
        applyAndClose();
      }
    });
  }
}

function showProperties() {
  if (!current) {
    toast.info('请先打开一篇笔记');
    return;
  }
  const fm = splitFrontMatter(textarea.value);
  const parsed = fm.frontMatter || '';
  const titleMatch = /(^|\n)\s*title\s*:\s*(.+?)(?:\n|$)/.exec(parsed);
  const tagsMatch = /(^|\n)\s*tags\s*:\s*(.+?)(?:\n|$)/.exec(parsed);
  let titleValue = titleMatch ? titleMatch[2].trim().replace(/^["']|["']$/g, '') : (current.title || '');
  let tagsValue = '';
  if (tagsMatch) {
    const raw = tagsMatch[2].trim();
    if (raw.startsWith('[') && raw.endsWith(']')) {
      tagsValue = raw.slice(1, -1).split(',').map((s) => s.trim().replace(/^["']|["']$/g, '')).filter(Boolean).join(', ');
    } else {
      tagsValue = raw;
    }
  } else {
    tagsValue = (current.tags || []).join(', ');
  }
  qs('#prop-title').value = titleValue || '';
  qs('#prop-tags').value = tagsValue || '';
  const created = toDateText(current.created);
  const updated = toDateText(current.updated);
  qs('#prop-dates').textContent = `${created} / ${updated}`;
  propsPanel.hidden = false;
  const toggle = qs('#btn-properties');
  if (toggle) toggle.setAttribute('aria-expanded', 'true');
  qs('#prop-title').focus();
}

function closeProperties() {
  if (!propsPanel) return;
  propsPanel.hidden = true;
  const toggle = qs('#btn-properties');
  if (toggle) toggle.setAttribute('aria-expanded', 'false');
}

function toDateText(isoText) {
  if (!isoText) return '—';
  const ts = Date.parse(isoText);
  if (Number.isNaN(ts)) return String(isoText);
  return formatDateTime(ts);
}

function applyProperties() {
  if (!current || !textarea) return;
  const titleValue = qs('#prop-title').value.trim();
  const tagsValue = qs('#prop-tags').value;
  const tags = tagsValue
    .split(/[,，]/)
    .map((t) => t.trim().replace(/^#/, ''))
    .filter(Boolean);
  const nextContent = setFrontMatterFields(textarea.value, {
    title: titleValue || null,
    tags: tags.length ? tags : null,
  });
  if (nextContent === textarea.value) {
    toast.info('属性没有变化');
    return;
  }
  setEditorContent(nextContent);
  if (titleValue) {
    current.title = titleValue;
    const titleEl = qs('#note-title');
    if (titleEl) titleEl.textContent = titleValue;
  }
  const tagsEl = qs('#prop-tags');
  if (tagsEl) tagsEl.value = tags.join(', ');
  toast.success('属性已更新，将在自动保存后写入磁盘');
  schedulePreview();
}

function setEditorContent(nextContent) {
  if (!textarea) return;
  const scroll = textarea.scrollTop;
  const start = textarea.selectionStart;
  textarea.value = nextContent;
  textarea.scrollTop = scroll;
  try {
    textarea.setSelectionRange(Math.min(start, nextContent.length), Math.min(start, nextContent.length));
  } catch {
    /* ignore */
  }
  textarea.dispatchEvent(new Event('input', { bubbles: true }));
}

// ---------------------------------------------------------------------------
// 反链面板
// ---------------------------------------------------------------------------
function bindBacklinks() {
  const toggle = qs('#btn-backlinks');
  const close = qs('#btn-backlinks-close');
  if (toggle) {
    toggle.addEventListener('click', () => {
      if (backlinksOpen) closeBacklinks();
      else openBacklinks();
    });
  }
  if (close) close.addEventListener('click', closeBacklinks);
}

function openBacklinks() {
  if (!current) {
    toast.info('请先打开一篇笔记');
    return;
  }
  backlinksOpen = true;
  backlinksPanel.hidden = false;
  const toggle = qs('#btn-backlinks');
  if (toggle) toggle.setAttribute('aria-expanded', 'true');
  refreshBacklinks();
}

function closeBacklinks() {
  backlinksOpen = false;
  if (backlinksPanel) backlinksPanel.hidden = true;
  const toggle = qs('#btn-backlinks');
  if (toggle) toggle.setAttribute('aria-expanded', 'false');
}

export async function refreshBacklinks() {
  if (!current) {
    if (backlinksList) backlinksList.innerHTML = '<p class="backlinks-empty">打开笔记后显示反向链接。</p>';
    const count = qs('#backlinks-count');
    if (count) count.textContent = '';
    return;
  }
  const token = (backlinksToken += 1);
  const path = current.path;
  try {
    const res = await ctx.api.call('link_backlinks', { path });
    if (token !== backlinksToken || !current || current.path !== path) return;
    const items = (res && Array.isArray(res.items)) ? res.items : [];
    renderBacklinks(items);
  } catch (err) {
    if (token !== backlinksToken) return;
    if (backlinksList) {
      backlinksList.innerHTML = '';
      backlinksList.appendChild(el('p', { className: 'backlinks-empty', text: `反链加载失败：${ctx.api.friendlyError(err)}` }));
    }
  }
}

function renderBacklinks(items) {
  const count = qs('#backlinks-count');
  if (count) count.textContent = items.length ? `(${items.length})` : '';
  const summary = qs('#backlinks-summary');
  if (summary) summary.textContent = items.length ? `共 ${items.length} 处引用` : '';
  if (!backlinksList) return;
  backlinksList.innerHTML = '';
  if (!items.length) {
    backlinksList.appendChild(el('p', { className: 'backlinks-empty', text: '还没有其他笔记引用本篇。在正文中输入 [[本篇标题]] 即可建立反链。' }));
    return;
  }
  items.forEach((item) => {
    const node = el('div', {
      className: 'backlink-item',
      role: 'button',
      tabindex: '0',
      title: `打开 ${item.srcPath} 并定位到第 ${(item.line || 0) + 1} 行`,
    }, [
      el('div', { className: 'backlink-source' }, [
        el('span', { text: item.srcTitle || item.srcPath }),
        el('span', { className: 'line-no', text: `第 ${(item.line || 0) + 1} 行` }),
      ]),
      el('div', { className: 'backlink-context', text: item.context || '' }),
    ]);
    const activate = () => {
      ctx.actions.openNote(item.srcPath, { line: item.line || 0 }).catch((err) => {
        toast.error(`打开反链来源失败：${ctx.api.friendlyError(err)}`);
      });
    };
    node.addEventListener('click', activate);
    node.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        activate();
      }
    });
    backlinksList.appendChild(node);
  });
}

// ---------------------------------------------------------------------------
// 保存
// ---------------------------------------------------------------------------
function markSaveIdle() {
  saving = false;
  const resolvers = saveIdleResolvers;
  saveIdleResolvers = [];
  for (const resolve of resolvers) resolve();
}

function waitSaveIdle(timeout = 4000) {
  if (!saving) return Promise.resolve();
  return new Promise((resolve) => {
    saveIdleResolvers.push(resolve);
    setTimeout(resolve, timeout);
  });
}

/**
 * 保存当前笔记。
 * @param {{manual?:boolean, auto?:boolean, force?:boolean}} [opts]
 * @returns {Promise<boolean>} 是否已保存成功（无修改视为成功）
 */
export async function saveNow(opts = {}) {
  const manual = !!opts.manual;
  if (!current || !textarea) {
    if (manual) toast.info('请先打开一篇笔记');
    return false;
  }
  if (saving) {
    saveQueued = true;
    if (manual) setSaveState('saving', true);
    return false;
  }
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  const content = textarea.value;
  if (!dirty && content === lastSavedContent) {
    if (manual) toast.info('没有需要保存的修改');
    return true;
  }
  const notePath = current.path;
  const baseContent = lastSavedContent;
  saving = true;
  saveQueued = false;
  if (autosaveTimer) {
    clearTimeout(autosaveTimer);
    autosaveTimer = null;
  }
  setSaveState('saving', true);
  try {
    const result = await ctx.api.call('vault_write', { path: notePath, content, expectRev: rev });
    if (!current || current.path !== notePath) return true;
    lastSavedContent = content;
    rev = Number(result && result.rev) || rev + 1;
    current.rev = rev;
    current.content = content;
    current.mtime = Number(result && result.mtime) || Date.now();
    current.title = (result && result.title) || titleOf(content, current.path);
    current.tags = result && Array.isArray(result.tags) ? result.tags : tagsOf(content);
    current.updated = new Date(current.mtime).toISOString();
    dirty = false;
    setSaveState('saved', false);
    setState({ current }, ['note-meta']);
    updateHeader();
    backlinksDebounced();
    if (manual) toast.success('已保存');
    return true;
  } catch (err) {
    if (!current || current.path !== notePath) return false;
    if (err && err.code === 2004) {
      const recovered = await handleConflictDialog();
      return recovered;
    }
    console.error('[MoNote] 保存失败', err);
    setSaveState('error', true);
    toast.error(`保存失败：${ctx.api.friendlyError(err)}`, {
      title: '内容仍保留在编辑区',
    });
    if (retryTimer) clearTimeout(retryTimer);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (dirty && current && current.path === notePath) {
        console.info('[MoNote] 自动重试保存');
        saveNow({ auto: true }).catch(() => {});
      }
    }, AUTOSAVE_RETRY_MS);
    return false;
  } finally {
    markSaveIdle();
    if (saveQueued || dirty) {
      saveQueued = false;
      if (current && current.path === notePath && dirty) {
        setTimeout(() => {
          if (dirty && !saving && current && current.path === notePath) saveNow({ auto: true }).catch(() => {});
        }, 60);
      }
    }
    setState({ dirty });
  }
}

function handleConflictDialog() {
  return new Promise((resolve) => {
    let settled = false;
    const finish = async (choice) => {
      if (settled) return;
      settled = true;
      modalHandle.close();
      if (choice === 'cancel') {
        setSaveState('unsaved', true);
        resolve(false);
        return;
      }
      if (choice === 'reload') {
        try {
          const note = await ctx.api.call('vault_read', { path: current.path });
          setNote(note, { preserveScroll: true });
          toast.info('已重新加载磁盘上的版本');
          resolve(true);
        } catch (err) {
          toast.error(`重新加载失败：${ctx.api.friendlyError(err)}`);
          setSaveState('error', true);
          resolve(false);
        }
        return;
      }
      // overwrite：不带 expectRev 强制写入
      try {
        const content = textarea.value;
        const result = await ctx.api.call('vault_write', { path: current.path, content });
        if (current) {
          rev = Number(result && result.rev) || rev + 1;
          current.rev = rev;
          current.content = content;
          current.title = (result && result.title) || current.title;
          current.tags = result && Array.isArray(result.tags) ? result.tags : current.tags;
          current.mtime = (result && result.mtime) || Date.now();
          current.updated = new Date(current.mtime).toISOString();
        }
        lastSavedContent = content;
        dirty = false;
        setSaveState('saved', false);
        updateHeader();
        toast.success('已覆盖保存（外部修改已被替换）');
        resolve(true);
      } catch (err) {
        toast.error(`覆盖保存失败：${ctx.api.friendlyError(err)}`);
        setSaveState('error', true);
        resolve(false);
      }
    };
    const reloadBtn = button('重新加载磁盘版本', { onClick: () => finish('reload') });
    const overwriteBtn = button('保留我的并覆盖', { variant: 'danger', onClick: () => finish('overwrite') });
    const cancelBtn = button('取消', { onClick: () => finish('cancel') });
    const contentNode = el('div', { className: 'confirm-content' }, [
      el('p', { className: 'confirm-message', text: '当前笔记在磁盘上已被外部修改（可能由其他编辑器或同步操作产生）。请选择处理方式：' }),
      el('ul', { className: 'confirm-detail' }, [
        el('li', { text: '重新加载磁盘版本：放弃当前未保存的修改；' }),
        el('li', { text: '保留我的并覆盖：用当前编辑内容覆盖磁盘版本；' }),
        el('li', { text: '取消：暂不处理，修改仍保留在编辑区。' }),
      ]),
    ]);
    const modalHandle = openModal({
      title: '保存冲突',
      subtitle: '修订号不一致（错误码 2004）',
      content: contentNode,
      footer: [cancelBtn, reloadBtn, overwriteBtn],
      size: 'sm',
      initialFocus: cancelBtn,
      onClose: () => {
        if (!settled) {
          settled = true;
          setSaveState('unsaved', true);
          resolve(false);
        }
      },
    });
  });
}

/** flush：等待进行中的保存结束，必要时再保存一次。 */
export async function flush() {
  if (!current || !textarea) return true;
  await waitSaveIdle(4000);
  if (saving) return false;
  if (dirty || textarea.value !== lastSavedContent) {
    return saveNow({ manual: false });
  }
  return true;
}

// ---------------------------------------------------------------------------
// 窗口失焦 / 退出前保存
// ---------------------------------------------------------------------------
function bindWindowFlush() {
  window.addEventListener('blur', () => {
    if (dirty && current) {
      saveNow({ auto: true }).catch(() => {});
    }
  });
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden' && dirty && current) {
      saveNow({ auto: true }).catch(() => {});
    }
  });
  window.addEventListener('beforeunload', () => {
    if (dirty && current) {
      // 尽力而为：Tauri 侧退出时由后端 before-quit 兜底；此处触发一次即时保存
      saveNow({ auto: true }).catch(() => {});
    }
  });
}

// ---------------------------------------------------------------------------
// 笔记装载 / 清空 / 外部变更
// ---------------------------------------------------------------------------
/**
 * 载入一篇笔记到编辑器。
 * @param {{path:string,title:string,content:string,rev:number,mtime:number,tags?:string[],created?:string,updated?:string}} note
 * @param {{focus?:boolean, preserveScroll?:boolean, line?:number}} [opts]
 */
export function setNote(note, opts = {}) {
  if (!textarea || !note) return;
  current = {
    path: note.path,
    title: note.title || titleOf(note.content, note.path),
    content: note.content,
    rev: Number(note.rev) || 0,
    mtime: Number(note.mtime) || Date.now(),
    tags: Array.isArray(note.tags) ? note.tags : tagsOf(note.content),
    created: note.created,
    updated: note.updated,
  };
  lastSavedContent = note.content;
  rev = current.rev;
  dirty = false;
  saveQueued = false;
  if (autosaveTimer) {
    clearTimeout(autosaveTimer);
    autosaveTimer = null;
  }
  textarea.value = note.content;
  textarea.disabled = false;
  document.body.classList.remove('no-note');
  const empty = qs('#editor-empty');
  if (empty) empty.hidden = true;
  setCurrentNote(note);
  setSaveState('saved', false);
  closeSuggest();
  closeProperties();
  updateHeader();
  updateWords();
  ctx.preview.render(note.content, { path: current.path, title: current.title, preserveScroll: false });
  backlinksOpen = !backlinksPanel.hidden;
  refreshBacklinks();
  if (opts.focus) {
    textarea.focus();
    try {
      textarea.setSelectionRange(0, 0);
    } catch {
      /* ignore */
    }
    textarea.scrollTop = 0;
    if (opts.line != null) revealLine(opts.line);
  }
}

function setCurrentNote(note) {
  state.current = note;
  state.dirty = false;
  state.saveState = 'saved';
  emit('note', note);
  emit('save-state', { saveState: 'saved', dirty: false });
}

function updateHeader() {
  if (!current) return;
  const titleEl = qs('#note-title');
  if (titleEl) titleEl.textContent = current.title || '未命名笔记';
  const pathEl = qs('#note-path');
  if (pathEl) {
    const vaultName = state.vault && state.vault.name ? state.vault.name : '';
    pathEl.textContent = vaultName ? `${vaultName} / ${current.path}` : current.path;
    pathEl.title = current.path;
  }
  const docEl = qs('#status-doc');
  if (docEl) docEl.textContent = current.title || current.path;
  const vaultEl = document.getElementById('vault-chip');
  if (vaultEl && state.vault) {
    vaultEl.textContent = state.vault.name || '';
    vaultEl.title = state.vault.root || '';
  }
  const wordsEl = qs('#status-words');
  if (wordsEl && current) {
    const stats = textStats(splitFrontMatter(textarea.value).body);
    wordsEl.textContent = `${stats.words} 字`;
  }
}

/** 清空编辑器（未打开笔记状态）。 */
export function clearNote() {
  current = null;
  rev = 0;
  dirty = false;
  lastSavedContent = '';
  if (autosaveTimer) {
    clearTimeout(autosaveTimer);
    autosaveTimer = null;
  }
  if (saveIdleResolvers.length) markSaveIdle();
  if (textarea) {
    textarea.value = '';
    textarea.disabled = true;
    textarea.blur();
  }
  setState({ current: null, dirty: false }, 'note');
  setSaveState('saved', false);
  document.body.classList.add('no-note');
  const empty = qs('#editor-empty');
  if (empty) empty.hidden = false;
  const titleEl = qs('#note-title');
  if (titleEl) titleEl.textContent = '未打开笔记';
  const pathEl = qs('#note-path');
  if (pathEl) pathEl.textContent = '';
  const docEl = qs('#status-doc');
  if (docEl) docEl.textContent = '未打开笔记';
  const wordsEl = qs('#status-words');
  if (wordsEl) wordsEl.textContent = '0 字';
  const count = qs('#backlinks-count');
  if (count) count.textContent = '';
  if (backlinksList) backlinksList.innerHTML = '<p class="backlinks-empty">打开笔记后显示反向链接。</p>';
  if (backlinksPanel) backlinksPanel.hidden = true;
  const toggle = qs('#btn-backlinks');
  if (toggle) toggle.setAttribute('aria-expanded', 'false');
  closeProperties();
  closeSuggest();
  ctx.preview.clearPreview();
  ctx.preview.setView('preview');
  document.title = 'MoNote 墨记';
}

/** 将焦点移到编辑器指定行并选中该行（反链跳转高亮）。 */
export function revealLine(line) {
  if (!textarea || line == null || line < 0) return;
  const lines = textarea.value.split('\n');
  const index = clamp(line, 0, Math.max(0, lines.length - 1));
  let start = 0;
  for (let i = 0; i < index; i += 1) start += lines[i].length + 1;
  const end = Math.min(textarea.value.length, start + (lines[index] || '').length);
  textarea.focus();
  try {
    textarea.setSelectionRange(start, end);
  } catch {
    /* ignore */
  }
  const style = window.getComputedStyle(textarea);
  const lineHeight = parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.75;
  textarea.scrollTop = Math.max(0, index * lineHeight - textarea.clientHeight / 2);
}

function insertAtCursor(text) {
  if (!textarea || !current) {
    toast.info('请先打开一篇笔记');
    return;
  }
  const start = textarea.selectionStart;
  const end = textarea.selectionEnd;
  replaceRange(start, end, text, start + text.length, start + text.length);
  textarea.focus();
}

/** 在光标处插入 Markdown 文本（供其它模块/引导使用）。 */
export function insertText(text) {
  insertAtCursor(text);
}

// ---------------------------------------------------------------------------
// 外部变更
// ---------------------------------------------------------------------------
/**
 * 处理 notes:external 事件。
 * @param {{path:string, kind:'created'|'modified'|'deleted'|'renamed', oldPath?:string}} payload
 */
export async function handleExternalChange(payload) {
  if (!current || !payload) return;
  const { path, kind, oldPath } = payload;
  if (kind === 'renamed') {
    if (oldPath && oldPath === current.path && path) {
      current.path = path;
      updateHeader();
      refreshBacklinks();
    }
    return;
  }
  if (path !== current.path) return;
  if (kind === 'deleted') {
    if (dirty) {
      toast.warn('当前笔记在磁盘上被删除，但编辑区仍有未保存内容；保存将重新创建文件。');
    } else {
      toast.warn('当前笔记在磁盘上被删除，已关闭该笔记。');
      clearNote();
    }
    return;
  }
  if (kind === 'modified' || kind === 'created') {
    if (dirty) {
      toast.warn('当前笔记在外部被修改；继续编辑时保存会提示冲突处理。');
      return;
    }
    try {
      const note = await ctx.api.call('vault_read', { path });
      setNote(note, { preserveScroll: true });
      toast.info('已重新加载外部修改');
    } catch (err) {
      console.warn('[MoNote] 外部变更重新加载失败', err);
    }
  }
}
