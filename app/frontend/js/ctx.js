/**
 * 运行时上下文注册表：打破组件之间的循环依赖。
 *
 * 约定：各组件只导出 init()，init 返回自己的 public API；
 * app.js 在启动时把 api/store/actions 与各组件句柄挂到 ctx 上，
 * 组件之间只通过 ctx.xxx 在运行时互相调用（不要在模块顶层解构）。
 *
 * 注意：ctx 中的字段在 app.js 完成初始化前可能为 null，调用前需判空。
 */
export const ctx = {
  /** api 模块聚合对象（call/invoke/listen/...） */
  api: null,
  /** 动作层（actions.js 导出的对象） */
  actions: null,
  /** 组件句柄 */
  editor: null,
  preview: null,
  sidebar: null,
  search: null,
  command: null,
  settings: null,
  notify: null,
  hotkeys: null,
  onboarding: null,
  statusbar: null,
  /** 组件级工具 */
  toast: null,
  modal: null,
};
