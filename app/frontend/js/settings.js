/**
 * 设置页（模态）：外观 / 编辑器 / 备份 / 同步 / 更新 / 快捷键 / 关于。
 *
 * 持久化：app_set_config({patch}) 深合并；主题/缩放/面板可见性经 ctx.actions 即时应用。
 * 快捷键：hotkey_set —— 冲突（错误码 1004）在行内提示；录制期间暂停全局快捷键。
 */
import { ctx } from './ctx.js';
import { state, on } from './state.js';
import { el, qs, clear } from './dom.js';
import { toast } from './toast.js';
import { formatDateTime, formatRelative, formatSize } from './util.js';
import * as hotkeys from './hotkeys.js';

let modal = null;
let activeTab = 'appearance';
let data = null;
let contentEl = null;
let navEl = null;
let capture = null;
let initialized = false;

const TABS = [
  { id: 'appearance', label: '外观' },
  { id: 'editor', label: '编辑器' },
  { id: 'backup', label: '备份' },
  { id: 'sync', label: '同步' },
  { id: 'update', label: '更新' },
  { id: 'hotkeys', label: '快捷键' },
  { id: 'about', label: '关于' },
];

/** 打开设置页。 */
export async function open(tab = 'appearance') {
  if (modal) {
    showTab(tab);
    return;
  }
  activeTab = TABS.some((t) => t.id === tab) ? tab : 'appearance';
  const [config, syncView, backups, appInfo] = await Promise.all([
    ctx.api.callSafe('app_get_config', {}, state.config),
    ctx.api.callSafe('sync_config', {}, null),
    ctx.api.callSafe('backup_list', {}, { items: [] }),
    ctx.api.callSafe('app_info', {}, null),
  ]);
  if (config) state.config = config;
  data = { config: config || state.config || {}, syncView, backups: (backups && backups.items) || [], appInfo };

  navEl = el('div', { className: 'settings-nav', role: 'tablist', 'aria-label': '设置分类', 'aria-orientation': 'vertical' });
  for (const tabDef of TABS) {
    const btn = el('button', {
      type: 'button',
      className: `settings-tab${tabDef.id === activeTab ? ' active' : ''}`,
      role: 'tab',
      id: `settings-tab-${tabDef.id}`,
      'aria-selected': tabDef.id === activeTab ? 'true' : 'false',
      'aria-controls': 'settings-content',
      tabindex: tabDef.id === activeTab ? '0' : '-1',
      text: tabDef.label,
      dataset: { tab: tabDef.id },
    });
    btn.addEventListener('click', () => showTab(tabDef.id));
    navEl.appendChild(btn);
  }
  bindNavKeyboard(navEl);
  contentEl = el('div', { className: 'settings-content', id: 'settings-content', role: 'tabpanel', 'aria-labelledby': `settings-tab-${activeTab}` });
  const layout = el('div', { className: 'settings-layout' }, [navEl, contentEl]);
  const closeBtn = el('button', { type: 'button', className: 'btn', text: '关闭', 'aria-label': '关闭设置' });
  modal = ctx.modal.openModal({
    title: '设置',
    subtitle: '偏好设置会保存到 config.json（密码存储于系统凭据管理器）',
    content: layout,
    footer: [closeBtn],
    size: 'xl',
    onClose: () => {
      cancelCapture();
      modal = null;
    },
  });
  closeBtn.addEventListener('click', () => modal && modal.close());
  renderTab(activeTab);
}

/** 关闭设置页（测试/命令面板使用）。 */
export function close() {
  cancelCapture();
  if (modal) {
    const m = modal;
    modal = null;
    m.close();
  }
}

function bindNavKeyboard(nav) {
  nav.addEventListener('keydown', (event) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp' && event.key !== 'Home' && event.key !== 'End') return;
    event.preventDefault();
    const tabs = [...nav.querySelectorAll('.settings-tab')];
    const current = tabs.findIndex((t) => t.dataset.tab === activeTab);
    let next = current;
    if (event.key === 'ArrowDown') next = (current + 1) % tabs.length;
    else if (event.key === 'ArrowUp') next = (current - 1 + tabs.length) % tabs.length;
    else if (event.key === 'Home') next = 0;
    else next = tabs.length - 1;
    showTab(tabs[next].dataset.tab);
    tabs[next].focus();
  });
}

function showTab(tabId) {
  activeTab = TABS.some((t) => t.id === tabId) ? tabId : 'appearance';
  if (navEl) {
    for (const btn of navEl.querySelectorAll('.settings-tab')) {
      const active = btn.dataset.tab === activeTab;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-selected', String(active));
      btn.tabIndex = active ? 0 : -1;
    }
  }
  if (contentEl) contentEl.setAttribute('aria-labelledby', `settings-tab-${activeTab}`);
  renderTab(activeTab);
}

function renderTab(tabId) {
  if (!contentEl) return;
  cancelCapture();
  clear(contentEl);
  switch (tabId) {
    case 'appearance': contentEl.appendChild(buildAppearance()); break;
    case 'editor': contentEl.appendChild(buildEditor()); break;
    case 'backup': contentEl.appendChild(buildBackup()); break;
    case 'sync': contentEl.appendChild(buildSync()); break;
    case 'update': contentEl.appendChild(buildUpdate()); break;
    case 'hotkeys': contentEl.appendChild(buildHotkeys()); break;
    case 'about': contentEl.appendChild(buildAbout()); break;
    default: break;
  }
}

// ---------------------------------------------------------------------------
// 通用控件
// ---------------------------------------------------------------------------
function section(title, children, hint) {
  const node = el('section', { className: 'settings-section' }, [
    el('h3', { text: title }),
    hint ? el('p', { className: 'settings-hint', text: hint }) : null,
    ...children,
  ]);
  return node;
}

function row(label, desc, control) {
  return el('div', { className: 'settings-row' }, [
    el('div', { className: 'settings-label' }, [
      el('span', { text: label }),
      desc ? el('span', { className: 'desc', text: desc }) : null,
    ]),
    el('div', { className: 'settings-control' }, Array.isArray(control) ? control : [control]),
  ]);
}

function button(labelText, onClick, variant = '') {
  const btn = el('button', {
    type: 'button',
    className: `btn${variant ? ` btn-${variant}` : ''}`,
    text: labelText,
  });
  btn.addEventListener('click', () => {
    Promise.resolve(onClick(btn)).catch((err) => toast.error(ctx.api.friendlyError(err)));
  });
  return btn;
}

function statusEl() {
  return el('span', { className: 'settings-ok', role: 'status', text: '' });
}

function setStatus(node, text, kind = 'ok') {
  if (!node) return;
  node.textContent = text;
  node.className = kind === 'error' ? 'settings-inline-error' : kind === 'warn' ? 'settings-inline-warn' : 'settings-ok';
}

async function persistConfig(patch) {
  const cfg = await ctx.api.call('app_set_config', { patch });
  if (cfg) state.config = cfg;
  return cfg;
}

// ---------------------------------------------------------------------------
// 外观
// ---------------------------------------------------------------------------
function buildAppearance() {
  const cfg = data.config || {};
  const ui = cfg.ui || {};
  const zoomPercent = Math.round((Number(ui.zoom) || state.zoom || 1) * 100);
  const wrap = el('div', {});

  const themeSelect = el('select', { className: 'input', id: 'settings-theme', 'aria-label': '主题' }, [
    el('option', { value: 'auto', text: '跟随系统' }),
    el('option', { value: 'light', text: '亮色' }),
    el('option', { value: 'dark', text: '暗色' }),
  ]);
  themeSelect.value = ['auto', 'light', 'dark'].includes(ui.theme) ? ui.theme : 'auto';
  themeSelect.addEventListener('change', () => {
    ctx.actions.setTheme(themeSelect.value);
  });

  const zoomValue = el('span', { className: 'range-value', text: `${zoomPercent}%` });
  const zoomRange = el('input', {
    type: 'range',
    id: 'settings-zoom',
    min: '100',
    max: '200',
    step: '10',
    value: String(zoomPercent),
    'aria-label': '界面缩放比例',
  });
  zoomRange.addEventListener('input', () => {
    zoomValue.textContent = `${zoomRange.value}%`;
    ctx.actions.setZoom(Number(zoomRange.value) / 100);
  });
  zoomRange.addEventListener('change', () => {
    ctx.actions.setZoom(Number(zoomRange.value) / 100, { persist: true });
  });

  const toolbarCheck = checkbox('settings-toolbar', '显示格式工具栏', ui.toolbarVisible !== false);
  toolbarCheck.addEventListener('change', () => {
    ctx.actions.setPanelVisible('toolbar', toolbarCheck.checked);
  });
  const sidebarCheck = checkbox('settings-sidebar', '显示侧边栏', state.panels.sidebar);
  sidebarCheck.addEventListener('change', () => {
    ctx.actions.setPanelVisible('sidebar', sidebarCheck.checked);
  });
  const previewCheck = checkbox('settings-preview', '显示预览区', state.panels.preview);
  previewCheck.addEventListener('change', () => {
    ctx.actions.setPanelVisible('preview', previewCheck.checked);
  });

  wrap.append(
    section('主题与显示', [
      row('主题', '跟随系统时会随操作系统亮/暗色实时切换（FR-602）', themeSelect),
      row('界面缩放', '100%–200%；采用相对单位布局，缩放不截断内容（NFR-111）', [zoomRange, zoomValue]),
      row('格式工具栏', '工具栏默认可见，可随时隐藏；Ctrl+Shift+T 切换', toolbarCheck),
      row('侧边栏', '文件树/标签/回收站/检索入口；Ctrl+\\ 切换', sidebarCheck),
      row('预览区', 'Markdown 实时预览与大纲；Ctrl+Shift+M 切换', previewCheck),
    ]),
    el('div', { className: 'settings-actions' }, [
      button('恢复默认外观', async () => {
        await ctx.actions.setTheme('auto');
        await ctx.actions.setZoom(1, { persist: true });
        ctx.actions.setPanelVisible('toolbar', true);
        ctx.actions.setPanelVisible('sidebar', true);
        ctx.actions.setPanelVisible('preview', true);
        showTab('appearance');
        toast.success('外观已恢复默认');
      }),
    ]),
  );
  return wrap;
}

function checkbox(id, label, checked) {
  const input = el('input', { type: 'checkbox', id, 'aria-label': label });
  input.checked = !!checked;
  const wrap = el('label', { className: 'settings-checkbox', for: id }, [input, el('span', { text: label })]);
  wrap.style.display = 'inline-flex';
  wrap.style.alignItems = 'center';
  wrap.style.gap = '0.4rem';
  wrap.style.cursor = 'pointer';
  return Object.assign(input, { labelText: label });
}

// ---------------------------------------------------------------------------
// 编辑器
// ---------------------------------------------------------------------------
function buildEditor() {
  const cfg = data.config || {};
  const editor = cfg.editor || {};
  const current = Number(editor.autosaveDebounceMs) || 2000;
  const input = el('input', {
    type: 'number',
    className: 'input',
    id: 'settings-autosave',
    min: '300',
    max: '60000',
    step: '100',
    value: String(current),
    'aria-label': '自动保存间隔（毫秒）',
    style: { minWidth: '7rem' },
  });
  const status = statusEl();
  const saveBtn = button('保存', async () => {
    const value = Math.round(Number(input.value));
    if (!Number.isFinite(value) || value < 300 || value > 60000) {
      setStatus(status, '请输入 300–60000 之间的毫秒数', 'error');
      return;
    }
    try {
      await persistConfig({ editor: { autosaveDebounceMs: value } });
      setStatus(status, '已保存；下一篇笔记的自动保存立即使用新间隔');
      toast.success('自动保存间隔已更新');
    } catch (err) {
      setStatus(status, `保存失败：${ctx.api.friendlyError(err)}`, 'error');
    }
  });
  return section('编辑与自动保存', [
    row('自动保存间隔（毫秒）', '输入停顿达到该时间后自动保存；默认 2000ms。切换笔记/窗口失焦/退出前也会保存（FR-105）', [input, el('span', { className: 'muted', text: 'ms' }), saveBtn, status]),
    row('手动保存', 'Ctrl+S 可立即保存，始终可用', el('span', { className: 'muted', text: '快捷键可在「快捷键」页修改' })),
    row('保存冲突', '同一笔记被外部修改时，保存会提示选择「重新加载」或「覆盖保存」', el('span', { className: 'muted', text: '错误码 2004' })),
  ]);
}

// ---------------------------------------------------------------------------
// 备份
// ---------------------------------------------------------------------------
function buildBackup() {
  const cfg = data.config || {};
  const backup = cfg.backup || {};
  const dirInput = el('input', {
    type: 'text',
    className: 'input',
    id: 'settings-backup-dir',
    value: backup.dir || '',
    placeholder: '备份目录',
    'aria-label': '备份目录',
    style: { minWidth: '18rem' },
  });
  const keepInput = el('input', {
    type: 'number',
    className: 'input',
    id: 'settings-backup-keep',
    min: '3',
    max: '10',
    step: '1',
    value: String(backup.keep || 5),
    'aria-label': '备份保留份数',
    style: { minWidth: '5rem' },
  });
  const status = statusEl();
  const status2 = statusEl();
  const browseBtn = button('浏览…', async () => {
    try {
      const res = await ctx.api.call('app_open_dialog', { mode: 'folder', title: '选择备份目录' });
      if (res && res.path) dirInput.value = res.path;
    } catch (err) {
      setStatus(status, `选择目录失败：${ctx.api.friendlyError(err)}`, 'error');
    }
  });
  const saveBtn = button('保存备份设置', async () => {
    const dir = dirInput.value.trim();
    const keep = Math.round(Number(keepInput.value));
    if (!dir) {
      setStatus(status, '请填写备份目录', 'error');
      return;
    }
    if (!Number.isFinite(keep) || keep < 3 || keep > 10) {
      setStatus(status, '保留份数需在 3–10 之间', 'error');
      return;
    }
    try {
      await persistConfig({ backup: { dir, keep } });
      setStatus(status, '备份设置已保存');
      toast.success('备份设置已保存');
    } catch (err) {
      setStatus(status, `保存失败：${ctx.api.friendlyError(err)}`, 'error');
    }
  });
  const backupNowBtn = button('立即备份', async () => {
    backupNowBtn.disabled = true;
    setStatus(status2, '备份中…', 'warn');
    try {
      const res = await ctx.api.call('backup_now', {});
      setStatus(status2, `备份完成：${res.file}（${formatSize(res.size)}，保留 ${res.kept} 份）`);
      toast.success('备份完成');
      await reloadBackups();
    } catch (err) {
      setStatus(status2, `备份失败：${ctx.api.friendlyError(err)}`, 'error');
      ctx.notify.pushLocal({ level: 'error', title: '备份失败', detail: ctx.api.friendlyError(err) });
    } finally {
      backupNowBtn.disabled = false;
    }
  });

  const historyBox = el('div', { className: 'settings-list' });
  const renderHistory = () => {
    clear(historyBox);
    const items = data.backups || [];
    if (!items.length) {
      historyBox.appendChild(el('p', { className: 'muted', text: '暂无备份记录' }));
      return;
    }
    for (const item of items) {
      historyBox.appendChild(el('div', { className: 'settings-list-item' }, [
        el('span', { className: 'meta', text: item.file, title: item.file }),
        el('span', { className: 'time', text: `${formatDateTime(item.mtime)} · ${formatSize(item.size)}` }),
      ]));
    }
  };
  renderHistory();

  async function reloadBackups() {
    try {
      const res = await ctx.api.call('backup_list', {});
      data.backups = (res && res.items) || [];
      renderHistory();
    } catch {
      /* ignore */
    }
  }

  const lastBackup = backup.lastBackupAt ? `上次备份：${formatRelative(Date.parse(backup.lastBackupAt))}（${formatDateTime(Date.parse(backup.lastBackupAt))}）` : '尚未备份';

  return section('备份', [
    row('备份目录', lastBackup, [dirInput, browseBtn]),
    row('保留份数', '仅保留最近 N 份备份（3–10，默认 5）', [keepInput, el('span', { className: 'muted', text: '份' }), saveBtn]),
    row('立即备份', '手动将整库打包为 zip 备份（FR-502）', [backupNowBtn, status]),
    el('div', { className: 'settings-row' }, [
      el('div', { className: 'settings-label' }, [
        el('span', { text: '备份历史' }),
        el('span', { className: 'desc', text: '按时间倒序；超过保留份数的旧备份会被自动删除' }),
      ]),
      el('div', { className: 'settings-control' }, [button('刷新', reloadBackups)]),
    ]),
    historyBox,
    el('div', { className: 'settings-row' }, [status2]),
  ]);
}

// ---------------------------------------------------------------------------
// WebDAV 同步
// ---------------------------------------------------------------------------
function buildSync() {
  const cfg = data.config || {};
  const view = data.syncView || { configured: false, url: (cfg.sync && cfg.sync.url) || '', username: (cfg.sync && cfg.sync.username) || '', passwordSet: false, autoOnStart: false, lastRun: null };
  const urlInput = el('input', { type: 'text', className: 'input', id: 'settings-sync-url', value: view.url || '', placeholder: 'https://dav.example.com/remote.php/dav/files/user/monote', 'aria-label': 'WebDAV 地址', style: { minWidth: '20rem' } });
  const userInput = el('input', { type: 'text', className: 'input', id: 'settings-sync-user', value: view.username || '', placeholder: '账号', 'aria-label': 'WebDAV 账号', style: { minWidth: '10rem' } });
  const passInput = el('input', { type: 'password', className: 'input', id: 'settings-sync-pass', value: '', placeholder: view.passwordSet ? '已保存密码（留空保持不变）' : '密码', 'aria-label': 'WebDAV 密码', autocomplete: 'new-password', style: { minWidth: '12rem' } });
  const autoCheck = checkbox('settings-sync-auto', '启动时自动同步', !!view.autoOnStart);
  const warn = el('p', { className: 'settings-inline-warn', hidden: true, text: '' });
  const status = statusEl();
  const syncStatus = statusEl();

  const updateHttpWarning = () => {
    const url = urlInput.value.trim();
    if (url && !url.startsWith('https://')) {
      warn.hidden = false;
      warn.textContent = '地址不是 HTTPS：凭据与笔记内容可能以明文传输，建议改用 HTTPS（NFR-108 / AC-031）。';
    } else {
      warn.hidden = true;
    }
  };
  urlInput.addEventListener('input', updateHttpWarning);
  updateHttpWarning();

  const saveBtn = button('保存配置', async () => {
    const url = urlInput.value.trim();
    const username = userInput.value.trim();
    const password = passInput.value;
    if (!url) {
      setStatus(status, '请填写 WebDAV 地址', 'error');
      return;
    }
    try {
      const res = await ctx.api.call('sync_configure', {
        url,
        username,
        password: password || undefined,
        autoOnStart: autoCheck.checked,
      });
      data.syncView = res || data.syncView;
      if (state.config) state.config.sync = { ...(state.config.sync || {}), url, username, autoOnStart: autoCheck.checked };
      passInput.value = '';
      passInput.placeholder = data.syncView && data.syncView.passwordSet ? '已保存密码（留空保持不变）' : '密码';
      setStatus(status, '同步配置已保存（密码存入系统凭据管理器）');
      toast.success('WebDAV 配置已保存');
    } catch (err) {
      setStatus(status, `保存失败：${ctx.api.friendlyError(err)}`, 'error');
    }
  });
  const syncBtn = button('立即同步', async () => {
    if (!urlInput.value.trim()) {
      setStatus(syncStatus, '请先填写并保存 WebDAV 配置', 'error');
      return;
    }
    syncBtn.disabled = true;
    setStatus(syncStatus, '同步中…', 'warn');
    try {
      const res = await ctx.api.call('sync_run', {});
      setStatus(syncStatus, `同步完成：上传 ${res.up} / 下载 ${res.down} / 冲突 ${res.conflict} / 跳过 ${res.skipped}${res.failed ? ` / 失败 ${res.failed}` : ''}`);
      toast.success('同步完成');
      if (res.conflict) {
        ctx.notify.pushLocal({ level: 'warn', title: `同步发现 ${res.conflict} 个冲突副本`, detail: '冲突文件以 -conflict-<时间戳>.md 保留，未丢弃任何一版。' });
      }
    } catch (err) {
      setStatus(syncStatus, `同步失败：${ctx.api.friendlyError(err)}`, 'error');
      ctx.notify.pushLocal({ level: 'error', title: '同步失败', detail: ctx.api.friendlyError(err) });
    } finally {
      syncBtn.disabled = false;
    }
  });

  const info = [];
  if (view.configured) info.push(`已配置：${view.url}`);
  if (view.passwordSet) info.push('密码：已保存');
  if (view.lastRun) info.push(`上次同步：${formatDateTime(Date.parse(view.lastRun))}`);

  return section('WebDAV 同步', [
    row('服务器地址', '支持 WebDAV（HTTPS 推荐）；不假设服务端版本历史等扩展能力', [urlInput, saveBtn]),
    row('账号 / 密码', '密码存储于系统凭据管理器，不写入 config.json（FR-503）', [userInput, passInput]),
    row('自动同步', '启动时自动同步整库；冲突策略为「新者为准 + 冲突副本」', autoCheck),
    warn,
    row('手动同步', info.join(' · ') || '尚未配置', [syncBtn, syncStatus]),
    el('div', { className: 'settings-row' }, [status, el('span', { className: 'muted', text: '同步过程中断网不会影响本地数据。' })]),
  ]);
}

// ---------------------------------------------------------------------------
// 更新
// ---------------------------------------------------------------------------
function buildUpdate() {
  const cfg = data.config || {};
  const update = cfg.update || {};
  const autoCheck = checkbox('settings-update-auto', '自动检查更新（每 7 天最多一次，仅发送版本号与系统类型）', update.autoCheck !== false);
  autoCheck.addEventListener('change', async () => {
    try {
      await persistConfig({ update: { autoCheck: autoCheck.checked } });
      toast.success(autoCheck.checked ? '已开启自动检查' : '已关闭自动检查');
    } catch (err) {
      toast.error(`保存失败：${ctx.api.friendlyError(err)}`);
    }
  });
  const status = statusEl();
  const checkBtn = button('检查更新', async () => {
    checkBtn.disabled = true;
    setStatus(status, '检查中…', 'warn');
    try {
      const res = await ctx.api.call('app_update_check', { manual: true });
      if (res.hasUpdate) {
        setStatus(status, `发现新版本 ${res.latest}${res.notes ? `：${res.notes}` : ''}${res.url ? `（下载：${res.url}）` : ''}`, 'warn');
      } else if (res.checked) {
        setStatus(status, `已是最新版本（当前 ${res.current}）`);
      } else {
        setStatus(status, `当前版本 ${res.current}（7 天节流中，本次未发起网络请求）`, 'warn');
      }
    } catch (err) {
      const code = err && err.code;
      setStatus(status, code === 4004 || code === 4003 ? '检查失败，请检查网络后重试' : `检查失败：${ctx.api.friendlyError(err)}`, 'error');
    } finally {
      checkBtn.disabled = false;
    }
  });
  const currentVersion = (data.appInfo && data.appInfo.version) || '1.0.0';
  return section('版本更新', [
    row('当前版本', data.appInfo ? `${currentVersion} · ${data.appInfo.platform}` : currentVersion, checkBtn),
    row('自动检查', '默认每 7 天执行一次；失败静默跳过，不影响其它功能（FR-605）', autoCheck),
    row('检查结果', 'v1 仅提示新版本，不自动下载安装', status),
  ]);
}

// ---------------------------------------------------------------------------
// 快捷键
// ---------------------------------------------------------------------------
function buildHotkeys() {
  const hint = el('p', { className: 'settings-hint', text: '点击「修改」后按下新的组合键（Esc 取消）。组合键需包含 Ctrl/Shift/Alt 修饰键，或使用 F1–F12。修改通过 hotkey_set 保存，冲突会被拒绝并提示冲突命令。' });
  const list = el('div', { className: 'settings-list', id: 'settings-hotkey-list' });
  renderHotkeyRows(list);
  return section('快捷键自定义（FR-606）', [hint, list]);
}

function renderHotkeyRows(list) {
  clear(list);
  const bindings = hotkeys.listBindings();
  let lastGroup = null;
  for (const cmd of bindings) {
    if (cmd.group !== lastGroup) {
      lastGroup = cmd.group;
      list.appendChild(el('div', { className: 'palette-group-title', text: cmd.group }));
    }
    const kbd = el('kbd', { text: cmd.combo || '未设置' });
    const errorEl = el('span', { className: 'settings-inline-error', hidden: true, role: 'alert' });
    const editBtn = button('修改', () => startCapture(cmd.id, kbd, editBtn, errorEl));
    const resetBtn = button('重置', async () => {
      try {
        const res = await ctx.api.call('hotkey_set', { commandId: cmd.id, reset: true });
        if (res && res.hotkeys) hotkeys.setFromConfig(res.hotkeys);
        else hotkeys.resetCombo(cmd.id);
        renderHotkeyRows(list);
        toast.success(`已恢复「${cmd.label}」的默认快捷键`);
      } catch (err) {
        errorEl.hidden = false;
        errorEl.textContent = `重置失败：${ctx.api.friendlyError(err)}`;
      }
    }, 'ghost');
    editBtn.classList.add('btn-sm');
    resetBtn.classList.add('btn-sm');
    const rowNode = el('div', { className: 'settings-hotkey-row', dataset: { commandId: cmd.id } }, [
      el('span', { className: 'hotkey-label', text: cmd.label }),
      kbd,
      editBtn,
      resetBtn,
      errorEl,
    ]);
    list.appendChild(rowNode);
  }
}

function startCapture(commandId, kbd, editBtn, errorEl) {
  cancelCapture();
  errorEl.hidden = true;
  hotkeys.suspend();
  const original = kbd.textContent;
  kbd.textContent = '请按新组合键…';
  editBtn.textContent = '取消';
  const onKeydown = async (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (event.key === 'Escape') {
      cancelCapture();
      return;
    }
    const combo = hotkeys.captureCombo(event);
    if (!combo) return;
    const conflict = hotkeys.findConflict(combo, commandId);
    if (conflict) {
      kbd.textContent = original;
      editBtn.textContent = '修改';
      errorEl.hidden = false;
      errorEl.textContent = `与「${conflict.label}」的 ${conflict.combo} 冲突，未保存`;
      hotkeys.resume();
      capture = null;
      return;
    }
    cancelCapture({ keepText: true });
    kbd.textContent = '保存中…';
    try {
      const res = await ctx.api.call('hotkey_set', { commandId, combo });
      if (res && res.hotkeys) hotkeys.setFromConfig(res.hotkeys);
      else hotkeys.setCombo(commandId, combo);
      kbd.textContent = hotkeys.comboFor(commandId) || combo;
      toast.success('快捷键已更新');
    } catch (err) {
      kbd.textContent = original;
      errorEl.hidden = false;
      errorEl.textContent = err && err.code === 1004 ? err.message : `保存失败：${ctx.api.friendlyError(err)}`;
    }
  };
  capture = { onKeydown };
  document.addEventListener('keydown', onKeydown, true);
}

function cancelCapture(opts = {}) {
  if (!capture) {
    hotkeys.resume();
    return;
  }
  document.removeEventListener('keydown', capture.onKeydown, true);
  capture = null;
  hotkeys.resume();
  if (!opts.keepText) {
    // 行内显示由 renderHotkeyRows 恢复
    const list = document.getElementById('settings-hotkey-list');
    if (list) renderHotkeyRows(list);
  }
}

// ---------------------------------------------------------------------------
// 关于
// ---------------------------------------------------------------------------
function buildAbout() {
  const info = data.appInfo || {};
  const dl = el('dl', {}, [
    el('dt', { text: '版本' }),
    el('dd', { text: String(info.version || '1.0.0') }),
    el('dt', { text: '平台' }),
    el('dd', { text: String(info.platform || '未知') }),
    el('dt', { text: '当前笔记库' }),
    el('dd', { text: String(info.vault || (state.vault && state.vault.root) || '未打开') }),
    el('dt', { text: '笔记数量' }),
    el('dd', { text: String(info.noteCount != null ? info.noteCount : (state.vault && state.vault.noteCount) || 0) }),
    el('dt', { text: '配置文件' }),
    el('dd', { text: String(info.configPath || '—') }),
    el('dt', { text: '数据位置' }),
    el('dd', { text: '笔记为本地标准 Markdown 文件；卸载应用不影响库内数据（FR-501）' }),
  ]);
  const about = el('div', { className: 'settings-about' }, [
    dl,
    el('div', { className: 'settings-actions' }, [
      button('打开 / 切换笔记库', async () => {
        await ctx.actions.pickVault();
      }),
      ctx.api.isMock
        ? button('重置演示数据', async () => {
            const ok = await ctx.modal.confirmDialog({
              title: '重置演示数据',
              message: '将删除浏览器 localStorage 中的演示数据并重新加载页面。',
              confirmLabel: '重置并重新加载',
              danger: true,
            });
            if (ok) {
              localStorage.removeItem('monote.mock.v1');
              window.location.reload();
            }
          }, 'danger-ghost')
        : null,
    ].filter(Boolean)),
    el('p', { className: 'muted', text: 'MoNote 墨记 · 本地优先 Markdown 笔记与知识管理工具 · 需求基线 v1.1 / 设计 v1.0' }),
  ]);
  return section('关于 MoNote', [about]);
}

// ---------------------------------------------------------------------------
// 其它
// ---------------------------------------------------------------------------
export function isOpen() {
  return !!modal;
}

export function getActiveTab() {
  return activeTab;
}

// 配置变更事件（后端 config:changed 或前端主动保存）时，若设置页打开则同步 state.config
on('config-changed', (cfg) => {
  if (cfg) data = { ...(data || {}), config: cfg };
});

export default { init: () => ({ open, close, isOpen }), open, close, isOpen };
