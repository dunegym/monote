/**
 * 检索：
 * - Ctrl+P 标题快速打开（浮层，输入即查、方向键 + 回车打开、最近打开优先）
 * - Ctrl+Shift+F 全文检索（模态面板，支持文件夹/标签/时间过滤、相关度/时间排序、命中高亮、分页）
 *
 * 后端命令：search_query {kind, term, filters, sort, page, pageSize}（page 从 0 开始）。
 */
import { ctx } from './ctx.js';
import { state } from './state.js';
import { qs, el, clear, focusTrap, isVisible } from './dom.js';
import { confirmDialog } from './modal.js';
import { fuzzyScore, formatRelative, highlightChars, startOfDay, daysAgoStart, formatDate, basename } from './util.js';

let quickOverlay = null;
let quickTrap = null;
let quickTimer = null;
let quickToken = 0;
let quickItems = [];
let quickIndex = -1;
let quickQuery = '';
let quickReturnFocus = null;
let quickBusy = false;

let fullModal = null;
let fullState = null;
let fullSearchTimer = null;
let fullToken = 0;

// ---------------------------------------------------------------------------
// 公共 API
// ---------------------------------------------------------------------------
export function init() {
  return { openQuick, openFull, focusNextResult, closeAll };
}

/** 关闭所有检索浮层（Esc 由各浮层自行处理）。 */
export function closeAll() {
  closeQuick();
  closeFull();
}

// ---------------------------------------------------------------------------
// 标题快速打开
// ---------------------------------------------------------------------------
export function openQuick() {
  if (quickOverlay) {
    focusQuickInput();
    return;
  }
  quickReturnFocus = document.activeElement;
  const input = el('input', {
    type: 'text',
    className: 'palette-input',
    id: 'quick-open-input',
    'aria-label': '标题检索：输入笔记名',
    'aria-autocomplete': 'list',
    'aria-controls': 'quick-open-list',
    autocomplete: 'off',
    spellcheck: 'false',
    placeholder: '输入笔记标题快速打开…（↑↓ 选择，Enter 打开，Esc 关闭）',
  });
  const list = el('div', { className: 'palette-list', id: 'quick-open-list', role: 'listbox', 'aria-label': '匹配的笔记' });
  const panel = el('div', { className: 'palette-panel', tabindex: '-1' }, [
    el('div', { className: 'palette-input-row' }, [
      el('span', { className: 'palette-icon', 'aria-hidden': 'true', text: '⌕' }),
      input,
    ]),
    list,
    el('div', { className: 'palette-footer' }, [
      el('span', { text: '↑↓ 选择' }),
      el('span', { text: '↵ 打开' }),
      el('span', { text: 'Esc 关闭' }),
      el('span', { className: 'palette-hint-static', text: '最近打开的笔记优先' }),
    ]),
  ]);
  const overlay = el('div', { className: 'palette-overlay', role: 'presentation' }, [panel]);
  document.body.appendChild(overlay);
  requestAnimationFrame(() => overlay.classList.add('visible'));
  quickOverlay = overlay;
  quickTrap = focusTrap(panel, { initialFocus: input });
  bindQuickEvents(input, list);
  input.addEventListener('input', () => scheduleQuickSearch(input.value));
  input.addEventListener('keydown', (event) => onQuickKeydown(event, input));
  overlay.addEventListener('mousedown', (event) => {
    if (event.target === overlay) closeQuick();
  });
  quickQuery = '';
  quickIndex = -1;
  quickItems = [];
  loadRecentQuickItems();
  focusQuickInput();
}

function focusQuickInput() {
  const input = document.getElementById('quick-open-input');
  if (input) {
    input.focus();
    input.select();
  }
}

function closeQuick() {
  if (!quickOverlay) return;
  if (quickTimer) {
    clearTimeout(quickTimer);
    quickTimer = null;
  }
  quickToken += 1;
  quickOverlay.remove();
  quickOverlay = null;
  if (quickTrap) {
    quickTrap.release();
    quickTrap = null;
  }
  quickItems = [];
  quickIndex = -1;
  if (quickReturnFocus && typeof quickReturnFocus.focus === 'function') {
    try {
      quickReturnFocus.focus();
    } catch {
      /* ignore */
    }
  }
  quickReturnFocus = null;
}

function scheduleQuickSearch(term) {
  quickQuery = term;
  if (quickTimer) clearTimeout(quickTimer);
  quickTimer = setTimeout(() => {
    quickTimer = null;
    runQuickSearch(term);
  }, 120);
}

async function runQuickSearch(term) {
  const token = (quickToken += 1);
  const query = String(term || '').trim();
  quickBusy = true;
  if (!query) {
    loadRecentQuickItems();
    quickBusy = false;
    return;
  }
  try {
    const res = await ctx.api.call('search_query', {
      kind: 'title',
      term: query,
      sort: 'mtime',
      page: 0,
      pageSize: 20,
    });
    if (token !== quickToken) return;
    const items = (res && Array.isArray(res.items)) ? res.items : [];
    // 最近打开的笔记优先（后端按修改时间排序，这里叠加本机打开记录）
    const recentIndex = new Map((state.recent || []).map((p, i) => [p, i]));
    items.sort((a, b) => {
      const ra = recentIndex.has(a.path) ? recentIndex.get(a.path) : Number.MAX_SAFE_INTEGER;
      const rb = recentIndex.has(b.path) ? recentIndex.get(b.path) : Number.MAX_SAFE_INTEGER;
      if (ra !== rb) return ra - rb;
      return (b.mtime || 0) - (a.mtime || 0);
    });
    quickItems = items.map((item) => ({
      path: item.path,
      title: item.title || basename(item.path),
      sub: `${item.path} · ${formatRelative(item.mtime)}`,
      mtime: item.mtime,
    }));
    quickIndex = quickItems.length ? 0 : -1;
    renderQuickList();
    quickBusy = false;
  } catch (err) {
    if (token !== quickToken) return;
    quickBusy = false;
    const list = document.getElementById('quick-open-list');
    if (list) {
      clear(list);
      list.appendChild(el('p', { className: 'palette-empty', text: `检索失败：${ctx.api.friendlyError(err)}` }));
    }
  }
}

function loadRecentQuickItems() {
  const notes = allNotesFromTree();
  const byPath = new Map(notes.map((n) => [n.path, n]));
  const seen = new Set();
  const items = [];
  for (const path of state.recent || []) {
    const note = byPath.get(path);
    if (note && !seen.has(path)) {
      seen.add(path);
      items.push({ path, title: note.title, sub: `${path} · 最近打开`, mtime: note.mtime });
    }
    if (items.length >= 20) break;
  }
  if (items.length < 10) {
    const rest = notes
      .filter((n) => !seen.has(n.path))
      .sort((a, b) => (b.mtime || 0) - (a.mtime || 0))
      .slice(0, 20 - items.length)
      .map((n) => ({ path: n.path, title: n.title, sub: `${n.path} · ${formatRelative(n.mtime)}`, mtime: n.mtime }));
    items.push(...rest);
  }
  quickItems = items;
  quickIndex = items.length ? 0 : -1;
  renderQuickList();
}

function allNotesFromTree() {
  const out = [];
  const walk = (nodes) => {
    for (const node of nodes || []) {
      if (node.kind === 'folder') walk(node.children);
      else out.push({ path: node.path, title: node.title || node.name, mtime: node.mtime || 0 });
    }
  };
  walk(state.tree);
  return out;
}

function renderQuickList() {
  const list = document.getElementById('quick-open-list');
  const input = document.getElementById('quick-open-input');
  if (!list) return;
  clear(list);
  if (input) input.setAttribute('aria-expanded', 'true');
  if (!quickItems.length) {
    list.appendChild(el('p', {
      className: 'palette-empty',
      text: quickQuery ? `没有标题匹配「${quickQuery}」的笔记` : '笔记库中还没有笔记',
    }));
    return;
  }
  quickItems.forEach((item, index) => {
    const node = el('div', {
      className: `palette-item${index === quickIndex ? ' active' : ''}`,
      role: 'option',
      id: `quick-open-option-${index}`,
      'aria-selected': index === quickIndex ? 'true' : 'false',
      dataset: { index: String(index) },
    }, [
      el('div', { className: 'palette-item-main' }, [
        el('div', { className: 'palette-item-title', text: item.title }),
        el('div', { className: 'palette-item-sub', text: item.sub }),
      ]),
      index < 9 ? el('kbd', { className: 'palette-kbd', text: String(index + 1) }) : null,
    ]);
    node.addEventListener('mousedown', (event) => {
      event.preventDefault();
      openQuickItem(index);
    });
    node.addEventListener('mousemove', () => {
      if (quickIndex !== index) {
        quickIndex = index;
        updateQuickActive();
      }
    });
    list.appendChild(node);
  });
  if (input) {
    const activeId = quickIndex >= 0 ? `quick-open-option-${quickIndex}` : '';
    if (activeId) input.setAttribute('aria-activedescendant', activeId);
    else input.removeAttribute('aria-activedescendant');
  }
}

function updateQuickActive() {
  const list = document.getElementById('quick-open-list');
  const input = document.getElementById('quick-open-input');
  if (!list) return;
  const nodes = [...list.querySelectorAll('.palette-item')];
  nodes.forEach((node, index) => {
    const active = index === quickIndex;
    node.classList.toggle('active', active);
    node.setAttribute('aria-selected', String(active));
    if (active) node.scrollIntoView({ block: 'nearest' });
  });
  if (input) {
    if (quickIndex >= 0) input.setAttribute('aria-activedescendant', `quick-open-option-${quickIndex}`);
    else input.removeAttribute('aria-activedescendant');
  }
}

function onQuickKeydown(event, input) {
  if (event.isComposing) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    if (!quickItems.length) return;
    quickIndex = (quickIndex + 1) % quickItems.length;
    updateQuickActive();
    return;
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault();
    if (!quickItems.length) return;
    quickIndex = (quickIndex - 1 + quickItems.length) % quickItems.length;
    updateQuickActive();
    return;
  }
  if (event.key === 'Enter') {
    event.preventDefault();
    if (quickIndex >= 0 && quickItems[quickIndex]) openQuickItem(quickIndex);
    else if (input.value.trim()) runQuickSearch(input.value);
    return;
  }
  if (event.key === 'Escape') {
    event.preventDefault();
    closeQuick();
  }
}

function openQuickItem(index) {
  const item = quickItems[index];
  if (!item) return;
  closeQuick();
  ctx.actions.openNote(item.path, { focus: true }).catch((err) => {
    ctx.toast.error(`打开笔记失败：${ctx.api.friendlyError(err)}`);
  });
}

function bindQuickEvents(input, list) {
  list.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      closeQuick();
    }
  });
  void input;
}

// ---------------------------------------------------------------------------
// 全文检索
// ---------------------------------------------------------------------------
/**
 * 打开全文检索面板。
 * @param {{term?:string, filters?:{folder?:string, tag?:string}}} [opts]
 */
export async function openFull(opts = {}) {
  if (fullModal) {
    closeFull();
  }
  const folderOptions = ctx.sidebar ? ctx.sidebar.getFolders() : [];
  const tags = await fetchTags();

  const input = el('input', {
    type: 'text',
    className: 'input',
    id: 'full-search-input',
    'aria-label': '全文检索关键词',
    placeholder: '关键词（空格分隔 AND）、"精确短语"、tag:标签名',
    autocomplete: 'off',
    spellcheck: 'false',
  });
  input.value = opts.term || '';

  const sortSelect = el('select', { className: 'input', id: 'search-sort', 'aria-label': '排序方式' }, [
    el('option', { value: 'relevance', text: '按相关度' }),
    el('option', { value: 'mtime', text: '按修改时间' }),
  ]);
  const searchBtn = el('button', { type: 'button', className: 'btn btn-primary', text: '检索' });
  const inputRow = el('div', { className: 'search-input-row' }, [input, searchBtn]);

  const folderSelect = el('select', { id: 'search-folder', 'aria-label': '按文件夹过滤' }, [
    el('option', { value: '', text: '全部文件夹' }),
    ...folderOptions.map((f) => el('option', { value: f.path, text: `${'　'.repeat(f.depth)}${f.name}` })),
  ]);
  const tagSelect = el('select', { id: 'search-tag', 'aria-label': '按标签过滤' }, [
    el('option', { value: '', text: '全部标签' }),
    ...tags.map((t) => el('option', { value: t.name, text: `#${t.name}（${t.count}）` })),
  ]);
  const timeSelect = el('select', { id: 'search-time', 'aria-label': '按修改时间过滤' }, [
    el('option', { value: 'all', text: '全部时间' }),
    el('option', { value: 'today', text: '今天' }),
    el('option', { value: '7d', text: '最近 7 天' }),
    el('option', { value: '30d', text: '最近 30 天' }),
    el('option', { value: 'custom', text: '自定义…' }),
  ]);
  const dateFrom = el('input', { type: 'date', className: 'input', id: 'search-date-from', 'aria-label': '开始日期' });
  const dateTo = el('input', { type: 'date', className: 'input', id: 'search-date-to', 'aria-label': '结束日期' });
  const customDates = el('div', { className: 'search-filter-date', hidden: true }, [
    dateFrom,
    el('span', { className: 'muted', text: '至' }),
    dateTo,
  ]);
  const clearFiltersBtn = el('button', { type: 'button', className: 'btn btn-sm btn-ghost', text: '清除过滤' });

  const filtersEl = el('div', { className: 'search-filters' }, [
    el('div', { className: 'search-filter' }, [el('label', { for: 'search-folder', text: '文件夹' }), folderSelect]),
    el('div', { className: 'search-filter' }, [el('label', { for: 'search-tag', text: '标签' }), tagSelect]),
    el('div', { className: 'search-filter' }, [el('label', { for: 'search-time', text: '修改时间' }), timeSelect]),
    el('div', { className: 'search-filter' }, [el('label', { for: 'search-sort', text: '排序' }), sortSelect]),
    customDates,
    el('div', { className: 'search-filter' }, [el('label', { text: ' ' }), clearFiltersBtn]),
  ]);

  const metaCount = el('span', { text: '' });
  const metaTook = el('span', { className: 'muted', text: '' });
  const degraded = el('div', { className: 'search-degraded', hidden: true, text: '索引不可用，已降级为逐文件扫描；结果可能稍慢。' });
  const metaRow = el('div', { className: 'search-meta-row' }, [metaCount, metaTook, degraded]);

  const results = el('div', { className: 'search-results', id: 'full-search-results', role: 'listbox', 'aria-label': '检索结果' });
  const loadMoreBtn = el('button', { type: 'button', className: 'btn btn-sm search-load-more', text: '加载更多', hidden: true });
  const syntax = el('div', { className: 'search-syntax' }, [
    el('span', { text: '语法：' }),
    el('code', { text: '虚拟内存 内存' }),
    el('span', { text: '（AND） · ' }),
    el('code', { text: '"精确短语"' }),
    el('span', { text: ' · ' }),
    el('code', { text: 'tag:考研' }),
    el('span', { text: ' · 回车立即检索，Esc 关闭' }),
  ]);

  const content = el('div', { className: 'search-panel' }, [inputRow, filtersEl, metaRow, results, loadMoreBtn, syntax]);
  const closeBtn = el('button', { type: 'button', className: 'btn', text: '关闭', 'aria-label': '关闭检索面板' });

  const modal = ctx.modal.openModal({
    title: '全文检索',
    subtitle: '在全部笔记正文与标题中检索',
    content,
    footer: [closeBtn],
    size: 'xl',
    initialFocus: input,
    onClose: () => {
      if (fullSearchTimer) clearTimeout(fullSearchTimer);
      fullToken += 1;
      fullModal = null;
      fullState = null;
    },
  });
  fullModal = modal;
  fullState = {
    page: 0,
    total: 0,
    items: [],
    term: input.value,
    sort: 'relevance',
    activeIndex: -1,
    loading: false,
    navigated: false,
    filters: {},
  };
  closeBtn.addEventListener('click', () => modal.close());

  const rerun = () => runFullSearch({ reset: true });
  input.addEventListener('keydown', (event) => {
    if (event.isComposing) return;
    if (event.key === 'Enter') {
      event.preventDefault();
      if (fullState && fullState.navigated && fullState.items.length && fullState.activeIndex >= 0) {
        openSearchResult(fullState.items[fullState.activeIndex]);
      } else {
        rerun();
      }
      return;
    }
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      if (!fullState || !fullState.items.length) return;
      fullState.navigated = true;
      fullState.activeIndex = Math.min(fullState.items.length - 1, fullState.activeIndex + 1);
      highlightSearchResult();
      return;
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault();
      if (!fullState || !fullState.items.length) return;
      fullState.navigated = true;
      fullState.activeIndex = Math.max(0, fullState.activeIndex - 1);
      highlightSearchResult();
      return;
    }
  });
  input.addEventListener('input', () => {
    if (fullState) fullState.navigated = false;
    if (fullSearchTimer) clearTimeout(fullSearchTimer);
    fullSearchTimer = setTimeout(() => {
      fullSearchTimer = null;
      runFullSearch({ reset: true });
    }, 280);
  });
  sortSelect.addEventListener('change', () => {
    if (fullState) fullState.sort = sortSelect.value;
    runFullSearch({ reset: true });
  });
  folderSelect.addEventListener('change', rerun);
  tagSelect.addEventListener('change', rerun);
  timeSelect.addEventListener('change', () => {
    customDates.hidden = timeSelect.value !== 'custom';
    if (timeSelect.value === 'custom') dateFrom.focus();
    else rerun();
  });
  dateFrom.addEventListener('change', rerun);
  dateTo.addEventListener('change', rerun);
  clearFiltersBtn.addEventListener('click', () => {
    folderSelect.value = '';
    tagSelect.value = '';
    timeSelect.value = 'all';
    customDates.hidden = true;
    dateFrom.value = '';
    dateTo.value = '';
    rerun();
  });
  searchBtn.addEventListener('click', rerun);
  loadMoreBtn.addEventListener('click', () => runFullSearch({ reset: false }));
  results.addEventListener('click', (event) => {
    const node = event.target.closest('[data-result-index]');
    if (!node || !fullState) return;
    const index = Number(node.dataset.resultIndex);
    const item = fullState.items[index];
    if (item) openSearchResult(item);
  });

  // 预填过滤条件
  if (opts.filters && opts.filters.folder) folderSelect.value = opts.filters.folder;
  if (opts.filters && opts.filters.tag) tagSelect.value = opts.filters.tag;
  input.focus();
  input.select();
  runFullSearch({ reset: true });
}

function closeFull() {
  if (fullModal) {
    const modal = fullModal;
    fullModal = null;
    modal.close();
  }
}

async function fetchTags() {
  try {
    const res = await ctx.api.call('tag_list', {});
    return (res && Array.isArray(res.tags)) ? res.tags : [];
  } catch {
    return [];
  }
}

function renderSearchResults(items, append) {
  const results = document.getElementById('full-search-results');
  if (!results || !fullState) return;
  if (!append) {
    clear(results);
    fullState.activeIndex = items.length ? 0 : -1;
  }
  const startIndex = append ? fullState.items.length - items.length : 0;
  items.forEach((item, i) => {
    const index = startIndex + i;
    const titleHtml = escapeText(item.title || '');
    const snippetHtml = item.hlOffset != null && item.hlLen != null
      ? highlightChars(item.snippet || '', item.hlOffset, item.hlLen)
      : escapeText(item.snippet || '');
    const node = el('div', {
      className: `search-result${index === fullState.activeIndex ? ' active' : ''}`,
      role: 'option',
      tabindex: '-1',
      'aria-selected': index === fullState.activeIndex ? 'true' : 'false',
      dataset: { resultIndex: String(index) },
      title: item.path,
    }, [
      el('div', { className: 'search-result-title', html: titleHtml }),
      item.snippet ? el('div', { className: 'search-result-snippet', html: snippetHtml }) : null,
      el('div', { className: 'search-result-meta' }, [
        el('span', { className: 'path', text: item.path }),
        el('span', { text: formatRelative(item.mtime) }),
      ]),
    ]);
    node.addEventListener('mousemove', () => {
      if (!fullState || fullState.activeIndex === index) return;
      fullState.activeIndex = index;
      highlightSearchResult();
    });
    results.appendChild(node);
  });
  if (!append && !items.length) {
    results.appendChild(el('p', { className: 'search-empty', text: fullState.term ? '没有匹配的笔记，试试更短的关键词或更换过滤条件。' : '输入关键词开始全文检索。' }));
  }
  highlightSearchResult();
}

function escapeText(text) {
  return String(text == null ? '' : text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function highlightSearchResult() {
  const results = document.getElementById('full-search-results');
  if (!results || !fullState) return;
  const nodes = [...results.querySelectorAll('.search-result')];
  nodes.forEach((node) => {
    const index = Number(node.dataset.resultIndex);
    const active = index === fullState.activeIndex;
    node.classList.toggle('active', active);
    node.setAttribute('aria-selected', String(active));
    if (active) node.scrollIntoView({ block: 'nearest' });
  });
}

function openSearchResult(item) {
  if (!item) return;
  const modal = fullModal;
  fullModal = null;
  if (modal) modal.close();
  ctx.actions.openNote(item.path, { focus: true }).catch((err) => {
    ctx.toast.error(`打开笔记失败：${ctx.api.friendlyError(err)}`);
  });
}

function computeFilters() {
  const folder = qs('#search-folder')?.value || '';
  const tag = qs('#search-tag')?.value || '';
  const time = qs('#search-time')?.value || 'all';
  const filters = {};
  if (folder) filters.folder = folder;
  if (tag) filters.tag = tag;
  const now = Date.now();
  if (time === 'today') filters.mtimeFrom = startOfDay(now);
  else if (time === '7d') filters.mtimeFrom = daysAgoStart(7, now);
  else if (time === '30d') filters.mtimeFrom = daysAgoStart(30, now);
  else if (time === 'custom') {
    const fromVal = qs('#search-date-from')?.value;
    const toVal = qs('#search-date-to')?.value;
    if (fromVal) {
      const ts = Date.parse(`${fromVal}T00:00:00`);
      if (!Number.isNaN(ts)) filters.mtimeFrom = ts;
    }
    if (toVal) {
      const ts = Date.parse(`${toVal}T23:59:59.999`);
      if (!Number.isNaN(ts)) filters.mtimeTo = ts;
    }
  }
  return filters;
}

async function runFullSearch({ reset = false } = {}) {
  if (!fullState) return;
  if (fullState.loading && !reset) return;
  const input = document.getElementById('full-search-input');
  const sortSelect = document.getElementById('search-sort');
  if (!input) return;
  const term = input.value;
  const sort = sortSelect ? sortSelect.value : 'relevance';
  const filters = computeFilters();
  const pageSize = 50;
  const page = reset ? 0 : fullState.page;
  fullState.loading = true;
  fullState.term = term;
  fullState.sort = sort;
  fullState.filters = filters;
  const token = (fullToken += 1);
  const metaCount = document.querySelector('.search-meta-row > span');
  if (metaCount) metaCount.textContent = '检索中…';
  try {
    const res = await ctx.api.call('search_query', {
      kind: 'full',
      term,
      filters,
      sort,
      page,
      pageSize,
    });
    if (token !== fullToken || !fullState) return;
    const items = (res && Array.isArray(res.items)) ? res.items : [];
    fullState.page = page;
    fullState.total = Number(res && res.total) || 0;
    if (reset) fullState.items = items;
    else fullState.items = [...fullState.items, ...items];
    fullState.loading = false;
    renderSearchResults(items, !reset);
    const countEl = document.querySelector('.search-meta-row > span');
    if (countEl) countEl.textContent = `共 ${fullState.total} 条结果${items.length ? `（已显示 ${fullState.items.length}）` : ''}`;
    const tookEl = document.querySelector('.search-meta-row > span.muted');
    if (tookEl) tookEl.textContent = res && res.tookMs != null ? `耗时 ${res.tookMs} ms${res.parsed ? ` · 解析：${res.parsed}` : ''}` : '';
    const degradedEl = document.querySelector('.search-degraded');
    if (degradedEl) degradedEl.hidden = !(res && res.degraded);
    const loadMore = document.querySelector('.search-load-more');
    if (loadMore) loadMore.hidden = fullState.items.length >= fullState.total;
  } catch (err) {
    if (token !== fullToken || !fullState) return;
    fullState.loading = false;
    const results = document.getElementById('full-search-results');
    if (results) {
      clear(results);
      results.appendChild(el('p', { className: 'search-empty', text: `检索失败：${ctx.api.friendlyError(err)}` }));
    }
    const countEl = document.querySelector('.search-meta-row > span');
    if (countEl) countEl.textContent = '检索失败';
  }
}

// 保留给命令面板调用
export { closeFull };

/**
 * 选中检索结果中的下一个（供全局快捷键 search.next 使用）。
 * 检索面板未打开时先打开全文检索面板。
 */
export function focusNextResult() {
  if (!fullModal || !fullState) {
    openFull({});
    return false;
  }
  if (!fullState.items.length) return false;
  fullState.navigated = true;
  fullState.activeIndex = Math.min(fullState.items.length - 1, fullState.activeIndex + 1);
  highlightSearchResult();
  return true;
}
