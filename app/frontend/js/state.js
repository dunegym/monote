/**
 * 全局状态与事件总线。
 *
 * 设计原则：
 * - 单一可变 state 对象（本文件导出），组件只读；
 * - 修改统一走 setState()，按事件名广播，组件订阅后自渲染；
 * - 简洁的 pub/sub，无第三方依赖。
 */

/** @typedef {{root?:string,name?:string,noteCount?:number,indexOk?:boolean,indexBuilding?:boolean}} VaultState */

const listeners = new Map();

/** 订阅事件，返回取消订阅函数。 */
export function on(event, handler) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event).add(handler);
  return () => off(event, handler);
}

/** 取消订阅。 */
export function off(event, handler) {
  const set = listeners.get(event);
  if (set) set.delete(handler);
}

/** 广播事件；单个 handler 抛错不影响其他订阅者。 */
export function emit(event, payload) {
  const set = listeners.get(event);
  if (set) {
    for (const handler of [...set]) {
      try {
        handler(payload);
      } catch (err) {
        console.error(`[monote] state listener error (${event})`, err);
      }
    }
  }
  const all = listeners.get('*');
  if (all) {
    for (const handler of [...all]) {
      try {
        handler({ event, payload });
      } catch (err) {
        console.error('[monote] state * listener error', err);
      }
    }
  }
}

/** 初始状态。 */
export const state = {
  /** @type {VaultState|null} */
  vault: null,
  /** @type {Array<any>} 目录树根节点 */
  tree: [],
  /** @type {Map<string, {name:string,path:string,title:string,mtime:number}>} 笔记名索引（双链解析用，key=小写） */
  noteNames: new Map(),
  /** 当前笔记 {path,title,content,rev,mtime,tags,created,updated} */
  current: null,
  /** 未保存修改 */
  dirty: false,
  /** saved | saving | unsaved | error */
  saveState: 'saved',
  /** 应用配置（AppConfig 结构，camelCase） */
  config: null,
  /** 解析后的主题：light | dark */
  resolvedTheme: 'light',
  /** 主题设置：auto | light | dark */
  theme: 'auto',
  /** 缩放 1.0 - 2.0 */
  zoom: 1,
  /** 面板可见性 */
  panels: {
    sidebar: true,
    toolbar: true,
    preview: true,
    properties: false,
    backlinks: false,
    outline: false,
  },
  /** 侧边栏活动 tab：files | tags | trash | search */
  sidebarTab: 'files',
  /** 标签筛选（选中标签） */
  activeTag: null,
  /** 文件夹筛选（全文检索用） */
  activeFolder: null,
  /** 后台任务状态 */
  status: {
    index: { building: false, degraded: false, done: 0, total: 0, phase: '', indexed: 0, ftsAvailable: true },
    sync: { running: false, phase: '', lastRun: null, configured: false },
    backup: { running: false, lastFile: null, lastAt: null },
  },
  /** 通知：{items, unread} */
  notify: { items: [], unread: 0 },
  /** 最近打开的笔记路径（localStorage 持久化） */
  recent: [],
  /** 是否处于 Mock 演示模式 */
  mock: false,
  /** 启动/加载状态 */
  booting: true,
  /** 全局错误横幅等 UI 提示 */
  banner: null,
};

/**
 * 合并状态并广播。
 * @param {Partial<typeof state>} patch
 * @param {string|string[]} [events] 额外广播的事件名
 */
export function setState(patch, events) {
  if (!patch) return;
  Object.assign(state, patch);
  emit('change', patch);
  if (events) {
    for (const evt of Array.isArray(events) ? events : [events]) emit(evt, patch);
  }
}

/** 修改嵌套面板状态并广播 panels。 */
export function setPanel(name, visible) {
  if (!(name in state.panels)) return;
  state.panels[name] = !!visible;
  emit('panels', { [name]: !!visible });
  emit('change', { panels: state.panels });
}

/** 修改指定后台任务状态（浅合并）并广播。 */
export function setStatus(kind, patch) {
  const cur = state.status[kind];
  if (!cur) return;
  Object.assign(cur, patch || {});
  emit('status', { kind, value: cur });
  emit('change', { status: state.status });
}

/** 设置保存状态并广播 save-state。 */
export function setSaveState(saveState, dirty) {
  state.saveState = saveState;
  if (typeof dirty === 'boolean') state.dirty = dirty;
  emit('save-state', { saveState: state.saveState, dirty: state.dirty });
  emit('change', { saveState: state.saveState, dirty: state.dirty });
}

/** 设置当前笔记并广播 note。 */
export function setCurrent(note) {
  state.current = note;
  emit('note', note);
  emit('change', { current: note });
}

/** 重置某个库相关的数据（切换库时）。 */
export function resetVaultData() {
  state.tree = [];
  state.noteNames = new Map();
  state.current = null;
  state.dirty = false;
  state.saveState = 'saved';
  state.activeTag = null;
  state.activeFolder = null;
  state.notify = { items: [], unread: 0 };
  state.status.index = { building: false, degraded: false, done: 0, total: 0, phase: '', indexed: 0, ftsAvailable: true };
  emit('tree', { nodes: [] });
  emit('note', null);
  emit('notify', state.notify);
  emit('change', { tree: [], current: null });
}
