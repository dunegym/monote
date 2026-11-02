/**
 * 左侧边栏：文件夹树 / 标签 / 回收站 / 检索入口。
 * 负责渲染与交互，业务动作统一委托给 ctx.actions（创建/重命名/删除/打开/还原等）。
 */
import { ctx } from './ctx.js';
import { state, on } from './state.js';
import { qs, el, clear, isVisible } from './dom.js';
import { toast } from './toast.js';
import { confirmDialog } from './modal.js';
import { basename, dirname, extname, joinPath, formatRelative, formatSize } from './util.js';

let treeEl = null;
let tagListEl = null;
let trashListEl = null;
let tagResultsEl = null;
let tagFilterBar = null;
let treeEmpty = null;
let currentTab = 'files';
let expanded = new Set();
let focusedPath = null;
let tagResultsToken = 0;
let contextMenu = null;
let bound = false;

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------
export function init() {
  treeEl = qs('#tree');
  tagListEl = qs('#tag-list');
  trashListEl = qs('#trash-list');
  tagResultsEl = qs('#tag-results');
  tagFilterBar = qs('#tag-filter-bar');
  treeEmpty = qs('#tree-empty');
  if (bound) return getApi();
  bound = true;

  bindTabs();
  bindToolbar();
  bindTreeKeyboard();
  bindSearchEntry();
  if (tagListEl) {
    tagListEl.addEventListener('click', (event) => {
      const chip = event.target.closest('[data-tag]');
      if (chip) selectTag(chip.dataset.tag);
    });
  }
  if (tagFilterBar) {
    tagFilterBar.addEventListener('click', (event) => {
      if (event.target.closest('[data-clear-tag]')) clearTagFilter();
    });
  }
  if (tagResultsEl) {
    tagResultsEl.addEventListener('click', (event) => {
      const item = event.target.closest('[data-note-path]');
      if (item) openPath(item.dataset.notePath);
    });
  }
  if (trashListEl) {
    trashListEl.addEventListener('click', (event) => {
      const restore = event.target.closest('[data-restore-trash]');
      if (restore) restoreTrash(restore.dataset.restoreTrash);
    });
  }
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') closeContextMenu();
  });
  on('note', (note) => {
    if (!note) return;
    setActiveNote(note.path);
  });
  on('tree', () => renderTree());
  return getApi();
}

function getApi() {
  return {
    refreshTree,
    refreshTags,
    refreshTrash,
    activateTab,
    setActiveNote,
    getFolders,
    focusTree,
  };
}

// ---------------------------------------------------------------------------
// Tab 切换
// ---------------------------------------------------------------------------
const TAB_PANELS = {
  files: 'panel-files',
  tags: 'panel-tags',
  trash: 'panel-trash',
  search: 'panel-search',
};

function bindTabs() {
  const tabs = [...document.querySelectorAll('.sidebar-tabs [role="tab"]')];
  tabs.forEach((tab) => {
    tab.addEventListener('click', () => activateTab(tab.dataset.tab));
    tab.addEventListener('keydown', (event) => {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
      event.preventDefault();
      const index = tabs.indexOf(tab);
      const next = tabs[(index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length];
      next.focus();
      activateTab(next.dataset.tab);
    });
  });
}

/**
 * 切换侧边栏 Tab。
 * @param {'files'|'tags'|'trash'|'search'} name
 */
export function activateTab(name) {
  const target = TAB_PANELS[name] ? name : 'files';
  currentTab = target;
  state.sidebarTab = target;
  for (const [key, panelId] of Object.entries(TAB_PANELS)) {
    const panel = document.getElementById(panelId);
    const tab = document.querySelector(`.sidebar-tabs [data-tab="${key}"]`);
    const active = key === target;
    if (panel) panel.hidden = !active;
    if (tab) {
      tab.classList.toggle('active', active);
      tab.setAttribute('aria-selected', String(active));
      tab.tabIndex = active ? 0 : -1;
    }
  }
  if (target === 'tags') refreshTags();
  else if (target === 'trash') refreshTrash();
  else if (target === 'files' && !state.tree.length) refreshTree();
}

// ---------------------------------------------------------------------------
// 工具栏按钮
// ---------------------------------------------------------------------------
function bindToolbar() {
  qs('#btn-new-note-side')?.addEventListener('click', () => ctx.actions.createNote({ parent: parentForNewItem() }));
  qs('#btn-new-folder-side')?.addEventListener('click', () => ctx.actions.createFolder({ parent: parentForNewItem() }));
  qs('#btn-refresh-tree')?.addEventListener('click', () => refreshTree());
  qs('#btn-refresh-tags')?.addEventListener('click', () => refreshTags());
  qs('#btn-refresh-trash')?.addEventListener('click', () => refreshTrash());
  qs('#btn-empty-trash')?.addEventListener('click', () => emptyTrash());
}

/** 当前选中的文件夹（用于新建）；若没有则根目录。 */
function parentForNewItem() {
  if (focusedPath) {
    const node = findNode(focusedPath);
    if (node && node.kind === 'folder') return node.path;
    if (node && node.kind === 'note') return dirname(node.path);
  }
  return '';
}

// ---------------------------------------------------------------------------
// 文件树
// ---------------------------------------------------------------------------
function findNode(path, nodes = state.tree) {
  for (const node of nodes || []) {
    if (node.path === path) return node;
    if (node.children) {
      const found = findNode(path, node.children);
      if (found) return found;
    }
  }
  return null;
}

/** 刷新文件树（调用 vault_list）。 */
export async function refreshTree() {
  try {
    const res = await ctx.api.call('vault_list', {});
    const nodes = (res && Array.isArray(res.nodes)) ? res.nodes : [];
    state.tree = nodes;
    state.emitTree = true;
    // 展开当前笔记所在路径
    if (state.current) expandAncestors(state.current.path);
    renderTree();
    return nodes;
  } catch (err) {
    console.error('[MoNote] 文件树加载失败', err);
    if (treeEl) {
      clear(treeEl);
      treeEl.appendChild(el('p', { className: 'tree-empty', text: `文件树加载失败：${ctx.api.friendlyError(err)}` }));
    }
    return [];
  }
}

function expandAncestors(path) {
  let dir = dirname(path);
  while (dir) {
    expanded.add(dir);
    dir = dirname(dir);
  }
}

function toggleFolder(path, force) {
  const shouldExpand = force != null ? force : !expanded.has(path);
  if (shouldExpand) expanded.add(path);
  else expanded.delete(path);
  renderTree();
}

function renderTree() {
  if (!treeEl) return;
  clear(treeEl);
  const nodes = state.tree || [];
  if (!nodes.length) {
    if (treeEmpty) treeEmpty.hidden = false;
    treeEl.appendChild(el('p', { className: 'tree-empty', text: '笔记库为空' }));
    return;
  }
  if (treeEmpty) treeEmpty.hidden = true;
  const rootGroup = el('div', { role: 'group' });
  treeEl.appendChild(rootGroup);
  for (const node of nodes) rootGroup.appendChild(renderNode(node, 0));
  // roving tabindex
  const rows = visibleRows();
  if (rows.length) {
    const preferred = rows.find((row) => row.dataset.path === focusedPath) || rows.find((row) => row.dataset.path === (state.current && state.current.path)) || rows[0];
    for (const row of rows) row.tabIndex = -1;
    preferred.tabIndex = 0;
    focusedPath = preferred.dataset.path;
  }
  highlightActive();
}

function renderNode(node, depth) {
  const isFolder = node.kind === 'folder';
  const isExpanded = isFolder && expanded.has(node.path);
  const isActive = state.current && state.current.path === node.path;
  const row = el('div', {
    className: `tree-row ${isFolder ? 'folder' : 'note'}${isActive ? ' selected' : ''}`,
    role: 'treeitem',
    tabindex: '-1',
    'aria-level': String(depth + 1),
    'aria-selected': isActive ? 'true' : 'false',
    'aria-expanded': isFolder ? String(isExpanded) : null,
    dataset: { path: node.path, kind: node.kind },
    title: node.path,
    style: { paddingLeft: `calc(0.25rem + ${depth * 0.85}rem)` },
  });

  const toggle = el('button', {
    type: 'button',
    className: `tree-toggle${isFolder ? '' : ' leaf'}`,
    tabindex: '-1',
    'aria-label': isFolder ? (isExpanded ? '折叠文件夹' : '展开文件夹') : '',
    'aria-hidden': isFolder ? null : 'true',
    text: isFolder ? (isExpanded ? '▾' : '▸') : '•',
  });
  toggle.addEventListener('click', (event) => {
    event.stopPropagation();
    if (isFolder) toggleFolder(node.path, !isExpanded);
  });

  const icon = el('span', { className: 'tree-icon', 'aria-hidden': 'true', text: isFolder ? (isExpanded ? '📂' : '📁') : '📄' });
  const label = el('span', { className: 'tree-label', text: node.title || node.name || basename(node.path) });

  const actions = el('span', { className: 'tree-actions' });
  const menuBtn = el('button', {
    type: 'button',
    className: 'icon-btn',
    tabindex: '-1',
    'aria-label': `更多操作：${node.name || node.path}`,
    title: '更多操作',
    text: '⋯',
  });
  menuBtn.addEventListener('click', (event) => {
    event.stopPropagation();
    openContextMenu(menuBtn, node);
  });
  actions.appendChild(menuBtn);

  row.append(toggle, icon, label, actions);
  row.addEventListener('click', (event) => {
    if (event.target.closest('.tree-actions')) return;
    focusedPath = node.path;
    if (isFolder) toggleFolder(node.path);
    else openPath(node.path);
  });

  const wrapper = el('div', { className: 'tree-node' }, [row]);
  if (isFolder) {
    const childGroup = el('div', { role: 'group', className: `tree-children${isExpanded ? '' : ' collapsed'}` });
    for (const child of node.children || []) childGroup.appendChild(renderNode(child, depth + 1));
    if (isExpanded && (!node.children || !node.children.length)) {
      childGroup.appendChild(el('p', { className: 'tree-empty', text: '（空文件夹）' }));
    }
    wrapper.appendChild(childGroup);
  }
  return wrapper;
}

function visibleRows() {
  if (!treeEl) return [];
  return [...treeEl.querySelectorAll('.tree-row')].filter((row) => isVisible(row));
}

function highlightActive() {
  if (!treeEl) return;
  const activePath = state.current ? state.current.path : null;
  for (const row of treeEl.querySelectorAll('.tree-row')) {
    const selected = row.dataset.path === activePath;
    row.classList.toggle('selected', selected);
    row.setAttribute('aria-selected', String(selected));
  }
}

function setActiveNote(path) {
  if (!path) {
    highlightActive();
    return;
  }
  expandAncestors(path);
  if (!treeEl) return;
  const row = [...treeEl.querySelectorAll('.tree-row')].find((r) => r.dataset.path === path);
  if (!row) {
    renderTree();
    return;
  }
  highlightActive();
  focusedPath = path;
  row.scrollIntoView({ block: 'nearest' });
}

function openPath(path) {
  ctx.actions.openNote(path).catch((err) => {
    toast.error(`打开笔记失败：${ctx.api.friendlyError(err)}`);
  });
}

function focusTree() {
  const rows = visibleRows();
  if (!rows.length) return;
  const row = rows.find((r) => r.dataset.path === focusedPath) || rows[0];
  row.tabIndex = 0;
  row.focus();
}

function bindTreeKeyboard() {
  if (!treeEl) return;
  treeEl.addEventListener('keydown', (event) => {
    const row = event.target.closest('.tree-row');
    if (!row) return;
    const rows = visibleRows();
    const index = rows.indexOf(row);
    if (index < 0) return;
    const move = (next) => {
      if (!next) return;
      event.preventDefault();
      for (const r of rows) r.tabIndex = -1;
      next.tabIndex = 0;
      next.focus();
      focusedPath = next.dataset.path;
    };
    switch (event.key) {
      case 'ArrowDown':
        move(rows[index + 1]);
        break;
      case 'ArrowUp':
        move(rows[index - 1]);
        break;
      case 'Home':
        move(rows[0]);
        break;
      case 'End':
        move(rows[rows.length - 1]);
        break;
      case 'ArrowRight': {
        event.preventDefault();
        const node = findNode(row.dataset.path);
        if (node && node.kind === 'folder') {
          if (!expanded.has(node.path)) toggleFolder(node.path, true);
          else {
            const children = visibleRows();
            const child = children[children.indexOf(row) + 1];
            if (child && child.dataset.path.startsWith(`${node.path}/`)) move(child);
          }
        }
        break;
      }
      case 'ArrowLeft': {
        event.preventDefault();
        const node = findNode(row.dataset.path);
        if (node && node.kind === 'folder' && expanded.has(node.path)) {
          toggleFolder(node.path, false);
        } else {
          const parent = visibleRows().find((r) => dirname(row.dataset.path) === r.dataset.path || (dirname(row.dataset.path).startsWith(`${r.dataset.path}/`) && r.dataset.kind === 'folder'));
          if (parent) move(parent);
        }
        break;
      }
      case 'Enter':
      case ' ':
        event.preventDefault();
        if (row.dataset.kind === 'folder') toggleFolder(row.dataset.path);
        else openPath(row.dataset.path);
        break;
      default:
        break;
    }
  });
}

// ---------------------------------------------------------------------------
// 右键/更多操作菜单
// ---------------------------------------------------------------------------
function closeContextMenu() {
  if (!contextMenu) return;
  const old = contextMenu;
  contextMenu = null;
  old.remove();
}

function openContextMenu(anchor, node) {
  closeContextMenu();
  const menu = el('div', { className: 'context-menu', role: 'menu', 'aria-label': `${node.name || node.path} 的操作` });
  contextMenu = menu;
  const isFolder = node.kind === 'folder';
  const addItem = (labelText, iconText, onClick, danger = false) => {
    const item = el('button', {
      type: 'button',
      className: `menu-item${danger ? ' danger' : ''}`,
      role: 'menuitem',
      tabindex: '0',
    }, [
      el('span', { className: 'menu-icon', 'aria-hidden': 'true', text: iconText }),
      el('span', { text: labelText }),
    ]);
    item.addEventListener('click', () => {
      closeContextMenu();
      onClick();
    });
    menu.appendChild(item);
    return item;
  };
  if (isFolder) {
    addItem('新建笔记', '＋', () => ctx.actions.createNote({ parent: node.path }));
    addItem('新建子文件夹', '📁', () => ctx.actions.createFolder({ parent: node.path }));
    menu.appendChild(el('div', { className: 'menu-sep' }));
  }
  addItem('重命名 / 移动', '✎', () => ctx.actions.renameNode(node));
  addItem('移入回收站', '🗑', () => ctx.actions.trashNode(node), true);
  document.body.appendChild(menu);

  const rect = anchor.getBoundingClientRect();
  const menuRect = menu.getBoundingClientRect();
  let left = rect.right - menuRect.width;
  if (left < 6) left = 6;
  let top = rect.bottom + 4;
  if (top + menuRect.height > window.innerHeight - 6) top = Math.max(6, rect.top - menuRect.height - 4);
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;

  setTimeout(() => {
    const first = menu.querySelector('.menu-item');
    if (first) first.focus();
    const onDown = (event) => {
      if (contextMenu && !contextMenu.contains(event.target)) {
        closeContextMenu();
        document.removeEventListener('mousedown', onDown, true);
      }
    };
    document.addEventListener('mousedown', onDown, true);
  }, 0);
  menu.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      closeContextMenu();
      anchor.focus();
    }
  });
}

// ---------------------------------------------------------------------------
// 标签
// ---------------------------------------------------------------------------
/** 刷新标签列表。 */
export async function refreshTags() {
  if (!tagListEl) return;
  try {
    const res = await ctx.api.call('tag_list', {});
    const tags = (res && Array.isArray(res.tags)) ? res.tags : [];
    clear(tagListEl);
    if (!tags.length) {
      tagListEl.appendChild(el('p', { className: 'empty-mini', text: '还没有标签。在属性面板或正文中用 #标签 添加。' }));
      return;
    }
    for (const tag of tags) {
      const chip = el('button', {
        type: 'button',
        className: `tag-chip${state.activeTag === tag.name ? ' active' : ''}`,
        dataset: { tag: tag.name },
        'aria-pressed': state.activeTag === tag.name ? 'true' : 'false',
        title: `筛选包含 #${tag.name} 的笔记（${tag.count} 篇）`,
      }, [
        el('span', { text: `#${tag.name}` }),
        el('span', { className: 'tag-count', text: String(tag.count) }),
      ]);
      tagListEl.appendChild(chip);
    }
  } catch (err) {
    clear(tagListEl);
    tagListEl.appendChild(el('p', { className: 'empty-mini', text: `标签加载失败：${ctx.api.friendlyError(err)}` }));
  }
}

async function selectTag(tag) {
  if (!tag) return;
  if (state.activeTag === tag) {
    clearTagFilter();
    return;
  }
  state.activeTag = tag;
  await refreshTags();
  activateTab('tags');
  if (tagFilterBar) {
    clear(tagFilterBar);
    tagFilterBar.hidden = false;
    tagFilterBar.append(
      el('span', { text: `筛选：#${tag}` }),
      el('button', { type: 'button', className: 'btn btn-sm btn-ghost', dataset: { clearTag: '1' }, text: '清除' }),
    );
  }
  if (tagResultsEl) {
    clear(tagResultsEl);
    tagResultsEl.appendChild(el('p', { className: 'empty-mini', text: '正在筛选…' }));
  }
  const token = (tagResultsToken += 1);
  try {
    const res = await ctx.api.call('search_query', {
      kind: 'full',
      term: `tag:${tag}`,
      filters: {},
      sort: 'mtime',
      page: 0,
      pageSize: 50,
    });
    if (token !== tagResultsToken) return;
    const items = (res && Array.isArray(res.items)) ? res.items : [];
    if (!tagResultsEl) return;
    clear(tagResultsEl);
    if (!items.length) {
      tagResultsEl.appendChild(el('p', { className: 'empty-mini', text: `没有包含 #${tag} 的笔记` }));
      return;
    }
    for (const item of items) {
      const node = el('div', {
        className: 'note-list-item',
        role: 'button',
        tabindex: '0',
        dataset: { notePath: item.path },
        title: item.path,
      }, [
        el('span', { className: 'note-item-title', text: item.title || basename(item.path) }),
        el('span', { className: 'note-item-meta', text: `${item.path} · ${formatRelative(item.mtime)}` }),
      ]);
      node.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          openPath(item.path);
        }
      });
      tagResultsEl.appendChild(node);
    }
  } catch (err) {
    if (token !== tagResultsToken) return;
    if (tagResultsEl) {
      clear(tagResultsEl);
      tagResultsEl.appendChild(el('p', { className: 'empty-mini', text: `筛选失败：${ctx.api.friendlyError(err)}` }));
    }
  }
}

function clearTagFilter() {
  state.activeTag = null;
  tagResultsToken += 1;
  if (tagFilterBar) {
    tagFilterBar.hidden = true;
    clear(tagFilterBar);
  }
  if (tagResultsEl) clear(tagResultsEl);
  refreshTags();
}

// ---------------------------------------------------------------------------
// 回收站
// ---------------------------------------------------------------------------
/** 刷新回收站列表。 */
export async function refreshTrash() {
  if (!trashListEl) return;
  try {
    const res = await ctx.api.call('vault_trash_list', {});
    const items = (res && Array.isArray(res.items)) ? res.items : [];
    clear(trashListEl);
    if (!items.length) {
      trashListEl.appendChild(el('p', { className: 'empty-mini', text: '回收站是空的。' }));
      return;
    }
    for (const item of items) {
      const remaining = Number(item.remainingDays);
      let daysText = '';
      let urgent = false;
      if (!Number.isFinite(remaining)) daysText = '剩余天数未知';
      else if (remaining <= 0) {
        daysText = '已到期，将在下次启动时永久清除';
        urgent = true;
      } else if (remaining <= 7) {
        daysText = `${remaining} 天后自动永久清除`;
        urgent = true;
      } else {
        daysText = `剩余 ${remaining} 天`;
      }
      const node = el('div', { className: 'trash-item', 'aria-label': `回收站条目 ${item.name}` }, [
        el('div', { className: 'trash-head' }, [
          el('span', { className: 'tree-icon', 'aria-hidden': 'true', text: item.isDir ? '📁' : '📄' }),
          el('span', { className: 'trash-name', text: item.name || basename(item.originalPath || item.id) }),
        ]),
        el('div', { className: 'trash-meta', text: `${item.originalPath || '（原路径未知）'}${item.size ? ` · ${formatSize(item.size)}` : ''}` }),
        el('div', { className: 'trash-meta', text: `删除于 ${formatRelative(Date.parse(item.deletedAt))}` }),
        el('div', { className: `trash-days${urgent ? ' urgent' : ''}`, text: daysText }),
        el('div', { className: 'trash-actions' }, [
          el('button', {
            type: 'button',
            className: 'btn btn-sm',
            text: '还原',
            dataset: { restoreTrash: item.id },
            'aria-label': `还原 ${item.name}`,
          }),
        ]),
      ]);
      trashListEl.appendChild(node);
    }
  } catch (err) {
    clear(trashListEl);
    trashListEl.appendChild(el('p', { className: 'empty-mini', text: `回收站加载失败：${ctx.api.friendlyError(err)}` }));
  }
}

async function restoreTrash(trashId) {
  try {
    const res = await ctx.api.call('vault_trash_restore', { trashId });
    if (res && res.renamedTo) toast.warn(`原路径已存在同名文件，已还原为：${res.renamedTo}`);
    else toast.success(`已还原到 ${res && res.path ? res.path : '原路径'}`);
    await ctx.actions.refreshAfterMutation();
  } catch (err) {
    toast.error(`还原失败：${ctx.api.friendlyError(err)}`);
  }
}

async function emptyTrash() {
  if (!trashListEl) return;
  const count = trashListEl.querySelectorAll('.trash-item').length;
  const ok = await confirmDialog({
    title: '清空回收站',
    message: `将永久删除回收站中的 ${count} 项内容，此操作无法恢复。`,
    detail: '（按设计，回收站条目在进入回收站 30 天后也会自动清除。）',
    confirmLabel: '永久删除',
    cancelLabel: '取消',
    danger: true,
  });
  if (!ok) return;
  try {
    const res = await ctx.api.call('vault_empty_trash', {});
    toast.success(`已清空回收站（${res && res.cleared != null ? res.cleared : count} 项）`);
    await refreshTrash();
  } catch (err) {
    toast.error(`清空回收站失败：${ctx.api.friendlyError(err)}`);
  }
}

// ---------------------------------------------------------------------------
// 检索入口
// ---------------------------------------------------------------------------
function bindSearchEntry() {
  const input = qs('#sidebar-search-input');
  if (input) {
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && !event.isComposing) {
        event.preventDefault();
        const term = input.value.trim();
        if (term) ctx.search.openFull({ term });
        else ctx.search.openFull({});
      }
    });
  }
  qs('#btn-quick-open-2')?.addEventListener('click', () => ctx.search.openQuick());
  qs('#btn-full-search')?.addEventListener('click', () => {
    const term = input ? input.value.trim() : '';
    ctx.search.openFull(term ? { term } : {});
  });
}

// ---------------------------------------------------------------------------
// 其它
// ---------------------------------------------------------------------------
function getFolders() {
  const folders = [];
  const walk = (nodes, depth) => {
    for (const node of nodes || []) {
      if (node.kind === 'folder') {
        folders.push({ path: node.path, name: node.name || basename(node.path), depth });
        walk(node.children, depth + 1);
      }
    }
  };
  walk(state.tree, 0);
  return folders;
}

/** 文件树是否包含某路径（检测外部删除）。 */
export function hasPath(path) {
  return !!findNode(path);
}

export { extname };

/** 兼容直接调用（部分模块通过 ctx.sidebar 使用）。 */
export default {
  init,
  refreshTree,
  refreshTags,
  refreshTrash,
  activateTab,
  setActiveNote,
  getFolders,
  focusTree,
};
