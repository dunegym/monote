# MoNote 实现开发合同（内部开发文档）

本文件是 MoNote（墨记）桌面应用**实现阶段**的团队开发合同。所有人（Lead 与各开发成员）共同遵守。

需求与设计的权威来源：

- 需求基线：`../../monote/docs/01-需求分析文档.md`（v1.1，26 FR + 11 NFR + 36 AC）
- 设计文档：`../../exp02/docs/01-软件设计文档.md`（v1.0，含 5 项基线修正 C-1~C-5：29 FR + 40 AC + UR-01~08）
- 本文件：实现层的模块边界、IPC 合同与文件所有权。

## 1. 技术栈

| 层 | 技术 |
| --- | --- |
| 桌面壳 | Tauri 2（Rust 核心 + 系统 WebView2） |
| 核心 | Rust 2021（`src-tauri/src/`），SQLite FTS5 经 `rusqlite`（bundled） |
| 前端 | 原生 HTML/CSS/JS（ES Module，无框架、无打包器），`frontend/` 由 Tauri 直接加载 |
| 测试 | Rust `#[cfg(test)]` 单元测试 + `tempfile` 临时库集成测试；前端手工/冒烟 |

## 2. 构建与验证

MSVC 环境在 `E:\IDE\Microsoft Visual Studio\18\Community`，直接用 `cargo` 前需要 `vcvars64.bat`：

```powershell
cd monote\app\src-tauri
cmd /c "call E:\IDE\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat >nul && cargo check"
cmd /c "call E:\IDE\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat >nul && cargo test"
cmd /c "call E:\IDE\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat >nul && cargo run"
```

（`cargo run` 会弹出 MoNote 窗口，验证 UI 用；CI 只用 check/test。）

## 3. 文件所有权（写入边界，互相不要越界）

| 范围 | 负责人 | 文件 |
| --- | --- | --- |
| 合同/类型/错误 | Lead | `src-tauri/src/lib.rs`、`error.rs`、`types.rs`、`state.rs`、`ipc.rs`、`src/main.rs`、`Cargo.toml`、`build.rs`、`tauri.conf.json`、`capabilities/`、`icons/` |
| M2 库 + M3 链接 | core-dev | `src-tauri/src/core/vault.rs`、`links.rs` |
| M4 检索索引 | search-dev | `src-tauri/src/core/index.rs` |
| M5/M6/M7 服务 | features-dev | `src-tauri/src/core/io.rs`、`backup.rs`、`sync.rs`、`update.rs`、`notify.rs`、`config.rs` |
| 前端 | frontend-dev | `frontend/**`（index.html、styles.css、js/**、vendor/**） |
| 集成/验证/文档 | Lead | `state.rs`、`ipc.rs`、`README.md`、`docs/**`、`tests/**` |

- 已存在的**公开函数签名是合同**：实现可以加私有辅助函数/结构，但不要改已声明公开函数的名称、参数、返回类型；确需调整先在团队任务/消息中说明。
- 新增文件必须属于自己目录；跨范围需求通过 Lead 协调。
- 不写 `unsafe`（确有必要需 Lead 批准）。
- 所有 pub 函数使用 `crate::error::Result` / `AppError`；不 panic（除测试）；不 `unwrap()` 用户输入路径。

## 4. Rust 模块边界

```
src/
├── main.rs            入口（GUI 子系统）
├── lib.rs             run()：Tauri Builder、状态、命令注册、启动流程
├── error.rs           错误码 + envelope（已实现）
├── types.rs           DTO + AppConfig（已实现）
├── state.rs           AppState（全局状态：Vault/Index/Config/Notify/锁）+ 服务编排
├── ipc.rs             #[tauri::command] 全部命令实现（envelope 返回）
└── core/
    ├── vault.rs       M2 库管理（唯一文件写入口）
    ├── links.rs       M3 front-matter/双链/标签（纯函数）
    ├── index.rs       M4 SQLite FTS5 检索
    ├── io.rs          M5 导入导出
    ├── backup.rs      M6 BKP 备份
    ├── sync.rs        M6 SYN WebDAV 同步
    ├── config.rs      M7 config.json
    ├── notify.rs      M7 通知中心
    └── update.rs      M7 版本检查
```

关键依赖方向（设计文档 3.2）：`ipc → state → core::*`；`core` 不依赖 WebView；**任何模块写库内文件都必须经 `Vault::write_atomic`/`create_note` 等 Vault 方法**（备份快照读、导出除外）。

### 数据流（写笔记，F-01）

```
前端 editor --save--> IPC vault_write{path, content, expectRev}
  state::write_note()
    1. links::touch_front_matter(content) 维护 updated/created
    2. Vault::write_atomic() 原子写 + 登记自写白名单(path,hash)
    3. links::parse_note() → SearchIndex::reindex_note()
    4. 更新修订号 rev+1；emit "notes:written"{path,rev}
```

## 5. IPC 合同（前端 ⇄ 核心）

- 命令注册名 = Rust 函数名（snake_case），前端 `window.__TAURI__.core.invoke('vault_open', {...})`。
- 所有命令返回 envelope：`{ok:true,data}` 或 `{ok:false,error:{code,msg,detail?}}`；**不要**用 Promise reject。
- 入参用 camelCase（Tauri 默认映射到 Rust snake_case），例如 `invoke('vault_write',{path, content, expectRev})`。
- 路径参数一律为**库内相对路径**（`/` 分隔），越界返回 1001。

| 命令 | 入参 | 成功 data | 说明/需求 |
| --- | --- | --- | --- |
| `vault_open` | `{rootPath}` | `OpenedVault` | 打开/切换库；切换前 flush 前端保存 |
| `vault_pick` | `{}` | `OpenedVault` \| `null` | 原生目录选择器选库并打开 |
| `vault_info` | `{}` | `VaultInfo` \| `null` | 当前打开的库信息（启动恢复异步，前端据此判断） |
| `vault_list` | `{depth?}` | `{nodes: TreeNode[]}` | 目录树 |
| `vault_read` | `{path}` | `NoteRead` | 读笔记 + rev |
| `vault_write` | `{path, content, expectRev?}` | `WriteResult` | 原子写；rev 冲突 2004 |
| `vault_rename` | `{path, newPath, rewriteLinks?}` | `RenameResult` | 重命名/移动 + 链接改写 |
| `vault_trash` | `{path}` | `TrashResult` | 移入回收站 |
| `vault_trash_list` | `{}` | `TrashList` | 回收站列表 |
| `vault_trash_restore` | `{trashId}` | `RestoreResult` | 还原 |
| `vault_empty_trash` | `{}` | `EmptyTrashResult` | 清空回收站 |
| `vault_create_note` | `{name?, parent?}` | `CreateResult` | 新建笔记 |
| `vault_create_folder` | `{path}` | `CreateResult` | 新建文件夹 |
| `vault_create_empty` | `{name}` | `CreateResult` | 缺失双链目标创建（FR-203） |
| `vault_save_attachment` | `{fileName, dataBase64}` | `{path}` | 粘贴图片（FR-103） |
| `vault_image_data` | `{path}` | `{dataUrl}` | 图片转 data URL 供预览 |
| `link_suggest` | `{prefix}` | `{items: SuggestItem[]}` | 双链补全（≤10） |
| `link_backlinks` | `{path}` | `{items: BacklinkItem[]}` | 反链 |
| `tag_list` | `{}` | `{tags: TagCount[]}` | 标签聚合 |
| `tag_normalize` | `{path?, apply?}` | `TagNormalizeReport{applied,scanned,changed,failed}` | 标签归位：正文行内 `#标签` → front-matter `tags`；`apply=false` 预览，`true` 执行（前端负责先自动备份） |
| `search_query` | `{kind:"title"\|"full", term, filters?, sort?, page?, pageSize?}` | `SearchResult` | 检索 |
| `index_rebuild` | `{}` | `{jobId}` | 全量重建（后台线程） |
| `index_status` | `{}` | `IndexStatus` | 索引进度/降级 |
| `io_import` | `{srcPath?, targetDir?}` | `ImportReport` | 导入（无 srcPath 弹选择框） |
| `io_export_note` | `{path, fmt:"md"\|"html"\|"pdf", outPath?, renderedHtml?}` | `ExportResult` | 导出（pdf 走前端打印） |
| `io_export_vault` | `{outPath?}` | `ExportResult` | 整库 ZIP |
| `backup_now` | `{}` | `BackupResult` | 立即备份 |
| `backup_list` | `{}` | `{items: BackupEntry[]}` | 备份历史 |
| `sync_configure` | `{url, username, password?}` | `SyncConfigView` | 保存配置 + 凭据 |
| `sync_config` | `{}` | `SyncConfigView` | 读配置视图 |
| `sync_run` | `{}` | `SyncPlan` | 手动同步 |
| `app_get_config` | `{}` | `AppConfig` | 读配置 |
| `app_set_config` | `{patch}` | `AppConfig` | 深合并保存 |
| `app_update_check` | `{manual?}` | `UpdateInfo` | 更新检查 |
| `notify_list` | `{limit?}` | `NotificationList` | 通知历史 |
| `notify_mark_read` | `{ids?}` | `{affected}` | 标记已读 |
| `hotkey_set` | `{commandId, combo, reset?}` | `{ok, hotkeys}` | 快捷键自定义；冲突 1004 |
| `app_open_dialog` | `{mode:"file"\|"folder"\|"save", title?, filters?}` | `DialogResult` | 原生对话框 |
| `app_info` | `{}` | `{version, platform, vault, configPath}` | 关于信息 |
| `app_log` | `{level, message}` | `{ok}` | 前端日志（可选） |

**事件（核心 → 前端，`window.__TAURI__.event.listen`）**：

| 事件名 | payload |
| --- | --- |
| `notes:written` | `{path, rev}` |
| `notes:external` | `{path, kind:"created"\|"modified"\|"deleted"\|"renamed", oldPath?}` |
| `index:progress` | `{done,total,phase:"rebuild"\|"reconcile"\|"done"}` |
| `sync:progress` | `{phase, path}` |
| `backup:done` | `{file, size}` |
| `notify:new` | `NotificationItem` |
| `vault:changed` | `{reason}`（目录树需刷新） |
| `vault:opened` | `OpenedVault`（启动自动恢复/打开库完成后推送；前端也可轮询 `vault_info`） |
| `config:changed` | `AppConfig`（配置被修改后推送，用于主题/缩放即时生效） |

## 6. 前端要求（frontend-dev）

- 纯静态：`frontend/index.html` + `frontend/styles.css` + `frontend/js/*.js`（ES Module）。
- Tauri 全局对象：`window.__TAURI__.core.invoke`、`window.__TAURI__.event.listen`（`withGlobalTauri` 已开）。
- 第三方库本地化（vendor，离线可用）：Markdown 渲染 `marked`；公式 `KaTeX`；代码高亮可选 `highlight.js`。从 npm/unpkg 下载后放到 `frontend/vendor/`，不要 CDN 运行期依赖（NFR-108 网络白名单）。
- 三栏布局（UR-01）：左侧栏（文件夹树 / 标签 / 回收站 / 检索入口）｜编辑区（工具栏+textarea）｜预览区；各栏可折叠；底部状态栏（字数、保存状态、同步状态、后台进度）。
- 必备功能（按 FR 优先级）：编辑/实时预览（FR-101/102）、格式工具栏（FR-104）、自动保存 2s + 失焦/切换/退出（FR-105）、大纲（FR-107）、文件夹/标签/双链/反链/回收站（FR-201~205）、标题打开 Ctrl+P（FR-301）、全文检索 Ctrl+Shift+F + 过滤/排序（FR-302/303）、导入导出入口（FR-401~403）、备份入口（FR-502）、同步设置（FR-503）、亮/暗/跟随系统主题（FR-602）、命令面板 Ctrl+Shift+P（FR-604）、通知中心（FR-504）、快捷键自定义（FR-606）。
- 无障碍（UR-08）：控件有 `aria-label`；键盘可完成主流程；焦点可见；200% 缩放布局不错乱（用 rem/相对布局，`zoom` 配置驱动 `document.documentElement` 字号/缩放）。
- 错误呈现：操作内联提示；后台错误进通知中心。
- 暗色/亮色：CSS variables + `prefers-color-scheme`；设置可强制。
- 首次启动 3 步引导（UR-02，localStorage 标记；也可用 config.onboardingDone）。
- 文件选择、导出、导入对话框经由 IPC（`app_open_dialog`/`vault_pick`）。
- 粘贴图片：paste 事件读剪贴板图片 → `vault_save_attachment` → 插入 `![](相对路径)` + 预览经 `vault_image_data` 内联显示。

## 7. 测试与完成定义（DoD）

- Rust：每个模块至少覆盖设计文档 13.3 指出的纯函数边界条件；`cargo test` 全绿。
- 关键单测：链接解析/改写、front-matter 维护、路径越界、原子写崩溃残留清理、回收站 30 天、FTS 中文检索/查询语法、导入编码嗅探、同步三方比对、7 天更新节流、通知上限 100。
- 集成测试：`tests/` 下用 `tempfile` 建临时库，跑"建库→建笔记→写入→索引→检索→重命名改写→回收站还原→备份→导出"闭环。
- 前端：`cargo run` 启动后手工走查 S-08 闭环；Lead 负责最终截图/验证。
- 文档：`README.md`（构建/运行/使用）、`docs/实现说明.md`、`docs/测试报告.md`（Lead 汇总）。

## 8. 提交约定

- 每完成一项，向 Lead 发消息（`send_message`）报告：改了哪些文件、测试命令与结果、遗留风险。
- 不要提交 git；由 Lead 统一审查/提交。
