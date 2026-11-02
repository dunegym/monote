/**
 * Markdown 预览渲染：
 * - marked（UMD，window.marked）+ KaTeX（window.katex）+ highlight.js（window.hljs），全部本地 vendor
 * - 双链 [[目标]] / [[目标|别名]] / [[#标题]]（缺失样式 + 点击创建）
 * - GFM 表格 / 任务列表 / 删除线 / 围栏代码高亮
 * - $行内公式$ 与 $$块级公式$$，语法错误红色显示
 * - 相对图片经 vault_image_data 转 data URL 显示
 * - 大纲（H1–H6）
 * - 导出用 HTML（内嵌图片 + KaTeX CSS 链接）供前端打印/HTML 导出
 */
import { ctx } from './ctx.js';
import { state } from './state.js';
import { splitFrontMatter, escapeHtml, dirname, joinPath, normalizePath, basename } from './util.js';

let container = null;
let outlineEl = null;
let previewStatusEl = null;
let currentPath = '';
let currentTitle = '';
let currentBody = '';
let imageCache = new Map();
let renderToken = 0;
let outlineItems = [];
let viewMode = 'preview';
let bound = false;

// ---------------------------------------------------------------------------
// marked 扩展：数学公式 + 双链
// ---------------------------------------------------------------------------
let markedConfigured = false;

function renderMath(tex, displayMode) {
  const katex = typeof window !== 'undefined' ? window.katex : null;
  if (!katex || typeof katex.renderToString !== 'function') {
    return `<span class="math-error" title="KaTeX 未加载">${escapeHtml(tex)}</span>`;
  }
  try {
    const html = katex.renderToString(tex, {
      displayMode,
      throwOnError: true,
      strict: 'ignore',
      trust: false,
      output: 'htmlAndMathml',
    });
    return html;
  } catch (err) {
    const msg = (err && err.message) || '语法错误';
    return `<span class="math-error" title="${escapeHtml(msg)}">${escapeHtml(tex)}</span>`;
  }
}

function findNoteByTitle(title) {
  if (!title) return null;
  return state.noteNames.get(String(title).trim().toLowerCase()) || null;
}

function isMissing(target) {
  return !findNoteByTitle(target);
}

function configureMarked() {
  if (markedConfigured) return;
  const marked = typeof window !== 'undefined' ? window.marked : null;
  if (!marked || typeof marked.use !== 'function') return;
  markedConfigured = true;

  const mathBlock = {
    name: 'mathBlock',
    level: 'block',
    start(src) {
      const idx = src.indexOf('$$');
      return idx >= 0 ? idx : undefined;
    },
    tokenizer(src) {
      const m = /^\$\$[ \t]*\r?\n?([\s\S]+?)\r?\n?[ \t]*\$\$(?:\r?\n|$)/.exec(src);
      if (!m) return undefined;
      return { type: 'mathBlock', raw: m[0], text: m[1].trim() };
    },
    renderer(token) {
      return `<div class="math-block">${renderMath(token.text, true)}</div>`;
    },
  };

  const mathInline = {
    name: 'mathInline',
    level: 'inline',
    start(src) {
      const idx = src.indexOf('$');
      return idx >= 0 ? idx : undefined;
    },
    tokenizer(src) {
      const m = /^\$([^$\n]+?)\$/.exec(src);
      if (!m) return undefined;
      const text = m[1];
      // 过滤货币金额等误判：$5,000 / $ 前后有空白
      if (/^\s|\s$/.test(text)) return undefined;
      if (/^[\d.,\s]+$/.test(text)) return undefined;
      return { type: 'mathInline', raw: m[0], text };
    },
    renderer(token) {
      return renderMath(token.text, false);
    },
  };

  const wikilink = {
    name: 'wikilink',
    level: 'inline',
    start(src) {
      const idx = src.indexOf('[[');
      return idx >= 0 ? idx : undefined;
    },
    tokenizer(src) {
      const m = /^\[\[([^\]\n]+?)\]\]/.exec(src);
      if (!m) return undefined;
      const inner = m[1];
      const pipe = inner.indexOf('|');
      const target = (pipe >= 0 ? inner.slice(0, pipe) : inner).trim();
      const alias = pipe >= 0 ? inner.slice(pipe + 1).trim() : '';
      if (!target) return undefined;
      return { type: 'wikilink', raw: m[0], target, alias };
    },
    renderer(token) {
      const target = token.target;
      const alias = token.alias;
      if (target.startsWith('#')) {
        const heading = target.slice(1).trim();
        return `<a class="md-anchor" href="#" data-anchor="${escapeHtml(heading)}" title="跳转到本笔记标题">${escapeHtml(alias || heading)}</a>`;
      }
      const display = alias || target;
      if (isMissing(target)) {
        return `<a class="wikilink wikilink-missing" href="#" data-wikilink="${escapeHtml(target)}" title="目标笔记不存在，点击创建">${escapeHtml(display)}</a>`;
      }
      return `<a class="wikilink" href="#" data-wikilink="${escapeHtml(target)}" title="打开笔记 ${escapeHtml(target)}">${escapeHtml(display)}</a>`;
    },
  };

  marked.use({
    gfm: true,
    breaks: false,
    extensions: [mathBlock, mathInline, wikilink],
  });
}

// ---------------------------------------------------------------------------
// HTML 清洗（本地内容最小安全化，避免渲染脚本/事件属性）
// ---------------------------------------------------------------------------
const DANGEROUS_TAGS = new Set(['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT', 'EMBED', 'LINK', 'META', 'BASE', 'FORM']);

function sanitize(root) {
  for (const node of [...root.querySelectorAll('*')]) {
    if (DANGEROUS_TAGS.has(node.tagName)) {
      node.remove();
      continue;
    }
    for (const attr of [...node.attributes]) {
      const name = attr.name.toLowerCase();
      if (name.startsWith('on')) {
        node.removeAttribute(attr.name);
        continue;
      }
      const value = attr.value || '';
      if (name === 'href' || name === 'xlink:href' || name === 'src') {
        const v = value.trim();
        if (/^javascript:/i.test(v) || /^vbscript:/i.test(v)) node.removeAttribute(attr.name);
        continue;
      }
      if (name === 'style') {
        const inKatex = node.closest && node.closest('.katex, .katex-display');
        if (!inKatex) node.removeAttribute('style');
        continue;
      }
    }
    if (node.tagName === 'INPUT') {
      if (node.type !== 'checkbox') node.remove();
      else {
        node.disabled = true;
        node.setAttribute('disabled', '');
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 代码高亮 / 标题锚点 / 图片解析 / 相对链接
// ---------------------------------------------------------------------------
function highlightCode(root) {
  const hljs = typeof window !== 'undefined' ? window.hljs : null;
  if (!hljs || typeof hljs.highlightElement !== 'function') return;
  for (const code of root.querySelectorAll('pre code')) {
    try {
      code.removeAttribute('data-highlighted');
      if (Array.from(code.classList).some((c) => c.startsWith('language-'))) {
        hljs.highlightElement(code);
      } else if (code.textContent && code.textContent.trim().length < 20000) {
        hljs.highlightElement(code);
      }
    } catch (err) {
      // 高亮失败保持纯文本，不影响预览
      console.warn('[MoNote] 代码高亮失败', err);
    }
  }
}

function markTaskLists(root) {
  for (const input of root.querySelectorAll('input[type="checkbox"]')) {
    const li = input.closest('li');
    if (li) li.classList.add('task-list-item');
  }
}

function collectHeadings(root) {
  const items = [];
  const headings = root.querySelectorAll('h1, h2, h3, h4, h5, h6');
  headings.forEach((heading, index) => {
    const id = `preview-heading-${index}`;
    heading.id = id;
    heading.classList.add('preview-heading');
    const level = Number(heading.tagName.slice(1));
    const text = heading.textContent.replace(/\s+/g, ' ').trim();
    // 尽力估算源码行号（供大纲点击时同步编辑器滚动）
    let line = -1;
    const bodyLines = currentBody.split(/\r\n|\r|\n/);
    for (let i = 0; i < bodyLines.length; i += 1) {
      const m = /^(\s{0,3})(#{1,6})\s+(.*)$/.exec(bodyLines[i]);
      if (m && m[3].trim().replace(/\s+#+\s*$/, '') === text) {
        line = i;
        break;
      }
    }
    items.push({ id, level, text, line });
  });
  return items;
}

function renderOutline() {
  if (!outlineEl) return;
  outlineEl.innerHTML = '';
  if (!outlineItems.length) {
    outlineEl.appendChild(Object.assign(document.createElement('p'), { className: 'outline-empty', textContent: '当前笔记没有 H1–H6 标题' }));
    return;
  }
  for (const item of outlineItems) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `outline-item level-${item.level}`;
    btn.textContent = item.text || '（空标题）';
    btn.title = item.text;
    btn.addEventListener('click', () => {
      scrollToHeading(item.id);
      if (ctx.editor && item.line >= 0) ctx.editor.revealLine(item.line);
    });
    outlineEl.appendChild(btn);
  }
}

/** 滚动预览到指定标题。 */
export function scrollToHeading(id) {
  const heading = container ? container.querySelector(`#${CSS.escape(String(id))}`) : null;
  if (heading) {
    heading.scrollIntoView({ behavior: 'smooth', block: 'start' });
    heading.classList.add('heading-flash');
    setTimeout(() => heading.classList.remove('heading-flash'), 1200);
    return true;
  }
  return false;
}

/** 按标题文字滚动（用于 [[#标题]]）。 */
export function scrollToHeadingText(text) {
  if (!container || !text) return false;
  const headings = [...container.querySelectorAll('h1, h2, h3, h4, h5, h6')];
  const target = text.trim();
  const found = headings.find((h) => h.textContent.replace(/\s+/g, ' ').trim() === target)
    || headings.find((h) => h.textContent.replace(/\s+/g, ' ').trim().includes(target));
  if (found) {
    found.scrollIntoView({ behavior: 'smooth', block: 'start' });
    found.classList.add('heading-flash');
    setTimeout(() => found.classList.remove('heading-flash'), 1200);
    return true;
  }
  return false;
}

function resolveRelativePath(src) {
  let raw = String(src || '').split('#')[0].split('?')[0];
  if (!raw) return '';
  raw = raw.replace(/^\.\//, '');
  if (raw.startsWith('/')) return normalizePath(raw.slice(1));
  if (/^[a-zA-Z]+:/.test(raw) || raw.startsWith('data:')) return '';
  return normalizePath(joinPath(dirname(currentPath), raw));
}

const TRANSPARENT_IMG = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

async function resolveImages(root, token) {
  const images = [...root.querySelectorAll('img')];
  await Promise.all(images.map(async (img) => {
    const src = img.dataset.pendingSrc || img.getAttribute('src') || '';
    if (!src || src.startsWith('data:') || src.startsWith('http:') || src.startsWith('https:') || src.startsWith('blob:')) return;
    const path = resolveRelativePath(src);
    if (!path) return;
    try {
      let dataUrl = imageCache.get(path);
      if (!dataUrl) {
        const res = await ctx.api.call('vault_image_data', { path });
        dataUrl = res && res.dataUrl;
        if (dataUrl) imageCache.set(path, dataUrl);
      }
      if (token !== renderToken) return;
      if (dataUrl) {
        img.src = dataUrl;
        img.setAttribute('data-src', path);
        delete img.dataset.pendingSrc;
      } else {
        throw new Error('no data');
      }
    } catch (err) {
      if (token !== renderToken) return;
      img.classList.add('img-error');
      const alt = img.getAttribute('alt') || basename(path);
      img.alt = `${alt}（图片加载失败：${path}）`;
      img.title = `图片加载失败：${path}`;
      img.removeAttribute('src');
      delete img.dataset.pendingSrc;
    }
  }));
}

/** 相对图片先替换占位图，避免浏览器对库内相对路径发起无效请求 / 显示裂图。 */
function prepareImages(root) {
  for (const img of root.querySelectorAll('img')) {
    const src = img.getAttribute('src') || '';
    if (!src || src.startsWith('data:') || /^[a-zA-Z]+:/.test(src)) continue;
    img.dataset.pendingSrc = src;
    img.src = TRANSPARENT_IMG;
    img.classList.add('img-loading');
  }
}

function enhanceLinks(root) {
  for (const a of root.querySelectorAll('a[href]')) {
    const href = (a.getAttribute('href') || '').trim();
    if (!href || href.startsWith('#')) continue;
    if (/^https?:/i.test(href) || /^mailto:/i.test(href)) {
      a.setAttribute('target', '_blank');
      a.setAttribute('rel', 'noopener noreferrer');
      a.dataset.external = '1';
      continue;
    }
    if (/\.(md|markdown)$/i.test(href)) {
      const path = resolveRelativePath(href);
      if (path) {
        a.dataset.notePath = path;
        a.classList.add('wikilink');
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 渲染主流程
// ---------------------------------------------------------------------------
function renderEmpty(message) {
  if (!container) return;
  container.innerHTML = `<p class="preview-empty">${escapeHtml(message)}</p>`;
  outlineItems = [];
  renderOutline();
}

/**
 * 渲染 Markdown 到预览区。
 * @param {string} content 笔记原始内容（含 front-matter）
 * @param {{path?:string, title?:string, preserveScroll?:boolean}} [opts]
 */
export function render(content, opts = {}) {
  if (!container) return;
  const prevScrollTop = container.scrollTop;
  const prevScrollRatio = container.scrollHeight > container.clientHeight
    ? container.scrollTop / (container.scrollHeight - container.clientHeight)
    : 0;
  currentPath = opts.path != null ? opts.path : currentPath;
  currentTitle = opts.title != null ? opts.title : currentTitle;
  const fm = splitFrontMatter(String(content || ''));
  currentBody = fm.body;
  const token = (renderToken += 1);
  const started = performance.now();

  if (!currentBody.trim()) {
    renderEmpty('这篇笔记还是空的，在左侧编辑区开始书写吧。');
    if (previewStatusEl) previewStatusEl.textContent = '';
    return;
  }

  configureMarked();
  const marked = typeof window !== 'undefined' ? window.marked : null;
  let html = '';
  if (!marked || typeof marked.parse !== 'function') {
    html = `<pre>${escapeHtml(currentBody)}</pre><p class="preview-empty">marked 未加载，已降级为纯文本预览。</p>`;
    container.innerHTML = html;
    outlineItems = [];
    renderOutline();
    return;
  }
  try {
    html = marked.parse(currentBody, { async: false });
    if (html && typeof html.then === 'function') html = '';
    html = String(html || '');
  } catch (err) {
    console.error('[MoNote] Markdown 渲染失败', err);
    container.innerHTML = `<p class="preview-empty">渲染失败：${escapeHtml(err && err.message ? err.message : String(err))}</p><pre>${escapeHtml(currentBody)}</pre>`;
    outlineItems = [];
    renderOutline();
    return;
  }

  container.innerHTML = html;
  sanitize(container);
  prepareImages(container);

  // marked 对非法/嵌套语法可能抛错；上面已 catch。再兜底：移除残留的原始 HTML 注释节点异常
  try {
    markTaskLists(container);
    highlightCode(container);
    outlineItems = collectHeadings(container);
    enhanceLinks(container);
    renderOutline();
  } catch (err) {
    console.warn('[MoNote] 预览后处理失败', err);
  }

  // 图片异步解析（不阻塞渲染）
  resolveImages(container, token).catch((err) => console.warn('[MoNote] 图片解析失败', err));

  if (opts.preserveScroll !== false) {
    requestAnimationFrame(() => {
      if (!container) return;
      const max = container.scrollHeight - container.clientHeight;
      if (max > 0) container.scrollTop = Math.round(prevScrollRatio * max);
      else container.scrollTop = prevScrollTop;
    });
  }
  if (previewStatusEl) {
    const ms = Math.round(performance.now() - started);
    previewStatusEl.textContent = `渲染 ${ms} ms`;
    setTimeout(() => {
      if (previewStatusEl) previewStatusEl.textContent = '';
    }, 2000);
  }
}

/** 清空预览。 */
export function clearPreview() {
  currentPath = '';
  currentTitle = '';
  currentBody = '';
  renderToken += 1;
  if (container) container.innerHTML = '<p class="preview-empty">打开一篇笔记后在此查看渲染效果。</p>';
  outlineItems = [];
  renderOutline();
}

/** 当前大纲项。 */
export function getOutline() {
  return [...outlineItems];
}

/** 切换预览/大纲视图。 */
export function setView(mode) {
  viewMode = mode === 'outline' ? 'outline' : 'preview';
  const previewTab = document.getElementById('ptab-preview');
  const outlineTab = document.getElementById('ptab-outline');
  if (container) container.hidden = viewMode !== 'preview';
  if (outlineEl) outlineEl.hidden = viewMode !== 'outline';
  if (previewTab) {
    const active = viewMode === 'preview';
    previewTab.classList.toggle('active', active);
    previewTab.setAttribute('aria-selected', String(active));
    previewTab.tabIndex = active ? 0 : -1;
  }
  if (outlineTab) {
    const active = viewMode === 'outline';
    outlineTab.classList.toggle('active', active);
    outlineTab.setAttribute('aria-selected', String(active));
    outlineTab.tabIndex = active ? 0 : -1;
  }
}

// ---------------------------------------------------------------------------
// 点击交互
// ---------------------------------------------------------------------------
async function handlePreviewClick(event) {
  const missing = event.target.closest('.wikilink-missing');
  if (missing) {
    event.preventDefault();
    const target = missing.dataset.wikilink;
    try {
      const confirmed = await ctx.modal.confirmDialog({
        title: '目标笔记不存在',
        message: `笔记「${target}」不存在，是否创建同名笔记并打开？`,
        confirmLabel: '创建并打开',
        cancelLabel: '取消',
        danger: false,
      });
      if (!confirmed) return;
      const res = await ctx.api.call('vault_create_empty', { name: target });
      await ctx.actions.refreshAll();
      if (res && res.path) await ctx.actions.openNote(res.path);
      ctx.toast.success(`已创建笔记「${target}」`);
    } catch (err) {
      ctx.toast.error(ctx.api.friendlyError(err));
    }
    return;
  }

  const wikilink = event.target.closest('.wikilink[data-wikilink]');
  if (wikilink) {
    event.preventDefault();
    const target = wikilink.dataset.wikilink;
    const note = findNoteByTitle(target);
    if (note) await ctx.actions.openNote(note.path);
    else ctx.toast.warn(`未找到笔记「${target}」`);
    return;
  }

  const anchor = event.target.closest('.md-anchor');
  if (anchor) {
    event.preventDefault();
    const heading = anchor.dataset.anchor || '';
    if (!scrollToHeadingText(heading)) ctx.toast.warn(`本笔记中未找到标题「${heading}」`);
    return;
  }

  const noteLink = event.target.closest('a[data-note-path]');
  if (noteLink) {
    event.preventDefault();
    const path = noteLink.dataset.notePath;
    try {
      await ctx.api.call('vault_read', { path });
      await ctx.actions.openNote(path);
    } catch (err) {
      ctx.toast.error(`链接目标打开失败：${ctx.api.friendlyError(err)}`);
    }
    return;
  }

  const external = event.target.closest('a[data-external]');
  if (external) {
    event.preventDefault();
    const url = external.getAttribute('href');
    try {
      const win = window.open(url, '_blank', 'noopener,noreferrer');
      if (!win) throw new Error('blocked');
    } catch {
      try {
        await navigator.clipboard.writeText(url);
        ctx.toast.info('已复制外部链接到剪贴板（桌面应用中请用系统浏览器打开）');
      } catch {
        ctx.toast.info(`外部链接：${url}`);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 导出 HTML（供 HTML 导出与 PDF 打印）
// ---------------------------------------------------------------------------
const EXPORT_CSS = `
:root { color-scheme: light; }
body { margin: 0; padding: 2.2rem 2.6rem 3rem; font-family: -apple-system, "Segoe UI", "Microsoft YaHei", "PingFang SC", sans-serif; font-size: 15px; line-height: 1.75; color: #1b2330; background: #fff; }
h1,h2,h3,h4,h5,h6 { line-height: 1.3; margin: 1.4em 0 .55em; font-weight: 600; }
h1 { font-size: 1.65em; border-bottom: 1px solid #e3e6ea; padding-bottom: .25em; }
h2 { font-size: 1.35em; border-bottom: 1px solid #e3e6ea; padding-bottom: .2em; }
h3 { font-size: 1.15em; }
p { margin: .7em 0; }
a { color: #1d5fd0; text-decoration: underline; }
blockquote { margin: .9em 0; padding: .35em 1em; color: #5b6472; border-left: 3px solid #c6cdd7; background: #f8f9fb; }
code { font-family: "Cascadia Code", Consolas, monospace; font-size: .88em; background: #f1f3f6; padding: .14em .36em; border-radius: 4px; }
pre { padding: .85em 1em; overflow: auto; background: #f6f8fa; border: 1px solid #e3e6ea; border-radius: 7px; line-height: 1.6; }
pre code { padding: 0; background: transparent; }
table { border-collapse: collapse; max-width: 100%; display: block; overflow-x: auto; font-size: .92em; }
th, td { border: 1px solid #c6cdd7; padding: .35em .8em; text-align: left; }
th { background: #f8f9fb; }
img { max-width: 100%; height: auto; border-radius: 6px; }
hr { border: none; border-top: 1px solid #e3e6ea; margin: 1.6em 0; }
.task-list-item { list-style: none; margin-left: -1.4em; }
.katex-display { margin: .9em 0; overflow-x: auto; overflow-y: hidden; }
.math-error { color: #c62828; background: #fdecec; border: 1px solid #c62828; border-radius: 4px; padding: .05em .4em; font-family: monospace; font-size: .85em; }
.wikilink { color: #1d5fd0; text-decoration: underline dotted; }
.wikilink-missing { color: #c62828; }
@media print { body { padding: 0; } .math-error { print-color-adjust: exact; } }
`;

/**
 * 生成自包含 HTML（图片内嵌为 data URL），用于 HTML 导出 / PDF 打印。
 * KaTeX 样式内联；字体 URL 改写为应用安装目录的绝对地址（同机打开时可用；
 * 图片已全部内嵌，满足 FR-402/AC-017 的离线要求）。
 * @param {{content:string, title:string, path:string}} opts
 * @returns {Promise<string>}
 */
export async function buildExportHtml(opts = {}) {
  const fm = splitFrontMatter(String(opts.content || ''));
  const prevPath = currentPath;
  currentPath = opts.path || '';
  configureMarked();
  const marked = typeof window !== 'undefined' ? window.marked : null;
  let bodyHtml = '';
  if (marked && typeof marked.parse === 'function') {
    try {
      bodyHtml = String(marked.parse(fm.body, { async: false }) || '');
    } catch (err) {
      bodyHtml = `<pre>${escapeHtml(fm.body)}</pre>`;
    }
  } else {
    bodyHtml = `<pre>${escapeHtml(fm.body)}</pre>`;
  }
  // 用临时容器做 sanitize + 图片内嵌 + 链接处理
  const tmp = document.createElement('div');
  tmp.innerHTML = bodyHtml;
  sanitize(tmp);
  for (const img of tmp.querySelectorAll('img')) {
    const src = img.dataset.pendingSrc || img.getAttribute('src') || '';
    if (!src || src.startsWith('data:') || /^https?:/i.test(src)) continue;
    const path = resolveRelativePath(src);
    if (!path) continue;
    try {
      let dataUrl = imageCache.get(path);
      if (!dataUrl) {
        const res = await ctx.api.call('vault_image_data', { path });
        dataUrl = res && res.dataUrl;
        if (dataUrl) imageCache.set(path, dataUrl);
      }
      if (dataUrl) img.src = dataUrl;
      else img.removeAttribute('src');
    } catch {
      img.removeAttribute('src');
    }
  }
  // 双链在导出中显示为普通文本 + 提示
  for (const a of tmp.querySelectorAll('.wikilink, .md-anchor')) {
    const span = document.createElement('span');
    span.textContent = a.textContent;
    span.style.color = '#1d5fd0';
    a.replaceWith(span);
  }
  const title = opts.title || currentTitle || 'MoNote 笔记';
  const katexCss = await loadKatexCssForExport();
  const fontBase = new URL('vendor/katex/fonts/', document.baseURI).href;
  const katexStyle = katexCss
    ? `<style>${katexCss.replace(/url\((?:'|")?fonts\//g, `url(${fontBase}`)}</style>`
    : `<link rel="stylesheet" href="${new URL('vendor/katex/katex.min.css', document.baseURI).href}">`;
  currentPath = prevPath;
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
${katexStyle}
<style>${EXPORT_CSS}</style>
</head>
<body>
${tmp.innerHTML}
</body>
</html>`;
}

let cachedKatexCss = null;

/** 读取 KaTeX CSS 文本以便导出时内联（离线可用；失败时降级为外链）。 */
async function loadKatexCssForExport() {
  if (cachedKatexCss != null) return cachedKatexCss;
  try {
    const url = new URL('vendor/katex/katex.min.css', document.baseURI).href;
    const res = await fetch(url);
    if (res.ok) {
      cachedKatexCss = await res.text();
      return cachedKatexCss;
    }
  } catch (err) {
    console.warn('[MoNote] 读取 KaTeX 样式失败，导出 HTML 将外链样式', err);
  }
  cachedKatexCss = '';
  return '';
}

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------
/**
 * @param {{onOpenLine?: (line:number)=>void}} [opts]
 */
export function init(opts = {}) {
  container = document.getElementById('preview');
  outlineEl = document.getElementById('outline');
  previewStatusEl = document.getElementById('preview-status');
  if (!container) {
    console.error('[MoNote] 缺少 #preview 容器');
    return { render, clearPreview, setView, getOutline, scrollToHeading, scrollToHeadingText, buildExportHtml, refreshOutline: renderOutline };
  }
  if (!bound) {
    container.addEventListener('click', (event) => {
      handlePreviewClick(event).catch((err) => {
        console.error('[MoNote] 预览点击处理失败', err);
        ctx.toast.error(ctx.api.friendlyError(err));
      });
    });
    const previewTab = document.getElementById('ptab-preview');
    const outlineTab = document.getElementById('ptab-outline');
    previewTab?.addEventListener('click', () => setView('preview'));
    outlineTab?.addEventListener('click', () => setView('outline'));
    const tabs = [previewTab, outlineTab].filter(Boolean);
    for (const tab of tabs) {
      tab.addEventListener('keydown', (event) => {
        if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
        event.preventDefault();
        const next = tab === previewTab ? outlineTab : previewTab;
        if (next) {
          setView(next === outlineTab ? 'outline' : 'preview');
          next.focus();
        }
      });
    }
    bound = true;
  }
  setView('preview');
  clearPreview();
  void opts;
  return { render, clearPreview, setView, getOutline, scrollToHeading, scrollToHeadingText, buildExportHtml, refreshOutline: renderOutline };
}
