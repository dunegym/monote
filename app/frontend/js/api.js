/**
 * Tauri IPC 封装（对应 CONTRACT.md 第 5 节）。
 *
 * - 真实环境：window.__TAURI__.core.invoke(command, args) → envelope {ok,data}|{ok:false,error}
 * - 非 Tauri 环境（浏览器静态预览）：自动降级到 mock 后端，并 console 警告；
 * - 事件：window.__TAURI__.event.listen(name, cb) / mock 事件总线；
 * - 不静默失败：call() 抛 ApiError，由调用方展示内联 toast / 通知。
 */
import { mockInvoke, mockBus } from './mock.js';

const tauriGlobal = typeof window !== 'undefined' ? window.__TAURI__ : null;
const realInvoke =
  tauriGlobal && tauriGlobal.core && typeof tauriGlobal.core.invoke === 'function'
    ? tauriGlobal.core.invoke.bind(tauriGlobal.core)
    : null;
const realListen =
  tauriGlobal && tauriGlobal.event && typeof tauriGlobal.event.listen === 'function'
    ? tauriGlobal.event.listen.bind(tauriGlobal.event)
    : null;

/** 是否处于 Mock 演示模式。 */
export const isMock = !realInvoke;

if (isMock) {
  // eslint-disable-next-line no-console
  console.warn(
    '[MoNote] 未检测到 window.__TAURI__（当前可能是浏览器静态预览）。已切换到 Mock 演示后端；真实功能以 Tauri 运行为准。',
  );
}

/** IPC 错误对象。 */
export class ApiError extends Error {
  /**
   * @param {number} code
   * @param {string} msg
   * @param {string} [detail]
   */
  constructor(code, msg, detail) {
    super(msg || '未知错误');
    this.name = 'ApiError';
    this.code = Number(code) || 0;
    this.detail = detail || '';
  }
}

const ERROR_HINTS = {
  1001: '路径不合法或超出笔记库范围',
  1002: '笔记或目录不存在，可能已被删除',
  1003: '目标名称已存在，请换一个名称',
  1004: '快捷键与其他命令冲突',
  1005: '参数或操作不受支持',
  2001: '请先打开一个笔记库',
  2002: '读写失败，请检查文件权限或磁盘状态',
  2003: '磁盘空间不足',
  2004: '笔记已被外部修改，请选择重新加载或覆盖保存',
  3001: '索引已降级，检索可能稍慢',
  3002: '检索语法错误，请检查查询写法',
  4001: 'WebDAV 认证失败，请检查账号密码',
  4002: '同步冲突，远端文件已被修改',
  4003: '网络不可达，请检查网络后重试',
  4004: '更新服务不可用',
  9000: '后端命令调用失败（可能尚未实现）',
  9001: '演示模式未支持该操作',
};

/** 将错误转成适合用户阅读的一句话。 */
export function friendlyError(err) {
  if (!err) return '未知错误';
  const code = typeof err.code === 'number' ? err.code : 0;
  const msg = err.message || err.msg || String(err);
  const hint = ERROR_HINTS[code];
  if (!hint) return msg;
  if (!msg || msg === hint) return hint;
  return `${msg}（${hint}）`;
}

function normalizeEnvelope(command, res) {
  if (res && typeof res === 'object' && typeof res.ok === 'boolean') {
    if (res.ok) return res;
    const error = res.error && typeof res.error === 'object' ? res.error : { code: 0, msg: String(res.error || '未知错误') };
    return {
      ok: false,
      error: {
        code: Number(error.code) || 0,
        msg: String(error.msg || '未知错误'),
        detail: error.detail == null ? '' : String(error.detail),
      },
    };
  }
  // 兼容早期后端直接返回 data 的情况（合同要求 envelope，这里仅做宽容处理）
  if (res !== undefined) {
    console.warn(`[MoNote] 命令 ${command} 未返回 envelope，已按成功数据处理（后端需按 CONTRACT.md 返回 {ok,data}）`);
  }
  return { ok: true, data: res };
}

/**
 * 调用命令并返回原始 envelope（不抛异常）。
 * @param {string} command
 * @param {Record<string, any>} [args]
 * @returns {Promise<{ok:true,data:any}|{ok:false,error:{code:number,msg:string,detail:string}}>}
 */
export async function invoke(command, args = {}) {
  if (isMock) {
    try {
      return await mockInvoke(command, args || {});
    } catch (err) {
      return { ok: false, error: { code: 9000, msg: String((err && err.message) || err), detail: '' } };
    }
  }
  try {
    const res = await realInvoke(command, args || {});
    return normalizeEnvelope(command, res);
  } catch (err) {
    const msg = String((err && err.message) || err || 'IPC 调用失败');
    return { ok: false, error: { code: 9000, msg, detail: String(err || '') } };
  }
}

/**
 * 调用命令并解包 data；失败时抛 ApiError。
 * @template T
 * @param {string} command
 * @param {Record<string, any>} [args]
 * @returns {Promise<T>}
 */
export async function call(command, args = {}) {
  const env = await invoke(command, args || {});
  if (!env.ok) {
    throw new ApiError(env.error.code, env.error.msg, env.error.detail);
  }
  return env.data;
}

/**
 * 调用命令，失败时返回 fallback 并输出警告（用于非关键 UI 数据）。
 */
export async function callSafe(command, args = {}, fallback = null) {
  try {
    return await call(command, args || {});
  } catch (err) {
    console.warn(`[MoNote] 命令 ${command} 失败：`, friendlyError(err));
    return fallback;
  }
}

/**
 * 订阅核心事件；返回取消订阅函数（异步，await 后得到）。
 * @param {string} event
 * @param {(payload:any)=>void} handler
 * @returns {Promise<() => void>}
 */
export async function listen(event, handler) {
  if (isMock) {
    const off = mockBus.on(event, handler);
    return () => off();
  }
  if (!realListen) return () => {};
  try {
    const unlisten = await realListen(event, (evt) => {
      try {
        handler(evt && evt.payload !== undefined ? evt.payload : evt);
      } catch (err) {
        console.error(`[MoNote] 事件 ${event} 处理失败`, err);
      }
    });
    return typeof unlisten === 'function' ? unlisten : () => {};
  } catch (err) {
    console.warn(`[MoNote] 订阅事件 ${event} 失败`, err);
    return () => {};
  }
}

/** 事件名常量（与 CONTRACT.md 5 节对齐）。 */
export const EVENTS = {
  NOTES_WRITTEN: 'notes:written',
  NOTES_EXTERNAL: 'notes:external',
  INDEX_PROGRESS: 'index:progress',
  SYNC_PROGRESS: 'sync:progress',
  BACKUP_DONE: 'backup:done',
  NOTIFY_NEW: 'notify:new',
  VAULT_CHANGED: 'vault:changed',
  VAULT_OPENED: 'vault:opened',
  CONFIG_CHANGED: 'config:changed',
};

/** 前端日志（可选，用于后端排查）。 */
export function logToBackend(level, message) {
  invoke('app_log', { level, message }).catch(() => {});
}

/** 便于组件使用的聚合对象。 */
export const api = { call, invoke, callSafe, listen, isMock, friendlyError, EVENTS, logToBackend };
