/**
 * MoNote 前端入口（ES Module）。
 *
 * 职责：初始化各组件、注册命令与快捷键、接线 Tauri 事件、渲染状态栏、
 * 处理标题栏/欢迎页/分隔条等全局交互，最后启动库加载流程。
 */
import { api, isMock, listen, EVENTS } from './api.js';
import { ctx } from './ctx.js';
import { state, on } from './state.js';
import { el, clear, qs } from './dom.js';
import { toast } from './toast.js';
import * as modal from './modal.js';
import { formatRelative, stripExt, basename, clamp } from './util.js';
import * as actionsModule from './actions.js';
import * as editorModule from './editor.js';
import * as previewModule from './preview.js';
import * as sidebarModule from './sidebar.js';
import * as searchModule from './search.js';
import * as commandModule from './command.js';
import * as settingsModule from './settings.js';
import * as notifyModule from './notify.js';
import * as hotkeysModule from './hotkeys.js';
import * as onboardingModule from './onboarding.js';

const actions = actionsModule.actions;
let booted = false;
let menuEl = null;
let menuReturnFocus = null;
let lastErrorToastAt = 0;

// ===========================================================================
// 启动
// ===========================================================================
function boot() {
  if (booted) return;
  booted = true;
  try {
    setupContext();
    initComponents();
    registerCommands();
    bindGlobalUi();
    bindShortcuts();
    bindApiEvents();
    initStatusbar();
    bootSplitter();
    wireMockNotice();
    window.__MONOTE_READY__ = true;
    hideBoot();
    actions.bootVault().catch((err) => {
      console.error('[MoNote] 启动库加载失败', err);
      toast.error(`启动失败：${api.friendlyError(err)}`);
    });
  } catch (err) {
    console.error('[MoNote] 启动异常', err);
    const msg = document.getElementById('boot-msg');
    const hint = document.getElementById('boot-hint');
    if (msg) msg.textContent = 'MoNote 启动失败';
    if (hint) {
      hint.hidden = false;
      hint.textContent = `错误：${err && err.message ? err.message : String(err)}。请打开开发者工具查看详情。`;
    }
  }
}

function setupContext() {
  ctx.api = api;
  ctx.toast = toast;
  ctx.modal = {
    openModal: modal.openModal,
    confirmDialog: modal.confirmDialog,
    promptDialog: modal.promptDialog,
    alertDialog: modal.alertDialog,
    closeTopModal: modal.closeTopModal,
    closeAllModals: modal.closeAllModals,
    isModalOpen: modal.isModalOpen,
    button: modal.button,
  };
  ctx.actions = actions;
}

function initComponents() {
  ctx.preview = previewModule.init();
  ctx.editor = editorModule.init();
  ctx.sidebar = sidebarModule.init();
  ctx.search = searchModule.init();
  ctx.command = commandModule.init();
  ctx.settings = { open: settingsModule.open, close: settingsModule.close, isOpen: settingsModule.isOpen };
  ctx.notify = notifyModule.init();
  ctx.onboarding = onboardingModule.init();
  ctx.hotkeys = hotkeysModule;
  hotkeysModule.init({ onCommand: handleHotkeyCommand });
  ctx.actions = actions;
  actionsModule.initActions();
}

function hideBoot() {
  const bootEl = document.getElementById('boot');
  const appEl = document.getElementById('app');
  if (appEl) appEl.hidden = false;
  if (bootEl) {
    bootEl.classList.add('boot-leaving');
    setTimeout(() => bootEl.remove(), 220);
  }
}

function wireMockNotice() {
  const mockFlag = document.getElementById('status-mock');
  if (mockFlag && isMock) mockFlag.hidden = false;
  const banner = document.getElementById('app-banner');
  if (banner && isMock) {
    banner.hidden = false;
    clear(banner);
    banner.append(
      el('span', { text: '演示模式：未连接 Tauri 后端，当前使用本地 Mock 数据。运行 `cargo run` 即为真实功能。' }),
      el('button', {
        type: 'button',
        className: 'icon-btn banner-close',
        text: '×',
        'aria-label': '关闭演示模式提示',
        on: { click: () => { banner.hidden = true; } },
      }),
    );
  }
}

// ===========================================================================
// 全局 UI 事件
// ===========================================================================
function bindGlobalUi() {
  // 标题栏
  qs('#btn-new-note')?.addEventListener('click', () => actions.createNote());
  qs('#btn-quick-open')?.addEventListener('click', () => actions.openQuickOpen());
  qs('#btn-command')?.addEventListener('click', () => actions.openCommandPalette());
  qs('#btn-theme')?.addEventListener('click', () => actions.cycleTheme());
  qs('#btn-settings')?.addEventListener('click', () => actions.openSettings());
  qs('#btn-import')?.addEventListener('click', (event) => {
    showMenu(event.currentTarget, [
      { label: '导入 Markdown 文件…', icon: '📄', run: () => actions.importNotes('file') },
      { label: '导入文件夹…', icon: '📁', run: () => actions.importNotes('folder') },
    ]);
  });
  qs('#btn-export')?.addEventListener('click', (event) => {
    showMenu(event.currentTarget, [
      { label: '导出 Markdown (.md)', icon: '📝', run: () => actions.exportNote('md'), disabled: !state.current },
      { label: '导出 HTML（图片内嵌）', icon: '🌐', run: () => actions.exportNote('html'), disabled: !state.current },
      { label: '导出 PDF（打印）…', icon: '🖨', run: () => actions.exportNote('pdf'), disabled: !state.current },
      { separator: true },
      { label: '导出整库 ZIP…', icon: '🗜', run: () => actions.exportVault(), disabled: !state.vault },
    ]);
  });
  qs('#btn-toggle-sidebar')?.addEventListener('click', () => actions.toggleSidebar());
  qs('#btn-collapse-sidebar')?.addEventListener('click', () => actions.toggleSidebar());
  qs('#btn-toggle-preview')?.addEventListener('click', () => actions.togglePreview());
  qs('#btn-toggle-preview-2')?.addEventListener('click', () => actions.togglePreview());
  qs('#btn-save')?.addEventListener('click', () => ctx.editor.saveNow({ manual: true }));
  qs('#btn-empty-new')?.addEventListener('click', () => actions.createNote());
  qs('#btn-empty-open-vault')?.addEventListener('click', () => actions.pickVault());
  // 状态栏
  qs('#btn-rebuild-index')?.addEventListener('click', () => actions.rebuildIndex());
  qs('#btn-backup')?.addEventListener('click', () => actions.backupNow());
  qs('#btn-sync')?.addEventListener('click', () => actions.syncNow());
  // 欢迎页
  qs('#welcome-open-vault')?.addEventListener('click', () => actions.pickVault());
  qs('#welcome-retry')?.addEventListener('click', () => actions.retryVaultDetect());
  // 预览区折叠按钮 aria
  const previewToggle = qs('#btn-toggle-preview');
  if (previewToggle) previewToggle.setAttribute('aria-expanded', 'true');
  const sidebarToggle = qs('#btn-toggle-sidebar');
  if (sidebarToggle) sidebarToggle.setAttribute('aria-expanded', 'true');
  // 点击页面其它区域关闭右键菜单
  document.addEventListener('mousedown', (event) => {
    if (menuEl && !menuEl.contains(event.target) && !(event.target.closest && event.target.closest('[data-menu-anchor]'))) {
      closeMenu();
    }
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && menuEl) closeMenu();
  });
  // 全局错误
  window.addEventListener('error', (event) => {
    console.error('[MoNote] 未捕获异常', event.error || event.message);
  });
  window.addEventListener('unhandledrejection', (event) => {
    const err = event.reason;
    console.error('[MoNote] 未处理的 Promise 拒绝', err);
    const now = Date.now();
    if (now - lastErrorToastAt > 3000) {
      lastErrorToastAt = now;
      toast.error(`发生未处理的错误：${api.friendlyError(err)}`);
    }
  });
}

// ===========================================================================
// 通用弹出菜单
// ===========================================================================
function showMenu(anchor, items) {
  closeMenu();
  if (anchor) anchor.dataset.menuAnchor = '1';
  menuReturnFocus = document.activeElement;
  const menu = el('div', { className: 'context-menu', role: 'menu', 'aria-label': '操作菜单' });
  for (const item of items) {
    if (item.separator) {
      menu.appendChild(el('div', { className: 'menu-sep' }));
      continue;
    }
    const btn = el('button', {
      type: 'button',
      className: 'menu-item',
      role: 'menuitem',
      disabled: !!item.disabled,
    }, [
      el('span', { className: 'menu-icon', 'aria-hidden': 'true', text: item.icon || '•' }),
      el('span', { text: item.label }),
    ]);
    if (!item.disabled) {
      btn.addEventListener('click', () => {
        closeMenu();
        Promise.resolve(item.run()).catch((err) => toast.error(api.friendlyError(err)));
      });
    }
    menu.appendChild(btn);
  }
  document.body.appendChild(menu);
  menuEl = menu;
  const rect = anchor ? anchor.getBoundingClientRect() : { left: 40, right: 120, top: 40, bottom: 60 };
  const mRect = menu.getBoundingClientRect();
  let left = Math.min(rect.left, window.innerWidth - mRect.width - 8);
  let top = rect.bottom + 4;
  if (top + mRect.height > window.innerHeight - 8) top = Math.max(8, rect.top - mRect.height - 4);
  menu.style.left = `${Math.max(8, left)}px`;
  menu.style.top = `${Math.round(top)}px`;
  const first = menu.querySelector('.menu-item:not([disabled])');
  if (first) first.focus();
}

function closeMenu() {
  if (!menuEl) return;
  menuEl.remove();
  menuEl = null;
  if (menuReturnFocus && typeof menuReturnFocus.focus === 'function') {
    try {
      menuReturnFocus.focus();
    } catch {
      /* ignore */
    }
  }
  menuReturnFocus = null;
}

// ===========================================================================
// 命令注册
// ===========================================================================
function registerCommands() {
  const hasNote = () => !!state.current;
  const hasVault = () => !!state.vault;
  const findCurrentNode = () => {
    let found = null;
    const walk = (nodes) => {
      for (const node of nodes || []) {
        if (state.current && node.path === state.current.path && node.kind === 'note') found = node;
        if (node.children) walk(node.children);
      }
    };
    walk(state.tree);
    return found || { path: state.current ? state.current.path : '', kind: 'note', name: state.current ? basename(state.current.path) : '' };
  };
  const list = [
    // 通用
    { id: 'command.palette', title: '打开命令面板', group: '通用', keywords: 'command palette 命令', shortcutId: 'command.palette', run: () => actions.openCommandPalette() },
    { id: 'settings.open', title: '打开设置', group: '通用', keywords: 'settings 偏好 配置', shortcutId: 'settings.open', run: () => actions.openSettings() },
    { id: 'app.checkUpdate', title: '检查更新', group: '通用', keywords: 'update 更新 版本', run: () => actions.checkUpdate(true) },
    { id: 'app.onboarding', title: '重新查看新手引导', group: '通用', keywords: 'onboarding 引导 教程', run: () => actions.showOnboarding() },
    { id: 'app.about', title: '关于 MoNote / 版本信息', group: '通用', keywords: 'about 关于 版本', run: () => actions.openSettings('about') },
    { id: 'app.notifications', title: '打开通知中心', group: '通用', keywords: 'notify 通知', run: () => ctx.notify.openPanel() },
    // 检索
    { id: 'search.title', title: '标题快速打开（Ctrl+P）', group: '检索', keywords: 'quick open 标题 打开', shortcutId: 'search.title', run: () => actions.openQuickOpen() },
    { id: 'search.full', title: '全文检索（Ctrl+Shift+F）', group: '检索', keywords: 'search 全文 检索', shortcutId: 'search.full', run: () => actions.openFullSearch() },
    { id: 'search.next', title: '检索：选中下一个结果', group: '检索', keywords: 'next 下一个 结果', shortcutId: 'search.next', run: () => actions.searchNextResult() },
    { id: 'index.rebuild', title: '重建检索索引', group: '检索', keywords: 'index 索引 rebuild', run: () => actions.rebuildIndex() },
    // 笔记
    { id: 'note.new', title: '新建笔记', group: '笔记', keywords: 'new note 创建 新建', shortcutId: 'note.new', when: hasVault, run: () => actions.createNote() },
    { id: 'note.save', title: '保存当前笔记', group: '笔记', keywords: 'save 保存', shortcutId: 'note.save', when: hasNote, run: () => ctx.editor.saveNow({ manual: true }) },
    { id: 'note.properties', title: '编辑笔记属性（标题 / 标签）', group: '笔记', keywords: 'properties 属性 title tags', when: hasNote, run: () => actions.toggleProperties() },
    { id: 'note.backlinks', title: '切换反向链接面板', group: '笔记', keywords: 'backlinks 反链 反向', shortcutId: 'backlinks.toggle', when: hasNote, run: () => actions.toggleBacklinks() },
    { id: 'note.trash', title: '删除当前笔记（移入回收站）', group: '笔记', keywords: 'delete 删除 trash 回收站', when: hasNote, run: () => actions.trashNode(findCurrentNode()) },
    // 视图
    { id: 'sidebar.toggle', title: '折叠 / 展开侧边栏', group: '视图', keywords: 'sidebar 侧栏', shortcutId: 'sidebar.toggle', run: () => actions.toggleSidebar() },
    { id: 'toolbar.toggle', title: '显示 / 隐藏格式工具栏', group: '视图', keywords: 'toolbar 工具栏', shortcutId: 'toolbar.toggle', run: () => actions.toggleToolbar() },
    { id: 'preview.toggle', title: '折叠 / 展开预览区', group: '视图', keywords: 'preview 预览', shortcutId: 'preview.toggle', run: () => actions.togglePreview() },
    { id: 'outline.toggle', title: '切换大纲面板', group: '视图', keywords: 'outline 大纲', shortcutId: 'outline.toggle', run: () => actions.toggleOutline() },
    { id: 'theme.toggle', title: '切换主题（自动 / 亮 / 暗）', group: '视图', keywords: 'theme 主题 暗色 亮色', shortcutId: 'theme.toggle', run: () => actions.cycleTheme() },
    { id: 'view.zoomIn', title: '放大界面', group: '视图', keywords: 'zoom 缩放 放大', run: () => actions.setZoom(clamp(state.zoom + 0.1, 1, 2), { persist: true }) },
    { id: 'view.zoomOut', title: '缩小界面', group: '视图', keywords: 'zoom 缩放 缩小', run: () => actions.setZoom(clamp(state.zoom - 0.1, 1, 2), { persist: true }) },
    { id: 'view.zoomReset', title: '重置界面缩放为 100%', group: '视图', keywords: 'zoom reset 缩放 重置', run: () => actions.setZoom(1, { persist: true }) },
    { id: 'view.focusEditor', title: '聚焦编辑器', group: '视图', keywords: 'focus editor 编辑器 焦点', run: () => ctx.editor.focus() },
    // 面板
    { id: 'panel.files', title: '切换到文件面板', group: '面板', keywords: 'files 文件 树', shortcutId: 'panel.files', run: () => actions.activateSidebarTab('files') },
    { id: 'panel.tags', title: '切换到标签面板', group: '面板', keywords: 'tags 标签', shortcutId: 'panel.tags', run: () => actions.activateSidebarTab('tags') },
    { id: 'panel.trash', title: '切换到回收站面板', group: '面板', keywords: 'trash 回收站', shortcutId: 'panel.trash', run: () => actions.activateSidebarTab('trash') },
    { id: 'panel.search', title: '切换到检索入口面板', group: '面板', keywords: 'search 检索', shortcutId: 'panel.search', run: () => actions.activateSidebarTab('search') },
    { id: 'trash.open', title: '打开回收站', group: '面板', keywords: 'trash 回收站', shortcutId: 'trash.open', run: () => actions.activateSidebarTab('trash') },
    // 文件 / 导入导出
    { id: 'vault.open', title: '打开 / 切换笔记库', group: '文件', keywords: 'vault 打开 切换 笔记库', when: hasVault, run: () => actions.pickVault() },
    { id: 'vault.import', title: '导入 Markdown 文件', group: '文件', keywords: 'import 导入', shortcutId: 'vault.import', when: hasVault, run: () => actions.importNotes('file') },
    { id: 'vault.importFolder', title: '导入文件夹', group: '文件', keywords: 'import folder 导入 文件夹', when: hasVault, run: () => actions.importNotes('folder') },
    { id: 'vault.export', title: '导出当前笔记为 Markdown', group: '文件', keywords: 'export 导出 md', shortcutId: 'vault.export', when: hasNote, run: () => actions.exportNote('md') },
    { id: 'vault.export.html', title: '导出当前笔记为 HTML（图片内嵌）', group: '文件', keywords: 'export 导出 html', when: hasNote, run: () => actions.exportNote('html') },
    { id: 'vault.export.pdf', title: '导出当前笔记为 PDF（打印）', group: '文件', keywords: 'export 导出 pdf 打印', when: hasNote, run: () => actions.exportNote('pdf') },
    { id: 'vault.export.zip', title: '导出整库 ZIP', group: '文件', keywords: 'export 导出 zip 整库', when: hasVault, run: () => actions.exportVault() },
    // 备份与同步
    { id: 'backup.now', title: '立即备份', group: '备份与同步', keywords: 'backup 备份', shortcutId: 'backup.now', when: hasVault, run: () => actions.backupNow() },
    { id: 'sync.now', title: '立即同步（WebDAV）', group: '备份与同步', keywords: 'sync 同步 webdav', shortcutId: 'sync.now', when: hasVault, run: () => actions.syncNow() },
    { id: 'settings.sync', title: '配置 WebDAV 同步', group: '备份与同步', keywords: 'webdav sync 同步 配置', run: () => actions.openSettings('sync') },
    { id: 'settings.backup', title: '备份设置', group: '备份与同步', keywords: 'backup 备份 设置', run: () => actions.openSettings('backup') },
  ];
  commandModule.registerCommands(list.map((cmd) => ({
    ...cmd,
    when: cmd.when || undefined,
    run: cmd.run,
  })));
}

function handleHotkeyCommand(commandId) {
  // 快捷键 → 命令注册表；命令执行失败由 command.run 统一提示
  commandModule.run(commandId).catch((err) => console.warn('[MoNote] 快捷键执行失败', err));
  if (commandId === 'command.palette') {
    // 命令面板打开时若再次按快捷键，focus 由 command.open 处理
  }
}

// ===========================================================================
// 快捷键（编辑器内部快捷键之外）
// ===========================================================================
function bindShortcuts() {
  // 编辑器/命令内快捷键由各自模块处理；这里只做少量非注册表快捷键。
  document.addEventListener('keydown', (event) => {
    if (event.isComposing) return;
    const ctrl = event.ctrlKey || event.metaKey;
    if (ctrl && !event.shiftKey && !event.altKey && (event.key === '=' || event.key === '+')) {
      event.preventDefault();
      actions.setZoom(clamp(state.zoom + 0.1, 1, 2), { persist: true });
    } else if (ctrl && !event.shiftKey && !event.altKey && event.key === '-') {
      event.preventDefault();
      actions.setZoom(clamp(state.zoom - 0.1, 1, 2), { persist: true });
    } else if (ctrl && !event.shiftKey && !event.altKey && event.key === '0') {
      event.preventDefault();
      actions.setZoom(1, { persist: true });
    }
  });
}

// ===========================================================================
// Tauri 事件接线
// ===========================================================================
function bindApiEvents() {
  const handlers = [
    [EVENTS.NOTES_WRITTEN, (payload) => actions.handleNotesWritten(payload)],
    [EVENTS.NOTES_EXTERNAL, (payload) => actions.handleNotesExternal(payload)],
    [EVENTS.INDEX_PROGRESS, (payload) => actions.handleIndexProgress(payload)],
    [EVENTS.SYNC_PROGRESS, (payload) => actions.handleSyncProgress(payload)],
    [EVENTS.BACKUP_DONE, (payload) => actions.handleBackupDone(payload)],
    [EVENTS.NOTIFY_NEW, (item) => notifyModule.handleIncoming(item)],
    [EVENTS.VAULT_CHANGED, (payload) => actions.handleVaultChanged(payload)],
    [EVENTS.VAULT_OPENED, (opened) => actions.handleVaultOpened(opened)],
    [EVENTS.CONFIG_CHANGED, (config) => actions.handleConfigChanged(config)],
  ];
  for (const [eventName, handler] of handlers) {
    listen(eventName, handler).catch((err) => console.warn(`[MoNote] 订阅事件 ${eventName} 失败`, err));
  }
}

// ===========================================================================
// 状态栏
// ===========================================================================
function initStatusbar() {
  on('save-state', () => renderSaveState());
  on('status', () => renderTaskStatus());
  on('vault', () => { renderTaskStatus(); renderVaultButtons(); });
  renderSaveState();
  renderTaskStatus();
  renderVaultButtons();
}

function renderSaveState() {
  const node = document.getElementById('status-save');
  const text = document.getElementById('status-save-text');
  if (!node) return;
  const map = { saved: '已保存', saving: '保存中…', unsaved: '未保存', error: '保存失败' };
  const current = node.classList.contains('saved') ? 'saved'
    : node.classList.contains('saving') ? 'saving'
      : node.classList.contains('unsaved') ? 'unsaved'
        : node.classList.contains('error') ? 'error' : 'saved';
  const saveState = state.saveState || current;
  node.classList.remove('saved', 'saving', 'unsaved', 'error');
  node.classList.add(saveState);
  if (text) text.textContent = map[saveState] || '已保存';
  node.title = saveState === 'error' ? '保存失败，内容保留在编辑区；每 30 秒自动重试' : map[saveState];
}

function renderTaskStatus() {
  const idx = state.status.index;
  const idxEl = document.getElementById('status-index');
  if (idxEl) {
    if (idx.building) {
      const total = Number(idx.total) || 0;
      const done = Number(idx.done) || 0;
      const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
      idxEl.textContent = '';
      idxEl.append(
        document.createTextNode('索引 '),
        el('span', { className: 'status-progress', 'aria-hidden': 'true' }, [
          el('span', { style: { width: `${pct}%` } }),
        ]),
        document.createTextNode(total > 0 ? ` ${done}/${total}` : ' 重建中…'),
      );
      idxEl.title = '索引重建中，检索保持可用（可能降级为逐文件扫描）';
    } else if (idx.degraded) {
      idxEl.textContent = '索引降级';
      idxEl.title = '索引不可用，检索降级为逐文件扫描';
    } else {
      idxEl.textContent = `索引就绪${idx.indexed ? ` · ${idx.indexed} 篇` : ''}`;
      idxEl.title = 'SQLite FTS5 索引状态';
    }
  }
  const sync = state.status.sync;
  const syncEl = document.getElementById('status-sync');
  if (syncEl) {
    const configured = !!(state.config && state.config.sync && state.config.sync.url) || sync.configured;
    if (sync.running) {
      syncEl.textContent = `同步中${sync.phase ? ` · ${sync.phase}` : ''}`;
    } else if (sync.lastRun) {
      syncEl.textContent = `已同步 · ${formatRelative(Date.parse(sync.lastRun))}`;
    } else if (configured) {
      syncEl.textContent = '同步已配置';
    } else {
      syncEl.textContent = '同步未配置';
    }
    syncEl.title = 'WebDAV 同步状态；可在设置中配置';
  }
  const backup = state.status.backup;
  const backupEl = document.getElementById('status-backup');
  if (backupEl) {
    if (backup.running) backupEl.textContent = '备份中…';
    else if (backup.lastAt) backupEl.textContent = `备份 · ${formatRelative(backup.lastAt)}`;
    else {
      const last = state.config && state.config.backup && state.config.backup.lastBackupAt;
      backupEl.textContent = last ? `上次备份 · ${formatRelative(Date.parse(last))}` : '备份 —';
    }
    backupEl.title = '备份状态；可在设置中配置备份目录与份数';
  }
}

function renderVaultButtons() {
  const hasVault = !!state.vault;
  for (const id of ['btn-backup', 'btn-sync', 'btn-rebuild-index']) {
    const btn = document.getElementById(id);
    if (btn) btn.disabled = !hasVault;
  }
}

// ===========================================================================
// 分隔条（预览宽度）
// ===========================================================================
const SPLIT_KEY = 'monote.previewWidth';

function bootSplitter() {
  const splitter = document.getElementById('splitter');
  const workspace = document.getElementById('workspace');
  if (!splitter || !workspace) return;
  try {
    const saved = Number(localStorage.getItem(SPLIT_KEY));
    if (Number.isFinite(saved) && saved >= 20 && saved <= 70) setPreviewWidth(saved);
  } catch {
    /* ignore */
  }

  let dragging = false;
  const onMove = (event) => {
    if (!dragging) return;
    const rect = workspace.getBoundingClientRect();
    const pointerX = event.clientX != null ? event.clientX : (event.touches && event.touches[0] ? event.touches[0].clientX : 0);
    const pct = clamp(((rect.right - pointerX) / rect.width) * 100, 20, 70);
    setPreviewWidth(Math.round(pct));
  };
  const finish = () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove('dragging');
    document.body.style.cursor = '';
    document.removeEventListener('mousemove', onMove);
    document.removeEventListener('mouseup', finish);
    document.removeEventListener('touchmove', onMove);
    document.removeEventListener('touchend', finish);
    try {
      localStorage.setItem(SPLIT_KEY, String(currentPreviewWidth()));
    } catch {
      /* ignore */
    }
  };
  splitter.addEventListener('mousedown', (event) => {
    event.preventDefault();
    dragging = true;
    splitter.classList.add('dragging');
    document.body.style.cursor = 'col-resize';
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', finish);
  });
  splitter.addEventListener('touchstart', (event) => {
    if (event.touches.length !== 1) return;
    dragging = true;
    splitter.classList.add('dragging');
    document.addEventListener('touchmove', onMove, { passive: true });
    document.addEventListener('touchend', finish);
  }, { passive: true });
  splitter.addEventListener('dblclick', () => setPreviewWidth(44));
  splitter.addEventListener('keydown', (event) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight' && event.key !== 'Home') return;
    event.preventDefault();
    if (event.key === 'Home') {
      setPreviewWidth(44);
      return;
    }
    const delta = event.key === 'ArrowLeft' ? 2 : -2;
    setPreviewWidth(clamp(currentPreviewWidth() + delta, 20, 70));
  });
  setPreviewWidth(currentPreviewWidth());
}

function setPreviewWidth(pct) {
  const value = clamp(Number(pct) || 44, 20, 70);
  document.documentElement.style.setProperty('--preview-w', `${value}%`);
  const splitter = document.getElementById('splitter');
  if (splitter) splitter.setAttribute('aria-valuenow', String(Math.round(value)));
  splitter?.setAttribute('aria-valuemin', '20');
  splitter?.setAttribute('aria-valuemax', '70');
}

function currentPreviewWidth() {
  const value = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--preview-w'));
  return Number.isFinite(value) ? value : 44;
}

// ===========================================================================
// 启动
// ===========================================================================
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot, { once: true });
} else {
  boot();
}

// 暴露少量调试接口（开发期排查）
if (isMock) {
  window.MoNoteDebug = { state, ctx, actions };
}
