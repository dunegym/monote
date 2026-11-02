# MoNote 前端（WebView）

MoNote（墨记）的 WebView 前端：纯静态 HTML/CSS/ES Module，由 Tauri 2 直接加载 `frontend/`（无打包器、无框架、无运行期 CDN 请求）。

- 需求基线：`monote/docs/01-需求分析文档.md`（v1.1）
- 设计文档：`exp02/docs/01-软件设计文档.md`
- 实现合同：`monote/app/CONTRACT.md` 第 5/6 节（IPC 与前端要求）
- 本目录所有权：`monote/app/frontend/**`（后端 Rust 不在本目录）

---

## 1. 运行方式

### 1.1 桌面应用（真实功能）

```powershell
cd monote\app\src-tauri
cmd /c "call E:\IDE\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat >nul && cargo run"
```

Tauri 配置 `withGlobalTauri=true`，前端通过 `window.__TAURI__.core.invoke(...)` / `window.__TAURI__.event.listen(...)` 与 Rust 后端通信。

### 1.2 浏览器静态预览（Mock 演示模式）

浏览器禁止 `file://` 加载 ES Module，因此不能直接双击 `index.html`。使用内置静态服务器：

```powershell
node monote/app/frontend/serve.mjs        # http://127.0.0.1:8099
node monote/app/frontend/serve.mjs 8123   # 指定端口
```

未检测到 `window.__TAURI__` 时，`js/api.js` 会打印警告并自动切换到 `js/mock.js` 的 Mock 后端（数据保存在 localStorage，仅用于 UI 演示与自测，**真实功能以 Tauri 为准**）。

Mock 演示辅助参数：

| URL 参数 | 作用 |
| --- | --- |
| `?reset=1` | 重置 Mock 数据（重建示例笔记库/配置/通知） |
| `?novault=1` | 模拟「尚未打开笔记库」，展示欢迎页与首次引导流程 |

Mock 数据也可在「设置 → 关于 → 重置演示数据」中重置。`window.MoNoteDebug`（仅 Mock 模式）暴露 `state / ctx / actions` 便于调试。

---

## 2. 目录结构

```
frontend/
├── index.html            应用外壳（三栏布局、状态栏、欢迎页、toast/overlay 挂载点）
├── styles.css            全部样式：主题变量、三栏布局、Markdown 排版、组件、响应式/缩放
├── serve.mjs             本地静态服务器（仅开发演示）
├── README.md             本文件
├── vendor/               第三方库（离线，无 CDN）
│   ├── marked.umd.js     marked 18.1.2（MIT）
│   ├── katex/            KaTeX 0.19.1（MIT，含字体）
│   ├── highlight/        highlight.js 11.12.0（BSD-3-Clause）
│   └── LICENSES.md
└── js/
    ├── app.js            入口：初始化组件、命令注册、事件接线、状态栏、分隔条、菜单
    ├── api.js            IPC 封装：invoke → envelope 解包 / ApiError / 事件订阅 / Mock 降级
    ├── mock.js           Mock 后端（全部 IPC 命令 + 事件总线 + 示例数据）
    ├── state.js          全局状态 + 发布订阅事件总线
    ├── ctx.js            运行时上下文（组件之间解耦，避免循环依赖）
    ├── actions.js        业务动作层（打开/新建/重命名/删除/导入导出/备份同步/主题/面板…）
    ├── util.js           通用工具（防抖、路径、front-matter、高亮偏移、格式化…）
    ├── dom.js            DOM 创建、焦点陷阱、事件委托
    ├── toast.js          内联提示（info/success/warn/error）
    ├── modal.js          模态框 / 确认框 / 输入框（焦点陷阱、Esc、层叠）
    ├── editor.js         编辑器：自动保存、格式工具栏、图片粘贴、[[ 补全、属性/反链面板
    ├── preview.js        Markdown 渲染：marked + KaTeX + highlight.js、双链、图片、大纲、导出 HTML
    ├── sidebar.js        侧边栏：文件树 / 标签 / 回收站 / 检索入口
    ├── search.js         Ctrl+P 快速打开 + Ctrl+Shift+F 全文检索（过滤/排序/高亮/分页）
    ├── command.js        命令面板（注册表、模糊过滤、键盘操作）
    ├── settings.js       设置页（外观/编辑器/备份/同步/更新/快捷键/关于）
    ├── notify.js         通知中心（未读角标、历史、实时插入、已读）
    ├── hotkeys.js        全局快捷键管理 + 默认表 + 冲突检测/规范化
    └── onboarding.js     首次启动 3 步引导（localStorage 标记）
```

数据流：`组件 → ctx.actions（业务） → api.call(IPC) → envelope → state/事件 → 组件重渲染`。
组件间不直接 import 彼此，统一从 `ctx.js` 取运行时句柄。

---

## 3. 与后端的 IPC 契约（按 CONTRACT.md 第 5 节）

- 调用：`window.__TAURI__.core.invoke('vault_write', { path, content, expectRev })`（入参 camelCase）；
- 返回：`{ok:true,data}` / `{ok:false,error:{code,msg,detail?}}` → `api.call()` 解包，失败抛 `ApiError`（含错误码友好文案）；
- 事件监听命令：`window.__TAURI__.event.listen(name, cb)`。

前端使用的命令（与实现对齐）：

`vault_info`、`vault_open`、`vault_pick`、`vault_list`、`vault_read`、`vault_write`、`vault_rename`、`vault_trash`、`vault_trash_list`、`vault_trash_restore`、`vault_empty_trash`、`vault_create_note`、`vault_create_folder`、`vault_create_empty`、`vault_save_attachment`、`vault_image_data`、`link_suggest`、`link_backlinks`、`tag_list`、`search_query`、`index_rebuild`、`index_status`、`io_import`、`io_export_note`、`io_export_vault`、`backup_now`、`backup_list`、`sync_config`、`sync_configure`、`sync_run`、`app_get_config`、`app_set_config`、`app_update_check`、`notify_list`、`notify_mark_read`、`hotkey_set`、`app_open_dialog`、`app_info`。

前端监听的事件：

`notes:written`、`notes:external`、`index:progress`、`sync:progress`、`backup:done`、`notify:new`、`vault:changed`、`vault:opened`、`config:changed`。

启动流程：`app_get_config` → `vault_info`（null 则显示欢迎页）→ 打开库后拉取目录树/标签/回收站/索引状态/通知；同时监听 `vault:opened`，后端异步恢复库完成后自动加载。

### 3.1 前端对实现细节的适配（联调记录）

| 项 | 说明 |
| --- | --- |
| `search_query.page` | 后端为 **0 开始** 的分页（`state.search` 直接透传），前端按 page=0 首页、pageSize=50 请求。 |
| `search_query` 空关键词 | 后端返回空结果集（不导出全库）；前端空输入时显示「输入关键词开始检索」提示。标签面板筛选改用 `tag:标签名` 查询（后端对纯 tag 查询走扫描）。 |
| `sync_configure` | 后端额外支持 `autoOnStart`，前端已传。 |
| `io_import` | 后端要求显式 `srcPath`（不会自行弹选择框），前端统一先 `app_open_dialog` 再 `io_import`。 |
| `io_export_note` `/fmt="pdf"` | 后端拒绝 PDF；前端使用独立的 `iframe + window.print()` 打印管线（A4，图片以 data URL 内嵌）。 |
| `sync:progress` | 后端只在同步开始时推送 `start`，结束时推 `vault:changed{reason:"synced"}`；前端据此结束「同步中」状态。 |
| `vault:changed` | 后端在一次写入/创建/重命名后都会推送；前端对 `reason` 分流：`written` 仅防抖刷新目录树（350ms），其它原因做 250ms 防抖的全量局部刷新（树/标签/回收站/预览），避免自动保存期间频繁重扫。 |
| `hotkey_set` | 后端允许任意命令 id（不限于 11 个默认键）；前端额外定义了 11 个高频命令默认键（大纲/反链/主题/导入导出/备份/同步/面板切换），首次自定义后写入 `config.hotkeys`。 |
| `app_info` | 后端额外返回 `noteCount`，前端「关于」页有则展示。 |
| `vault_create_empty` / 双链目标 | 后端以**展示标题**（优先 front-matter title）作为链接目标，同时兼容文件名（去扩展名）；前端 `noteNames` 同时登记两者，避免标题与文件名不一致时误判「缺失链接」。 |

---

## 4. 第三方库（vendor，离线）

| 库 | 版本 | 文件 | 许可证 |
| --- | --- | --- | --- |
| marked | 18.1.2 | `vendor/marked.umd.js` | MIT |
| KaTeX | 0.19.1 | `vendor/katex/katex.min.js`、`katex.min.css`、`fonts/*` | MIT |
| highlight.js | 11.12.0 | `vendor/highlight/highlight.min.js`（common languages） | BSD-3-Clause |

详细来源与下载命令见 `vendor/LICENSES.md`。全部为本地文件，页面首屏不发起任何网络请求（NFR-108）。KaTeX 字体保留 ttf/woff/woff2 以兼容各平台 WebView。

---

## 5. 功能清单（对照 CONTRACT 第 6 节 / UR-01~08）

- **三栏 UI（UR-01）**：侧边栏（文件 / 标签 / 回收站 / 检索入口 Tabs）｜编辑区（标题栏 + 属性面板 + 格式工具栏 + textarea）｜预览区（预览 / 大纲切换）。三栏均可折叠（`Ctrl+\`、`Ctrl+Shift+T`、`Ctrl+Shift+M`、侧栏按钮、预览按钮），中间有可拖拽分隔条（双击复位）。
- **状态栏**：字数（不含空白）、保存状态（已保存/保存中/未保存/保存失败）、索引状态与重建进度条、同步状态、备份状态、当前库路径、Mock 标识；索引/备份/同步后台任务不阻塞编辑（UR-07）。
- **编辑与实时预览（FR-101/102）**：输入防抖 150ms 渲染；GFM 表格、任务列表、删除线、围栏代码高亮；`$...$` / `$$...$$` KaTeX，语法错误红色内联提示；非法 Markdown 不崩溃；大纲（H1–H6，点击滚动预览并同步编辑器行）。
- **格式工具栏（FR-104）**：加粗/斜体/删除线/H1–H3/无序/有序/任务/引用/行内代码/代码块/链接/表格/公式/分隔线；可隐藏（`Ctrl+Shift+T`）。
- **自动保存（FR-105）**：输入停顿（默认 2000ms，可在设置改）、切换笔记前 flush、窗口失焦、页面隐藏、`beforeunload` 尽力保存；`Ctrl+S` 手动保存；修订号冲突（2004）弹「重新加载 / 覆盖保存 / 取消」；保存失败保留编辑区内容并每 30s 重试。
- **图片粘贴（FR-103）**：paste/drag&drop 图片 → 校验格式与 ≤10MB → `vault_save_attachment` → 插入 `![](<attachments/...>)`；预览经 `vault_image_data` 换 data URL（加载中占位、失败样式）。
- **front-matter（附录 A.2）**：属性面板编辑 `title` / `tags`，保留未知字段；`created/updated` 展示并以后端维护为准。
- **双链与反链（FR-203/204）**：输入 `[[` 弹出补全（≤10 项，后端按修改时间排序，方向键 + 回车选择）；支持 `[[目标|别名]]`、`[[#标题]]`；缺失目标红色样式，点击确认后 `vault_create_empty` 创建并跳转；反链面板显示来源标题/行号/上下文，点击跳到源笔记并选中对应行；`notes:written`/`notes:external` 后 350ms 刷新反链（≤2s）。
- **文件夹树（FR-201）**：目录树渲染、展开/折叠、新建笔记/文件夹、重命名/移动（后端改写链接）、删除（确认后进回收站）、刷新；支持方向键/Home/End/Enter 键盘导航与「更多操作」菜单。
- **标签（FR-202）**：标签面板聚合 front-matter 与行内标签计数，点击筛选（`tag:` 查询）并列出结果。
- **检索（FR-301/302/303）**：`Ctrl+P` 标题快速打开（最近打开优先、方向键 + 回车）；`Ctrl+Shift+F` 全文检索（多关键词 AND、`"精确短语"`、`tag:` 语法提示，文件夹/标签/时间[今天/7天/30天/自定义]过滤，相关度/修改时间排序，snippet 按 `hlOffset/hlLen` 高亮，50 条/页「加载更多」，降级时提示）。
- **回收站（FR-205）**：列表显示原路径/大小/删除时间/剩余天数（≤7 天红色警示），还原（同名自动改名时提示），清空（危险确认、确认按钮非默认焦点）。
- **导入导出（FR-401~403）**：导入 .md/文件夹（后端返回转码/跳过报告，前端弹报告）；导出 Markdown（后端保存框）、HTML（前端把图片内嵌为 data URL 后交给后端写文件）、PDF（前端打印管线）、整库 ZIP。
- **备份与同步（FR-502/503）**：设置页配置备份目录/保留份数、立即备份、备份历史；WebDAV 地址/账号/密码（密码不显示、不落 config）、自动同步、立即同步、非 HTTPS 警告、同步结果与冲突提示；断网失败提示且本地数据不受影响。
- **设置（FR-602/604/606/605）**：主题 auto/light/dark、缩放 100%–200%、自动保存间隔、工具栏/侧栏/预览显隐、备份、同步、更新检查（手动/自动开关、失败提示）、快捷键自定义（录制、冲突提示 1004、重置）、关于信息。
- **命令面板（FR-604）**：`Ctrl+Shift+P`，40+ 命令覆盖新建/保存/属性/反链/删除/导入/导出/整库导出/备份/同步/调色/主题/各面板/检索/设置/更新/引导等；模糊过滤 + 分组 + 快捷键提示 + 键盘执行。
- **通知中心（FR-504）**：铃铛 + 未读角标，历史列表（后端上限 100），`notify:new` 实时插入，info/warn/error 样式，全部已读/单条已读；前端后台任务错误（备份/同步等）也写入该面板。
- **首次引导（UR-02）**：3 步（新建 → 双链 → 检索），可跳过，`localStorage('monote.onboardingDone')` 与 `config.onboardingDone` 双标记。
- **无障碍（UR-08）**：控件 aria-label/role、模态/浮层焦点陷阱、Esc 关闭、破坏性操作默认焦点在「取消」、可见焦点环、树/标签页方向键导航、对话框 `aria-modal`。
- **缩放（UR-08/NFR-111）**：`document.documentElement` 字号驱动 rem 布局；200% 时切换 `zoom-large` 紧凑规则（隐藏品牌副标题/库 chip、工具栏横向滚动、状态栏换行、侧栏收窄），三栏均不截断、可折叠；应用根节点 `position:fixed` 防止页面整体滚动。
- **主题（FR-602）**：CSS variables；auto 监听 `prefers-color-scheme` 实时切换；亮/暗主题正文对比度按 WCAG AA 取色。
- **错误处理**：所有 `invoke` 失败经 toast/内联错误展示（不静默）；后台错误进通知中心；保存冲突/只读/网络错误给出可操作提示。

---

## 6. 手工走查清单（联调用）

> 建议用 `cargo run` 启动真实窗口；以下每项均可在 UI 中直接观察。

### 6.1 启动与库

- [ ] 首次启动（无库）：显示欢迎页；点「选择 / 新建笔记库」弹出原生目录选择；取消后仍停留欢迎页。
- [ ] 选择库后：目录树/标签/回收站/通知加载；`vault:opened` 异步恢复也能自动进入主界面；状态栏显示库路径与索引状态。
- [ ] 重启应用：自动恢复上次库；首次启动引导只出现一次（跳过也可）。
- [ ] 无库时按 Ctrl+P / Ctrl+Shift+F：给出「请先打开笔记库」提示，不崩溃。

### 6.2 编辑 / 预览 / 保存

- [ ] 输入文字，预览约 150ms 内更新；停止输入 2s 后状态栏「保存中… → 已保存」；用外部记事本修改该文件后保存，出现 2004 冲突三选一弹窗。
- [ ] 编辑后立即切换笔记，原笔记内容已写入磁盘（切换前 flush）。
- [ ] 编辑后立刻点窗口外（失焦），自动保存；`Ctrl+S` 立即保存。
- [ ] 工具栏每个按钮生效（加粗/斜体/标题/列表/引用/代码/链接/表格/公式），选中文本时包裹、空选插入占位。
- [ ] GFM：表格、任务列表（可勾选样式）、删除线、围栏代码高亮均正确。
- [ ] KaTeX：`$E=mc^2$` 与 `$$...$$` 渲染；`$E=mc^$` 原位置红色「公式错误」。
- [ ] 大纲：含多级标题时列出，点击滚动预览；长文编辑时大纲同步刷新。
- [ ] 粘贴 2MB PNG：`attachments/` 新增文件、编辑器插入 `![](attachments/...)`、预览显示图片；粘贴 12MB PNG：提示超过 10MB 且不插入链接。

### 6.3 双链 / 反链 / 标签

- [ ] 输入 `[[`：弹出补全（≤10 项）；继续输入过滤；↑↓ 选择、Enter 插入；Esc 关闭。
- [ ] 预览点击存在的 `[[目标]]` 跳转；点击缺失目标弹出确认，创建同名笔记并跳转；`[[#标题]]` 跳到本笔记对应标题。
- [ ] 反链面板：引用当前笔记的笔记/行号/上下文正确；点击跳到源笔记并选中行；在另一篇笔记中增删引用后 ≤2s 面板更新。
- [ ] 属性面板修改标题/标签并应用：front-matter 更新（未知字段保留），保存后目录树/标签面板同步。
- [ ] 标签面板合并行内 `#标签` 与 front-matter `tags`，点击后结果列表正确。

### 6.4 文件树 / 回收站

- [ ] 新建笔记/文件夹、重命名笔记（双链引用被改写）、重命名文件夹（内部链接被改写）、删除笔记/文件夹确认后进回收站。
- [ ] 删除当前正在编辑的笔记后编辑区进入空状态，不报错。
- [ ] 回收站：剩余天数显示（<7 天红色）；还原回原路径；原路径冲突时自动改名并提示；清空需二次确认且默认焦点在「取消」。
- [ ] 文件树键盘：↑↓ 移动、←→ 折叠/展开、Enter 打开/展开。

### 6.5 检索

- [ ] `Ctrl+P`：输入即查；结果最近打开优先；↑↓ + Enter 打开；Esc 关闭。
- [ ] `Ctrl+Shift+F`：`内存 tag:考研` 只返回同时命中的笔记；`"精确短语"` 只返回连续包含的笔记；snippet 高亮位置正确。
- [ ] 过滤：文件夹/标签/今天/7 天/30 天/自定义日期；排序切换相关度/修改时间；结果总数、耗时、降级提示正确。
- [ ] 索引重建期间状态栏显示进度；检索仍可用；索引损坏时显示「索引降级/正在重建」提示。

### 6.6 设置 / 命令 / 通知 / 主题

- [ ] 设置各分页可保存：主题 auto 跟随系统切换（2s 内）、缩放 100%→200% 布局无错乱、自动保存间隔生效、备份目录/份数保存、立即备份与历史刷新。
- [ ] WebDAV：保存配置（密码不回显）、非 HTTPS 警告、立即同步显示上传/下载/冲突统计；断网时提示失败且本地不受影响。
- [ ] 更新检查：手动检查显示结果或「检查失败，请检查网络后重试」；关闭自动检查后 7 天内无请求。
- [ ] 快捷键：把「命令面板」改为 `Ctrl+K` 立即生效、旧键失效；与既有键冲突时行内提示冲突对象（错误码 1004）；重置恢复默认。
- [ ] 命令面板：覆盖所有主要命令，模糊过滤、键盘执行；任意界面状态均可打开。
- [ ] 通知中心：未读角标、历史列表、全部已读、单条已读；运行备份/同步/更新后能看到新通知（若后端发布）。
- [ ] 主题切换：`◐` 按钮循环 auto/light/dark；暗色下全部区域可读、对比度正常。

### 6.7 键盘 / 无障碍 / 缩放

- [ ] 仅键盘完成：新建笔记 → 输入 `[[` 建双链 → Ctrl+P / Ctrl+Shift+F 打开；Tab 焦点顺序符合视觉顺序。
- [ ] Esc 关闭对话框/浮层/通知面板；焦点回到触发控件；对话框 Tab 不逃出。
- [ ] 200% 缩放：三栏、状态栏、工具栏、设置页、对话框均无横向截断；可折叠面板；浏览器窗口缩小到 960×600 仍可操作。
- [ ] 断开鼠标后可完成 S-08 闭环（新建→双链→检索）。

---

## 7. 本地验证结果（frontend-dev 自测）

- **语法检查**：全部 `js/*.js`（ES Module）+ `serve.mjs` 通过 `node --check`（复制为 `.mjs` 后检查，0 失败）。
- **浏览器冒烟（Playwright / `node serve.mjs`，Mock 后端）**：
  - 页面加载 0 个 JS 错误；Mock 警告 1 条（预期）；
  - 打开笔记、Markdown 渲染（表格/任务列表/代码高亮/KaTeX 2 处/大纲 5 项）；
  - 实时预览、2s 自动保存落盘（rev 递增）、切换笔记前 flush；
  - `[[` 补全（10 项、方向键 + Enter 插入）、缺失链接确认创建并跳转；
  - 反链面板列出并跳转；标签面板筛选；回收站还原/清空确认（默认焦点「取消」）；
  - `Ctrl+P` 快速打开（最近优先）、`Ctrl+Shift+F` 全文检索（tag/文件夹/时间过滤、排序、高亮、自定义日期）；
  - 命令面板（40+ 命令、快捷键显示、执行备份）、`Ctrl+Shift+P`；
  - 设置：主题 auto 跟随系统亮/暗切换、缩放 200%、备份/同步/更新/关于、快捷键改 `Ctrl+K` 生效后重置恢复；
  - 通知中心未读/全部已读；导出菜单、导出 md/html（Mock 路径）；导入完成报告；
  - 工具栏/侧边栏/预览栏快捷键显隐；分隔条双击复位；
  - 欢迎页 + 3 步引导（跳过/下一步/开始使用、localStorage 标记）；
  - 200% 缩放截图走查（暗色/亮色）无页面整体滚动、面板内容不截断。

---

## 8. 已知限制与联调注意点（风险）

1. **退出前 flush**：浏览器 `beforeunload` 中无法可靠等待异步 IPC；当前策略为「2s 自动保存 + 失焦/页面隐藏/切换笔记时保存 + `beforeunload` 尽力保存」。若在最后 2s 内强制结束进程，可能丢失极少量输入（仍在 NFR-105 允许的 ≤10s 内）。若需严格 before-quit 保存，建议后端在窗口关闭路径中通过 WebView 侧 `beforeunload`/暂停等待机制协调（或接受当前口径）。
2. **PDF 导出 / HTML 导出**：PDF 走前端 `iframe + window.print()`（设计 DD-6），依赖 WebView2 打印对话框；联调时需实际验证打印内容与 A4 分页。HTML 导出由前端生成 `renderedHtml`（图片以 data URL **完全内嵌**，满足 FR-402/AC-017；KaTeX 样式内联、字体 URL 指向应用安装目录，复制到另一台机器时图片正常、公式排版可能因缺字体而退化；如需完全自包含可后续把 KaTeX 字体也 base64 内嵌）。若后端对 `renderedHtml` 的使用方式有变化需同步。
3. **大库性能**：目录树一次性渲染全量节点（后端 `vault_list` 全量）；1 万篇笔记时树展开/搜索渲染需实测。标签/检索列表均分页（全文 50/页、标题 20/页）。
4. **`vault:changed` 触发频率**：后端每次写入/创建/重命名都会推送，前端做了 250ms 防抖（tree/tags/trash/预览重渲染）。若实测导致编辑输入卡顿，可将该刷新缩减为「仅 tree 变更」或同样防抖更久。
5. **Mock 数据差异**：Mock 的搜索/空关键词/分页行为尽量贴近后端，但仅用于演示；联调一律以 `cargo run` 为准。Mock 仅 persistence 于 localStorage，`?reset=1` 重置。
6. **图标/字体**：使用系统字体与 emoji 图标（无图标字体依赖）；若目标机器缺少 emoji 字体，个别图标显示为方块，不影响功能。
7. **`app_log` 命令**：CONTRACT 为可选命令且未在 `invoke_handler` 注册；前端 `logToBackend` 仅被安全调用（失败静默），未使用。
8. **通知实时性**：`notify:new` 事件由各服务发布；前端收到即插入列表。若后端某些后台任务只写库不发事件，需刷新面板才能看到（可在打开面板时调用 `notify_list` 重新拉取）。
9. **快捷键与 WebView 保留键**：个别组合可能与 WebView2/系统快捷键冲突（如 Ctrl+W/Ctrl+R）；默认表避开；用户自定义时前端会做冲突检测，但无法阻止系统级占用。
10. **文件树拖拽移动**：任务中为可选，本版通过「重命名/移动」对话框实现移动（修改路径），未实现树内拖拽。

---

## 9. 变更边界

- 本目录只改 `frontend/**`；未修改 Rust、`tauri.conf.json`、`capabilities/`。
- 若需新增 IPC 命令或调整返回结构，请先同步 `CONTRACT.md` 第 5 节并通知前端。
