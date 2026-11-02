/**
 * 动作层：跨组件的业务操作（打开/新建/重命名/回收站/导入导出/备份/同步/主题/面板…）。
 * 组件只调用 `ctx.actions.*`，避免组件之间的循环依赖。
 */
import { ctx } from './ctx.js';
import { state, setState, setStatus, emit } from './state.js';
import { api, EVENTS, invoke } from './api.js';
import { toast } from './toast.js';
import { confirmDialog, promptDialog, openModal, alertDialog, button } from './modal.js';
import {
  basename,
  stripExt,
  dirname,
  extname,
  joinPath,
  clamp,
  debounce,
  formatSize,
  formatDateTime,
  formatRelative,
  escapeHtml,
} from './util.js';
import { DEFAULT_HOTKEYS } from './hotkeys.js';

const RECENT_KEY = 'monote.recent';
const THEME_CYCLE = ['auto', 'light', 'dark'];
const THEME_LABELS = { auto: '跟随系统', light: '亮色', dark: '暗色' };

let defaultConfigFactory = () => ({
  version: 1,
  vault: { root: null },
  ui: { theme: 'auto', language: 'zh-CN', zoom: 1.0, toolbarVisible: true, sidebarVisible: true, previewVisible: true },
  editor: { autosaveDebounceMs: 2000 },
  backup: { dir: '', keep: 5, lastBackupAt: null },
  sync: { autoOnStart: false, url: '', username: '' },
  update: { autoCheck: true, lastCheck: null, manifestUrl: 'https://update.monote.app/manifest.json' },
  hotkeys: { ...DEFAULT_HOTKEYS },
  onboardingDone: false,
});

let booting = false;
let vaultWaitResolve = null;
let vaultOpenSeq = 0;
let lastOpenedRoot = null;
let manualBackupRunning = false;
const persistZoom = debounce(async () => {
  try {
    const cfg = await api.call('app_set_config', { patch: { ui: { zoom: state.zoom } } });
    if (cfg) state.config = cfg;
  } catch (err) {
    console.warn('[MoNote] 保存缩放设置失败', err);
  }
}, 500);

// ===========================================================================
// 启动 / 库
// ===========================================================================
/** 读取配置 + 恢复当前库；未打开库时显示欢迎页。 */
export async function bootVault() {
  if (booting) return;
  booting = true;
  try {
    loadRecent();
    let config = await api.callSafe('app_get_config', {}, null);
    if (!config) {
      console.warn('[MoNote] app_get_config 不可用，使用默认配置（后端可能尚未就绪）');
      config = defaultConfigFactory();
    }
    applyConfig(config, { persist: false });
    updateVaultUI();

    let info = await api.callSafe('vault_info', {}, null);
    if (!info && state.config && state.config.vault && state.config.vault.root) {
      // 兼容后端尚未实现 vault_info 的情况：尝试用配置中的库路径打开
      info = await api.callSafe('vault_open', { rootPath: state.config.vault.root }, null);
    }
    if (info) {
      await afterOpen(info);
    } else {
      // 等待后端启动时异步恢复库完成（vault:opened 事件），最多 2.5s
      const opened = await waitVaultOpened(2500);
      if (opened) await afterOpen(opened);
      else showWelcome();
    }
  } catch (err) {
    console.error('[MoNote] 启动加载失败', err);
    showWelcome(`启动加载失败：${api.friendlyError(err)}`);
  } finally {
    booting = false;
  }
}

function waitVaultOpened(timeoutMs) {
  return new Promise((resolve) => {
    vaultWaitResolve = resolve;
    setTimeout(() => {
      if (vaultWaitResolve === resolve) {
        vaultWaitResolve = null;
        resolve(null);
      }
    }, timeoutMs);
  });
}

/** 事件 vault:opened 的处理。 */
export async function handleVaultOpened(opened) {
  if (!opened || !opened.root) return;
  if (vaultWaitResolve) {
    const resolve = vaultWaitResolve;
    vaultWaitResolve = null;
    resolve(opened);
    return;
  }
  if (state.vault && state.vault.root === opened.root) {
    setState({ vault: { ...state.vault, ...opened } }, 'vault');
    return;
  }
  await afterOpen(opened);
}

let openInFlight = null;
let openInFlightRoot = null;

/** 打开库后初始化 UI；同一库的并发调用（如 vault_pick + vault:opened 事件）会合并。 */
async function afterOpen(opened) {
  if (openInFlight && openInFlightRoot === opened.root) return openInFlight;
  openInFlightRoot = opened.root;
  openInFlight = doAfterOpen(opened);
  try {
    return await openInFlight;
  } finally {
    if (openInFlightRoot === opened.root) {
      openInFlight = null;
      openInFlightRoot = null;
    }
  }
}

async function doAfterOpen(opened) {
  const seq = (vaultOpenSeq += 1);
  lastOpenedRoot = opened.root;
  setState({
    vault: {
      root: opened.root,
      name: opened.name || basename(opened.root),
      noteCount: opened.noteCount != null ? opened.noteCount : 0,
      indexOk: opened.indexOk !== false,
      indexBuilding: !!opened.indexBuilding,
    },
  }, 'vault');
  hideWelcome();
  ctx.editor.clearNote();
  try {
    await refreshAll();
  } catch (err) {
    console.warn('[MoNote] 打开库后加载数据失败', err);
  }
  if (seq !== vaultOpenSeq) return;
  if (state.config) {
    state.config.vault = { ...(state.config.vault || {}), root: opened.root };
  }
  updateVaultUI();
  document.body.classList.remove('no-vault');
  if (opened.indexBuilding) setStatus('index', { building: true, phase: 'rebuild' });
  loadIndexStatus();
  refreshNotify();
  ctx.onboarding.maybeShow();
  document.title = 'MoNote 墨记';
}

/** 选择/新建笔记库。 */
export async function pickVault() {
  try {
    if (state.current) await ctx.editor.flush();
    const opened = await api.call('vault_pick', {});
    if (!opened) return false; // 用户取消
    await afterOpen(opened);
    toast.success(`已打开笔记库：${opened.name || basename(opened.root)}`);
    return true;
  } catch (err) {
    toast.error(`打开笔记库失败：${api.friendlyError(err)}`);
    return false;
  }
}

/** 重新检测当前库（欢迎页按钮）。 */
export async function retryVaultDetect() {
  try {
    const info = await api.call('vault_info', {});
    if (info) {
      await afterOpen(info);
      return true;
    }
    const opened = await api.callSafe('vault_open', { rootPath: (state.config && state.config.vault && state.config.vault.root) || '' }, null);
    if (opened) {
      await afterOpen(opened);
      return true;
    }
    showWelcome('还没有打开的笔记库：请点击下方按钮选择或新建一个文件夹作为笔记库。');
    return false;
  } catch (err) {
    showWelcome(`检测失败：${api.friendlyError(err)}`);
    return false;
  }
}

function showWelcome(hint) {
  const welcome = document.getElementById('welcome-screen');
  if (welcome) welcome.hidden = false;
  setState({ vault: null }, 'vault');
  updateVaultUI();
  if (hint) {
    const el = document.getElementById('welcome-hint');
    if (el) el.textContent = hint;
  }
}

function hideWelcome() {
  const welcome = document.getElementById('welcome-screen');
  if (welcome) welcome.hidden = true;
}

/** 刷新目录树/标签/回收站/索引状态/通知。 */
export async function refreshAll() {
  const tasks = [
    ctx.sidebar.refreshTree().then((nodes) => {
      rebuildNoteNames(nodes || state.tree);
      return nodes;
    }),
    ctx.sidebar.refreshTags(),
    ctx.sidebar.refreshTrash(),
    loadIndexStatus(),
    refreshNotify(),
  ];
  await Promise.all(tasks.map((p) => Promise.resolve(p).catch((err) => {
    console.warn('[MoNote] 刷新数据失败', err);
  })));
  updateVaultUI();
}

/** 数据变更后的局部刷新（创建/重命名/删除/导入后）。 */
export async function refreshAfterMutation() {
  try {
    const nodes = await ctx.sidebar.refreshTree();
    rebuildNoteNames(nodes || state.tree);
    await Promise.all([
      ctx.sidebar.refreshTags(),
      ctx.sidebar.refreshTrash(),
      loadStatusOnly(),
    ].map((p) => Promise.resolve(p).catch(() => {})));
    // 缺失双链样式会随笔记名集合变化，重新渲染当前预览
    if (state.current) {
      ctx.preview.render(ctx.editor.getValue(), { path: state.current.path, title: state.current.title });
    }
    updateVaultUI();
  } catch (err) {
    console.warn('[MoNote] 刷新失败', err);
  }
}

async function loadStatusOnly() {
  try {
    const res = await api.call('index_status', {});
    if (res) {
      setStatus('index', {
        building: !!res.building,
        degraded: !!res.degraded,
        done: res.progressDone || 0,
        total: res.progressTotal || 0,
        indexed: res.indexed || 0,
        ftsAvailable: res.ftsAvailable !== false,
      });
    }
  } catch {
    /* ignore */
  }
}

async function loadIndexStatus() {
  await loadStatusOnly();
}

function rebuildNoteNames(nodes) {
  const map = new Map();
  const walk = (list) => {
    for (const node of list || []) {
      if (node.kind === 'folder') {
        walk(node.children);
      } else {
        const title = node.title || stripExt(node.name || basename(node.path));
        const entry = {
          name: title,
          path: node.path,
          title,
          mtime: node.mtime || 0,
        };
        map.set(String(title).trim().toLowerCase(), entry);
        // 双链也允许以文件名（去扩展名）为目标（需求附录 A.3 与后端 name_index/find_by_name 兼容）
        const stem = stripExt(node.name || basename(node.path));
        if (stem && !map.has(stem.toLowerCase())) map.set(stem.toLowerCase(), { ...entry, name: stem });
      }
    }
  };
  walk(nodes);
  state.noteNames = map;
}

async function refreshNotify() {
  try {
    await ctx.notify.refresh();
  } catch {
    /* ignore */
  }
}

function updateVaultUI() {
  const vault = state.vault;
  const chip = document.getElementById('vault-chip');
  if (chip) {
    chip.textContent = vault ? vault.name || basename(vault.root) : '';
    chip.title = vault ? vault.root || '' : '未打开笔记库';
  }
  const statusVault = document.getElementById('status-vault');
  if (statusVault) {
    statusVault.textContent = vault ? (vault.root || vault.name || '') : '未打开笔记库';
    statusVault.title = vault ? vault.root || '' : '';
  }
  const welcome = document.getElementById('welcome-screen');
  if (welcome && vault) welcome.hidden = true;
  document.body.classList.toggle('no-vault', !vault);
}

/** 保存/应用 AppConfig。 */
export function applyConfig(config, opts = {}) {
  if (!config) return;
  state.config = config;
  const ui = config.ui || {};
  state.theme = THEME_CYCLE.includes(ui.theme) ? ui.theme : 'auto';
  applyThemeDom(state.theme);
  const zoom = clamp(Number(ui.zoom) || 1, 1, 2);
  applyZoomDom(Math.round(zoom * 10) / 10);
  const sidebar = ui.sidebarVisible !== false;
  const toolbar = ui.toolbarVisible !== false;
  const preview = ui.previewVisible !== false;
  setPanelVisible('sidebar', sidebar, { persist: false, quiet: true });
  setPanelVisible('toolbar', toolbar, { persist: false, quiet: true });
  setPanelVisible('preview', preview, { persist: false, quiet: true });
  if (ctx.hotkeys) ctx.hotkeys.setFromConfig(config.hotkeys);
  if (!opts.persist) void opts;
}

/** 后端 config:changed 事件。 */
export function handleConfigChanged(config) {
  if (!config) return;
  state.config = config;
  const ui = config.ui || {};
  if (ui.theme && THEME_CYCLE.includes(ui.theme)) {
    state.theme = ui.theme;
    applyThemeDom(ui.theme);
  }
  if (ui.zoom != null) applyZoomDom(Math.round(clamp(Number(ui.zoom) || 1, 1, 2) * 10) / 10);
  if (ui.sidebarVisible != null) setPanelVisible('sidebar', ui.sidebarVisible !== false, { persist: false, quiet: true });
  if (ui.toolbarVisible != null) setPanelVisible('toolbar', ui.toolbarVisible !== false, { persist: false, quiet: true });
  if (ui.previewVisible != null) setPanelVisible('preview', ui.previewVisible !== false, { persist: false, quiet: true });
  if (config.hotkeys && ctx.hotkeys) ctx.hotkeys.setFromConfig(config.hotkeys);
  emit('config-changed', config);
}

// ===========================================================================
// 笔记
// ===========================================================================
/**
 * 打开笔记。
 * @param {string} path
 * @param {{focus?:boolean, line?:number}} [opts]
 */
export async function openNote(path, opts = {}) {
  if (!path) return;
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  if (state.current && state.current.path === path && opts.line == null) {
    if (opts.focus !== false) ctx.editor.focus();
    return;
  }
  const saved = await ctx.editor.flush();
  if (!saved) {
    const proceed = await confirmDialog({
      title: '切换笔记',
      message: '当前笔记还有未保存的修改，且上次保存失败。仍要切换到其它笔记吗？',
      detail: '未保存的内容可能在切换后无法恢复。',
      confirmLabel: '仍然切换',
      danger: true,
    });
    if (!proceed) return;
  }
  try {
    const note = await api.call('vault_read', { path });
    if (!note || !note.path) throw new Error('笔记内容为空');
    ctx.editor.setNote(note, { focus: opts.focus !== false, line: opts.line });
    ctx.sidebar.setActiveNote(path);
    rememberRecent(path);
    document.title = `${note.title || stripExt(basename(path))} · MoNote`;
    setState({ activeTag: state.activeTag });
  } catch (err) {
    toast.error(`打开笔记失败：${api.friendlyError(err)}`);
    ctx.notify.pushLocal({ level: 'error', title: '打开笔记失败', detail: `${path}：${api.friendlyError(err)}` });
  }
}

/** 新建笔记（带命名对话框）。 */
export async function createNote(opts = {}) {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  const parent = opts.parent || '';
  const name = await promptDialog({
    title: parent ? `新建笔记（${parent}/）` : '新建笔记',
    label: '笔记名称',
    value: '未命名笔记',
    placeholder: '例如：第3章-内存管理',
    help: '名称中的特殊字符会被自动处理；同名时后端会自动追加序号。',
    confirmLabel: '创建并打开',
    validate: (value) => {
      if (!value) return '名称不能为空';
      if (value.length > 120) return '名称不能超过 120 个字符';
      if (/[\\/]/.test(value)) return '名称不能包含斜杠，请使用文件夹组织层级';
      return null;
    },
  });
  if (!name) return;
  try {
    const res = await api.call('vault_create_note', { name, parent });
    await refreshAfterMutation();
    if (res && res.path) {
      await openNote(res.path, { focus: true });
      toast.success(`已创建笔记「${name}」`);
    }
  } catch (err) {
    toast.error(`新建笔记失败：${api.friendlyError(err)}`);
  }
}

/** 新建文件夹。 */
export async function createFolder(opts = {}) {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  const parent = opts.parent || '';
  const name = await promptDialog({
    title: parent ? `新建文件夹（${parent}/）` : '新建文件夹',
    label: '文件夹名称',
    value: '新建文件夹',
    confirmLabel: '创建',
    validate: (value) => {
      if (!value) return '名称不能为空';
      if (/[\\/]/.test(value)) return '名称不能包含斜杠；如需嵌套请在文件夹中继续新建';
      if (/^[.\s]/.test(value)) return '名称不能以点或空格开头';
      return null;
    },
  });
  if (!name) return;
  try {
    const path = joinPath(parent, name);
    await api.call('vault_create_folder', { path });
    await refreshAfterMutation();
    toast.success(`已创建文件夹「${path}」`);
  } catch (err) {
    toast.error(`新建文件夹失败：${api.friendlyError(err)}`);
  }
}

/** 重命名/移动节点。 */
export async function renameNode(node) {
  if (!node) return;
  const affectsCurrent = state.current && (state.current.path === node.path || (node.kind === 'folder' && state.current.path.startsWith(`${node.path}/`)));
  if (affectsCurrent) {
    await ctx.editor.flush();
  }
  const oldName = node.kind === 'note'
    ? stripExt(node.name || basename(node.path))
    : (node.name || basename(node.path));
  const name = await promptDialog({
    title: node.kind === 'note' ? '重命名笔记' : '重命名文件夹',
    label: '新名称',
    value: oldName,
    confirmLabel: '重命名',
    help: node.kind === 'folder' ? '文件夹内笔记的相对链接会自动更新（FR-201）' : '如有其它笔记通过双链引用本笔记，链接会自动更新',
    validate: (value) => {
      if (!value) return '名称不能为空';
      if (/[\\/]/.test(value)) return '名称不能包含斜杠';
      return null;
    },
  });
  if (!name || name === oldName) return;
  const ext = node.kind === 'note' ? (extname(node.path) || '.md') : '';
  const newPath = joinPath(dirname(node.path), `${name}${ext}`);
  try {
    const res = await api.call('vault_rename', { path: node.path, newPath, rewriteLinks: true });
    const rewrittenCount = res && Array.isArray(res.rewritten)
      ? res.rewritten.reduce((sum, item) => sum + (item.count || 0), 0)
      : 0;
    await refreshAfterMutation();
    // 当前打开的笔记路径跟随重命名
    if (state.current) {
      if (state.current.path === node.path && node.kind === 'note') {
        await openNote(res && res.newPath ? res.newPath : newPath, { focus: false });
      } else if (node.kind === 'folder' && state.current.path.startsWith(`${node.path}/`)) {
        const target = `${newPath}/${state.current.path.slice(node.path.length + 1)}`;
        await openNote(target, { focus: false });
      }
    }
    toast.success(`已重命名${rewrittenCount ? `，并更新 ${rewrittenCount} 处双链` : ''}`);
  } catch (err) {
    toast.error(`重命名失败：${api.friendlyError(err)}`);
  }
}

/** 移入回收站（确认后）。 */
export async function trashNode(node) {
  if (!node) return;
  const isFolder = node.kind === 'folder';
  let childCount = 0;
  if (isFolder) {
    const countNotes = (n) => {
      let count = 0;
      for (const child of n.children || []) {
        if (child.kind === 'folder') count += countNotes(child);
        else count += 1;
      }
      return count;
    };
    childCount = countNotes(node);
  }
  const ok = await confirmDialog({
    title: isFolder ? '删除文件夹' : '删除笔记',
    message: isFolder
      ? `将文件夹「${node.name || node.path}」（含 ${childCount} 篇笔记）移入回收站？`
      : `将笔记「${node.title || node.name || node.path}」移入回收站？`,
    detail: '内容会进入回收站，30 天后自动永久清除；在此之前可以还原。',
    confirmLabel: '移入回收站',
    cancelLabel: '取消',
    danger: true,
  });
  if (!ok) return;
  try {
    await api.call('vault_trash', { path: node.path });
    const affectsCurrent = state.current && (state.current.path === node.path || (isFolder && state.current.path.startsWith(`${node.path}/`)));
    if (affectsCurrent) ctx.editor.clearNote();
    await refreshAfterMutation();
    toast.success('已移入回收站');
  } catch (err) {
    toast.error(`删除失败：${api.friendlyError(err)}`);
  }
}

// ===========================================================================
// 导入 / 导出
// ===========================================================================
/** 导入单文件或文件夹。 */
export async function importNotes(mode = 'file') {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  try {
    const dialog = await api.call('app_open_dialog', {
      mode,
      title: mode === 'folder' ? '选择要导入的文件夹' : '选择要导入的 Markdown 文件',
      filters: mode === 'folder' ? undefined : ['md', 'markdown'],
    });
    if (!dialog || !dialog.path) return;
    const report = await api.call('io_import', { srcPath: dialog.path });
    await refreshAfterMutation();
    showImportReport(report, dialog.path);
  } catch (err) {
    if (isCancelError(err)) {
      toast.info('已取消导入');
      return;
    }
    toast.error(`导入失败：${api.friendlyError(err)}`);
  }
}

function showImportReport(report, srcPath) {
  if (!report) return;
  const skipped = Array.isArray(report.skipped) ? report.skipped : [];
  const transcoded = Array.isArray(report.transcoded) ? report.transcoded : [];
  const content = document.createElement('div');
  const p = document.createElement('p');
  p.textContent = `已从 ${srcPath} 导入 ${report.imported} 个文件。`;
  content.appendChild(p);
  if (transcoded.length) {
    const t = document.createElement('p');
    t.className = 'settings-inline-warn';
    t.textContent = `已转码导入（GB18030→UTF-8）${transcoded.length} 个：${transcoded.join('、')}`;
    content.appendChild(t);
  }
  if (skipped.length) {
    const title = document.createElement('p');
    title.className = 'settings-inline-error';
    title.textContent = `跳过 ${skipped.length} 个文件：`;
    content.appendChild(title);
    const pre = document.createElement('pre');
    pre.className = 'confirm-detail';
    pre.textContent = skipped.map((s) => `${s.file}：${s.reason}`).join('\n');
    content.appendChild(pre);
  }
  const modal = openModal({
    title: '导入完成',
    content,
    size: 'md',
    footer: [button('知道了', { variant: 'primary', onClick: () => modal.close() })],
  });
  toast.success(`导入完成：${report.imported} 个文件${transcoded.length ? `，${transcoded.length} 个转码` : ''}${skipped.length ? `，${skipped.length} 个跳过` : ''}`);
}

/** 导出当前笔记。fmt = md | html | pdf（pdf 走前端打印）。 */
export async function exportNote(fmt = 'md') {
  if (!state.current) {
    toast.info('请先打开一篇笔记');
    return;
  }
  const note = state.current;
  const savedOk = await ctx.editor.flush();
  if (!savedOk) {
    const proceed = await confirmDialog({
      title: '导出笔记',
      message: '当前笔记尚未保存成功，导出的内容可能是编辑区中的最新版本。继续导出？',
      confirmLabel: '继续导出',
      danger: false,
    });
    if (!proceed) return;
  }
  const content = ctx.editor.getValue();
  try {
    if (fmt === 'pdf') {
      const html = await ctx.preview.buildExportHtml({ content, title: note.title, path: note.path });
      await printHtml(html);
      toast.success('已打开打印对话框：请选择「另存为 PDF」并确认页面设置（默认 A4）');
      return;
    }
    const patch = { path: note.path, fmt };
    if (fmt === 'html') {
      patch.renderedHtml = await ctx.preview.buildExportHtml({ content, title: note.title, path: note.path });
    }
    const res = await api.call('io_export_note', patch);
    toast.success(`已导出到 ${res && res.outPath ? res.outPath : '所选位置'}${res && res.size ? `（${formatSize(res.size)}）` : ''}`);
  } catch (err) {
    if (isCancelError(err)) {
      toast.info('已取消导出');
      return;
    }
    toast.error(`导出失败：${api.friendlyError(err)}`);
  }
}

/** 整库导出 ZIP。 */
export async function exportVault() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  if (state.current) await ctx.editor.flush();
  try {
    const res = await api.call('io_export_vault', {});
    toast.success(`整库已导出到 ${res && res.outPath ? res.outPath : '所选位置'}${res && res.size ? `（${formatSize(res.size)}）` : ''}`);
  } catch (err) {
    if (isCancelError(err)) {
      toast.info('已取消导出');
      return;
    }
    toast.error(`整库导出失败：${api.friendlyError(err)}`);
  }
}

function isCancelError(err) {
  const msg = String((err && (err.message || err.msg)) || '');
  return !!err && (err.code === 1005 || err.code === 1001) && /取消|cancel/i.test(msg);
}

function printHtml(html) {
  return new Promise((resolve, reject) => {
    const iframe = document.createElement('iframe');
    iframe.setAttribute('aria-hidden', 'true');
    iframe.title = 'MoNote 打印';
    iframe.style.cssText = 'position:fixed;right:0;bottom:0;width:1px;height:1px;border:0;visibility:hidden;';
    document.body.appendChild(iframe);
    let settled = false;
    const done = (err) => {
      if (settled) return;
      settled = true;
      setTimeout(() => iframe.remove(), 1500);
      if (err) reject(err);
      else resolve();
    };
    const timer = setTimeout(() => done(new Error('打印页面加载超时')), 8000);
    iframe.addEventListener('load', () => {
      clearTimeout(timer);
      setTimeout(() => {
        try {
          iframe.contentWindow.focus();
          iframe.contentWindow.print();
          done();
        } catch (err) {
          done(err);
        }
      }, 350);
    }, { once: true });
    const doc = iframe.contentDocument;
    if (!doc) {
      clearTimeout(timer);
      done(new Error('无法创建打印文档'));
      return;
    }
    doc.open();
    doc.write(html);
    doc.close();
  });
}

// ===========================================================================
// 备份 / 同步 / 索引 / 更新
// ===========================================================================
export async function backupNow() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  manualBackupRunning = true;
  setStatus('backup', { running: true });
  try {
    const res = await api.call('backup_now', {});
    setStatus('backup', { running: false, lastFile: res.file, lastAt: Date.now() });
    toast.success(`备份完成：${res.file}（${formatSize(res.size)}，保留 ${res.kept} 份）`);
  } catch (err) {
    setStatus('backup', { running: false });
    toast.error(`备份失败：${api.friendlyError(err)}`);
    ctx.notify.pushLocal({ level: 'error', title: '备份失败', detail: api.friendlyError(err) });
  } finally {
    manualBackupRunning = false;
  }
}

/**
 * 标签归位：把正文行内 #标签 统一迁移到文件头 front-matter 的 tags 字段（FR-202 单一来源约定）。
 * 先预览 → 确认 → 自动备份 → 应用；当前笔记会重新载入，避免旧缓冲覆盖归位结果。
 */
export async function normalizeTags() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return false;
  }
  try {
    if (state.current) await ctx.editor.flush();
    const preview = await api.call('tag_normalize', { apply: false });
    const changed = (preview && preview.changed) || [];
    if (!changed.length) {
      toast.info('所有笔记的标签都已在文件头，无需整理');
      return false;
    }
    const detail =
      changed
        .slice(0, 8)
        .map((c) => `${c.path}  ←  ${(c.removedInlineTags || []).map((t) => `#${t}`).join(' ')}`)
        .join('\n') + (changed.length > 8 ? `\n… 共 ${changed.length} 篇` : '');
    const ok = await confirmDialog({
      title: '标签归位（统一到文件头）',
      message: `检测到 ${changed.length} 篇笔记的正文中含有 #标签。将把它们迁移到文件头 front-matter 的 tags 字段，并从正文移除这些 #标签；执行前会自动备份整个笔记库（当前笔记会先保存）。`,
      detail,
      confirmLabel: '开始迁移',
    });
    if (!ok) return false;
    try {
      await api.call('backup_now', {});
    } catch (err) {
      const force = await confirmDialog({
        title: '自动备份失败',
        message: `备份失败：${api.friendlyError(err)}。是否仍继续迁移？（建议先手动备份）`,
        confirmLabel: '继续迁移',
        danger: true,
      });
      if (!force) return false;
    }
    const report = await api.call('tag_normalize', { apply: true });
    const done = (report && report.changed && report.changed.length) || 0;
    const failed = (report && report.failed) || [];
    toast.success(`标签归位完成：${done} 篇笔记已更新${failed.length ? `，${failed.length} 篇跳过` : ''}`);
    if (failed.length) {
      ctx.notify.pushLocal({ level: 'warn', title: '部分笔记已跳过标签归位', detail: failed.join('\n') });
    }
    await refreshAll();
    // 当前笔记可能已被改写：重新读取并载入编辑器，防止旧缓冲区在下次自动保存时覆盖归位结果
    const cur = state.current;
    if (cur) {
      const fresh = await api.callSafe('vault_read', { path: cur.path }, null);
      if (fresh) ctx.editor.setNote(fresh);
    }
    return true;
  } catch (err) {
    toast.error(`标签归位失败：${api.friendlyError(err)}`);
    return false;
  }
}

export async function syncNow() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  let view = null;
  try {
    view = await api.call('sync_config', {});
  } catch {
    view = null;
  }
  if (!view || !view.configured || !view.url) {
    toast.info('尚未配置 WebDAV 同步，请在设置中完成配置');
    openSettings('sync');
    return;
  }
  setStatus('sync', { running: true, phase: 'start', configured: true });
  try {
    const res = await api.call('sync_run', {});
    setStatus('sync', { running: false, phase: '', lastRun: res.lastRun, configured: true });
    toast.success(`同步完成：上传 ${res.up} / 下载 ${res.down} / 冲突 ${res.conflict}${res.failed ? ` / 失败 ${res.failed}` : ''}`);
    if (res.conflict) {
      ctx.notify.pushLocal({
        level: 'warn',
        title: `同步发现 ${res.conflict} 个冲突副本`,
        detail: '冲突文件以 -conflict-<时间戳>.md 形式保留，未丢弃任何一版。',
      });
    }
    await refreshAfterMutation();
  } catch (err) {
    setStatus('sync', { running: false, phase: '' });
    toast.error(`同步失败：${api.friendlyError(err)}`);
    ctx.notify.pushLocal({ level: 'error', title: '同步失败', detail: api.friendlyError(err) });
  }
}

export async function rebuildIndex() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  try {
    await api.call('index_rebuild', {});
    setStatus('index', { building: true, phase: 'rebuild', done: 0, total: 0 });
    toast.info('索引重建已开始，后台进行，可在状态栏查看进度');
  } catch (err) {
    toast.error(`索引重建失败：${api.friendlyError(err)}`);
  }
}

export async function checkUpdate(manual = true) {
  if (!manual) {
    api.callSafe('app_update_check', { manual: false }, null);
    return;
  }
  try {
    const res = await api.call('app_update_check', { manual: true });
    if (res.hasUpdate) {
      await alertDialog({
        title: '发现新版本',
        message: `当前版本 ${res.current}，最新版本 ${res.latest}。`,
        detail: [res.notes || '', res.url ? `下载地址：${res.url}` : '', 'v1 仅提示新版本，不自动下载安装。'].filter(Boolean).join('\n'),
      });
    } else if (res.checked) {
      await alertDialog({ title: '检查更新', message: `已是最新版本（当前 ${res.current}）。` });
    } else {
      await alertDialog({ title: '检查更新', message: `当前版本 ${res.current}。`, detail: '7 天内已检查过，本次未发起网络请求。' });
    }
  } catch (err) {
    await alertDialog({ title: '检查更新', message: '检查失败，请检查网络后重试。', detail: api.friendlyError(err) });
  }
}

// ===========================================================================
// 外观 / 面板
// ===========================================================================
function resolveTheme(theme) {
  if (theme === 'light' || theme === 'dark') return theme;
  try {
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  } catch {
    return 'light';
  }
}

function applyThemeDom(theme) {
  const resolved = resolveTheme(theme);
  state.theme = theme;
  state.resolvedTheme = resolved;
  document.documentElement.setAttribute('data-theme', resolved);
  document.documentElement.style.colorScheme = resolved;
  const btn = document.getElementById('btn-theme');
  if (btn) {
    btn.textContent = theme === 'auto' ? '◐' : theme === 'light' ? '☀' : '☾';
    btn.title = `主题：${THEME_LABELS[theme] || theme}（点击切换）`;
    btn.setAttribute('aria-label', `当前主题：${THEME_LABELS[theme] || theme}，点击切换`);
  }
}

/** 设置主题（auto/light/dark）并持久化。 */
export async function setTheme(theme) {
  const next = THEME_CYCLE.includes(theme) ? theme : 'auto';
  applyThemeDom(next);
  if (state.config) state.config.ui = { ...(state.config.ui || {}), theme: next };
  try {
    const cfg = await api.callSafe('app_set_config', { patch: { ui: { theme: next } } }, null);
    if (cfg) state.config = cfg;
  } catch (err) {
    console.warn('[MoNote] 保存主题失败', err);
  }
  toast.info(`主题：${THEME_LABELS[next]}`);
}

/** 循环切换主题。 */
export async function cycleTheme() {
  const index = THEME_CYCLE.indexOf(state.theme);
  await setTheme(THEME_CYCLE[(index + 1) % THEME_CYCLE.length]);
}

/** 系统主题变化时由 app.js 调用。 */
export function handleSystemThemeChange() {
  if (state.theme === 'auto') applyThemeDom('auto');
}

function applyZoomDom(zoom) {
  const value = clamp(Number(zoom) || 1, 1, 2);
  state.zoom = value;
  document.documentElement.style.fontSize = `${16 * value}px`;
  document.body.classList.toggle('zoom-large', value >= 1.5);
  const zoomInput = document.getElementById('settings-zoom');
  if (zoomInput && document.activeElement !== zoomInput) zoomInput.value = String(Math.round(value * 100));
}

/** 设置界面缩放（1.0–2.0）并持久化（防抖）。 */
export function setZoom(zoom, opts = {}) {
  applyZoomDom(Math.round(clamp(Number(zoom) || 1, 1, 2) * 10) / 10);
  if (state.config) state.config.ui = { ...(state.config.ui || {}), zoom: state.zoom };
  persistZoom();
  if (opts.persist) persistZoom.flush();
}

/** 面板可见性：sidebar / toolbar / preview。 */
export function setPanelVisible(name, visible, opts = {}) {
  if (!['sidebar', 'toolbar', 'preview'].includes(name)) return;
  state.panels[name] = !!visible;
  if (name === 'sidebar') document.body.classList.toggle('sidebar-collapsed', !visible);
  else if (name === 'toolbar') document.body.classList.toggle('toolbar-hidden', !visible);
  else if (name === 'preview') document.body.classList.toggle('preview-collapsed', !visible);
  const map = {
    sidebar: document.getElementById('btn-toggle-sidebar'),
    preview: document.getElementById('btn-toggle-preview'),
    toolbar: null,
  };
  if (map[name]) map[name].setAttribute('aria-expanded', visible ? 'true' : 'false');
  if (name === 'sidebar' && !visible) {
    const active = document.activeElement;
    const sidebar = document.getElementById('sidebar');
    if (sidebar && active && sidebar.contains(active)) {
      const target = document.getElementById('btn-toggle-sidebar') || document.getElementById('editor');
      if (target) target.focus();
    }
  }
  if (state.config) {
    const key = name === 'sidebar' ? 'sidebarVisible' : name === 'toolbar' ? 'toolbarVisible' : 'previewVisible';
    state.config.ui = { ...(state.config.ui || {}), [key]: !!visible };
  }
  if (opts.persist !== false) {
    const key = name === 'sidebar' ? 'sidebarVisible' : name === 'toolbar' ? 'toolbarVisible' : 'previewVisible';
    api.callSafe('app_set_config', { patch: { ui: { [key]: !!visible } } }, null).then((cfg) => {
      if (cfg) state.config = cfg;
    }).catch(() => {});
  }
  if (!opts.quiet) {
    const labels = { sidebar: '侧边栏', toolbar: '格式工具栏', preview: '预览区' };
    toast.info(`${labels[name]}已${visible ? '显示' : '隐藏'}`);
  }
}

export function toggleSidebar() {
  setPanelVisible('sidebar', !state.panels.sidebar);
}

export function toggleToolbar() {
  setPanelVisible('toolbar', !state.panels.toolbar);
}

export function togglePreview() {
  setPanelVisible('preview', !state.panels.preview);
}

export function toggleBacklinks() {
  const btn = document.getElementById('btn-backlinks');
  if (btn) btn.click();
}

export function toggleProperties() {
  const btn = document.getElementById('btn-properties');
  if (btn) btn.click();
}

export function toggleOutline() {
  if (!state.panels.preview) setPanelVisible('preview', true, { quiet: true });
  const outline = document.getElementById('outline');
  const isOutline = outline && !outline.hidden;
  ctx.preview.setView(isOutline ? 'preview' : 'outline');
}

export function toggleTheme() {
  return cycleTheme();
}

// ===========================================================================
// 检索 / 设置 / 命令
// ===========================================================================
export function openQuickOpen() {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  ctx.search.openQuick();
}

export function openFullSearch(opts = {}) {
  if (!state.vault) {
    toast.info('请先打开笔记库');
    return;
  }
  ctx.search.openFull(opts);
}

export function openSettings(tab) {
  ctx.settings.open(tab);
}

export function openCommandPalette() {
  ctx.command.open();
}

/** 全文检索：选中下一个结果（快捷键 search.next）。 */
export function searchNextResult() {
  if (ctx.search && typeof ctx.search.focusNextResult === 'function') {
    return ctx.search.focusNextResult();
  }
  return false;
}

export async function runCommand(id) {
  return ctx.command.run(id);
}

export function showOnboarding() {
  ctx.onboarding.open();
}

export async function activateSidebarTab(tab) {
  setPanelVisible('sidebar', true, { quiet: true });
  ctx.sidebar.activateTab(tab);
}

// ===========================================================================
// 最近打开
// ===========================================================================
function loadRecent() {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    const list = raw ? JSON.parse(raw) : [];
    state.recent = Array.isArray(list) ? list.filter((p) => typeof p === 'string').slice(0, 30) : [];
  } catch {
    state.recent = [];
  }
}

function saveRecent() {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(state.recent.slice(0, 30)));
  } catch {
    /* ignore */
  }
}

function rememberRecent(path) {
  if (!path) return;
  state.recent = [path, ...state.recent.filter((p) => p !== path)].slice(0, 30);
  saveRecent();
}

// ===========================================================================
// 事件处理（由 app.js 接线）
// ===========================================================================
export function handleNotesWritten(payload) {
  if (!payload || !payload.path) return;
  if (state.current && state.current.path === payload.path && payload.rev != null) {
    // 保存响应已更新 rev；这里兜底防止事件晚于响应造成倒退
    if (Number(payload.rev) > Number(state.current.rev || 0)) state.current.rev = Number(payload.rev);
  }
  // 引用关系可能变化：刷新反链面板（FR-204 要求 2 秒内更新）
  backlinksRefreshDebounced();
}

export function handleNotesExternal(payload) {
  if (!payload) return;
  ctx.editor.handleExternalChange(payload).catch((err) => console.warn('[MoNote] 处理外部变更失败', err));
  backlinksRefreshDebounced();
  if (payload.kind === 'renamed' || payload.kind === 'deleted' || payload.kind === 'created') {
    refreshAfterMutationDebounced();
  }
}

export function handleIndexProgress(payload) {
  if (!payload) return;
  if (payload.phase === 'done') {
    setStatus('index', { building: false, phase: 'done', done: payload.total || payload.done || 0, total: payload.total || payload.done || 0 });
    loadStatusOnly();
    toast.success('索引重建完成');
  } else {
    setStatus('index', {
      building: true,
      phase: payload.phase || 'rebuild',
      done: payload.done || 0,
      total: payload.total || 0,
    });
  }
}

export function handleSyncProgress(payload) {
  if (!payload) return;
  const running = payload.phase !== 'done' && payload.phase !== 'error';
  setStatus('sync', { running, phase: payload.phase || '', configured: true });
}

export function handleBackupDone(payload) {
  if (!payload) return;
  setStatus('backup', { running: false, lastFile: payload.file, lastAt: Date.now() });
  if (!manualBackupRunning) {
    toast.success(`自动备份完成：${payload.file}`);
  }
}

export async function handleVaultChanged(payload) {
  const reason = payload && payload.reason;
  if (reason === 'opened') return;
  if (reason === 'synced') {
    // 后端只在同步开始时推 sync:progress；同步结束以 vault:changed{synced} 表示
    setStatus('sync', { running: false, phase: '', lastRun: new Date().toISOString(), configured: true });
  }
  if (reason === 'written') {
    // 本应用每次写笔记都会推送；只需刷新目录树（标题可能来自 front-matter），
    // 避免每次自动保存都重扫标签/回收站/预览（NFR-102）。
    refreshTreeOnlyDebounced();
    return;
  }
  refreshAfterMutationDebounced();
}

const refreshTreeOnlyDebounced = debounce(async () => {
  try {
    const nodes = await ctx.sidebar.refreshTree();
    rebuildNoteNames(nodes || state.tree);
  } catch (err) {
    console.warn('[MoNote] 刷新目录树失败', err);
  }
}, 350);

const backlinksRefreshDebounced = debounce(() => {
  if (state.current && ctx.editor) {
    ctx.editor.refreshBacklinks();
  }
}, 350);

const refreshAfterMutationDebounced = debounce(() => {
  refreshAfterMutation().catch(() => {});
}, 250);

export function bindSystemThemeListener() {
  try {
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () => {
      if (state.theme === 'auto') applyThemeDom('auto');
    };
    if (typeof mq.addEventListener === 'function') mq.addEventListener('change', handler);
    else if (typeof mq.addListener === 'function') mq.addListener(handler);
  } catch (err) {
    console.warn('[MoNote] 系统主题监听不可用', err);
  }
}

/** 应用启动后的初始化（绑定主题监听等）。 */
export function initActions() {
  bindSystemThemeListener();
  loadRecent();
  return getActions();
}

function getActions() {
  return {
    bootVault,
    pickVault,
    retryVaultDetect,
    handleVaultOpened,
    refreshAll,
    refreshAfterMutation,
    openNote,
    createNote,
    createFolder,
    renameNode,
    trashNode,
    importNotes,
    exportNote,
    exportVault,
    backupNow,
    normalizeTags,
    syncNow,
    rebuildIndex,
    checkUpdate,
    setTheme,
    cycleTheme,
    setZoom,
    setPanelVisible,
    toggleSidebar,
    toggleToolbar,
    togglePreview,
    toggleBacklinks,
    toggleProperties,
    toggleOutline,
    toggleTheme,
    openQuickOpen,
    openFullSearch,
    openSettings,
    openCommandPalette,
    runCommand,
    searchNextResult,
    showOnboarding,
    activateSidebarTab,
    applyConfig,
    handleConfigChanged,
    handleSystemThemeChange,
    handleNotesWritten,
    handleNotesExternal,
    handleIndexProgress,
    handleSyncProgress,
    handleBackupDone,
    handleVaultChanged,
    getRecent: () => [...state.recent],
  };
}

/** 动作对象（ctx.actions 使用）。 */
export const actions = getActions();
