/**
 * Mock 后端：在非 Tauri 环境（静态服务器 / 浏览器演示）下模拟全部 IPC 命令。
 *
 * - 数据持久化到 localStorage（`monote.mock.v1`），可加 `?reset=1` 重置；
 * - `?novault=1` 用于演示「首次启动未打开库」的欢迎页；
 * - 事件通过 mockBus 发出（api.js 在 mock 模式下把 listen 转接到这里）；
 * - 真实功能以 Tauri 运行为准，本文件只用于 UI 演示与前端自测。
 */
import {
  basename,
  dirname,
  stripExt,
  joinPath,
  normalizePath,
  titleOf,
  tagsOf,
  splitFrontMatter,
  parseFrontMatter,
  textStats,
  formatDate,
  startOfDay,
  daysAgoStart,
  escapeHtml,
  clamp,
} from './util.js';

const STORAGE_KEY = 'monote.mock.v1';
const QUERY = (() => {
  try {
    return new URLSearchParams(window.location.search);
  } catch {
    return new URLSearchParams();
  }
})();

// ---------------------------------------------------------------------------
// 事件总线
// ---------------------------------------------------------------------------
const busListeners = new Map();

/** mock 事件总线（api.js 在 mock 模式下转接 listen）。 */
export const mockBus = {
  on(event, handler) {
    if (!busListeners.has(event)) busListeners.set(event, new Set());
    busListeners.get(event).add(handler);
    return () => mockBus.off(event, handler);
  },
  off(event, handler) {
    const set = busListeners.get(event);
    if (set) set.delete(handler);
  },
  emit(event, payload) {
    const set = busListeners.get(event);
    if (!set) return;
    for (const fn of [...set]) {
      try {
        fn(payload);
      } catch (err) {
        console.error('[monote mock] event handler error', err);
      }
    }
  },
};

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const mockDelay = (min = 8, max = 30) => sleep(min + Math.random() * (max - min));
const ok = (data) => ({ ok: true, data });
const fail = (code, msg, detail) => ({ ok: false, error: { code, msg, detail: detail || '' } });
const now = () => Date.now();
const iso = (ts = now()) => new Date(ts).toISOString();
const pad2 = (n) => (n < 10 ? `0${n}` : `${n}`);
const stamp = (ts = now()) => {
  const d = new Date(ts);
  return `${d.getFullYear()}${pad2(d.getMonth() + 1)}${pad2(d.getDate())}-${pad2(d.getHours())}${pad2(d.getMinutes())}${pad2(d.getSeconds())}`;
};
const unique = (arr) => [...new Set(arr)];

function deepMerge(base, patch) {
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return base;
  const out = Array.isArray(base) ? [...base] : { ...(base || {}) };
  for (const [key, value] of Object.entries(patch)) {
    if (value && typeof value === 'object' && !Array.isArray(value) && out[key] && typeof out[key] === 'object') {
      out[key] = deepMerge(out[key], value);
    } else {
      out[key] = value;
    }
  }
  return out;
}

function noteContent({ title, tags, created, updated, body }) {
  const tagLine = tags && tags.length ? `tags: [${tags.join(', ')}]\n` : 'tags: []\n';
  return `---\ntitle: ${title}\n${tagLine}created: ${created || iso()}\nupdated: ${updated || iso()}\n---\n\n${body || ''}`;
}

function noteTitle(path, content) {
  return titleOf(content, path);
}

const DEFAULT_HOTKEYS = {
  'command.palette': 'Ctrl+Shift+P',
  'search.title': 'Ctrl+P',
  'search.full': 'Ctrl+Shift+F',
  'note.new': 'Ctrl+N',
  'note.save': 'Ctrl+S',
  'sidebar.toggle': 'Ctrl+\\',
  'toolbar.toggle': 'Ctrl+Shift+T',
  'preview.toggle': 'Ctrl+Shift+M',
  'trash.open': 'Ctrl+Shift+Backspace',
  'settings.open': 'Ctrl+,',
  'search.next': 'Ctrl+Shift+J',
};

const DAY = 86400000;

function defaultNotifications() {
  return [
    {
      id: 3,
      level: 'info',
      title: '欢迎使用 MoNote（演示模式）',
      detail: '当前未连接 Tauri 后端，界面使用本地 Mock 数据。运行 cargo run 后即为真实功能。',
      ts: now() - 3600 * 1000,
      read: false,
    },
    {
      id: 2,
      level: 'info',
      title: '备份计划已就绪',
      detail: '距上次备份超过 24 小时后，程序启动时会自动备份整库，最多保留 5 份。',
      ts: now() - 26 * 3600 * 1000,
      read: true,
    },
  ];
}

function defaultData() {
  const t = now();
  const h = 3600 * 1000;
  const notes = {};
  const add = (path, created, updated, body, tags, title) => {
    notes[path] = {
      content: noteContent({ title: title || stripExt(path), tags, created: iso(created), updated: iso(updated), body }),
      rev: 1,
      mtime: updated,
    };
  };

  add(
    '欢迎使用 MoNote.md',
    t - 7 * DAY,
    t - 2 * h,
    `# 欢迎使用 MoNote 墨记

这是一款**本地优先**的个人 Markdown 笔记与知识管理工具。所有笔记都是库文件夹里的标准 \`.md\` 文件。

## 三步上手

- [x] 新建你的第一篇笔记
- [ ] 输入 \`[[\` 建立双链
- [ ] 用 <kbd>Ctrl+P</kbd> 或 <kbd>Ctrl+Shift+F</kbd> 检索

## 双链

输入 \`[[操作系统/第3章-内存管理|内存管理]]\` 即可链接到其他笔记。预览中点击悬空的链接（如 [[概率论]]）会创建同名笔记。

## 标签

在正文里写 #入门 #帮助，或在「属性」面板中添加标签，两者等价。

## 公式与代码

行内公式 $E = mc^2$，块级公式：

$$
\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}
$$

\`\`\`javascript
function hello(name) {
  return \`你好，\${name}！\`;
}
\`\`\`

> 提示：编辑停顿 2 秒会自动保存；左下角能看到保存状态。
`,
    ['入门', '帮助'],
    '欢迎使用 MoNote',
  );

  add(
    '操作系统/第3章-内存管理.md',
    t - 30 * DAY,
    t - 3 * h,
    `# 第3章 内存管理

> 本章讨论虚拟内存、分页与页面置换算法。

## 3.1 虚拟内存

每个进程拥有独立的虚拟地址空间，[[二叉树]] 是理解地址空间映射的好例子。分页机制把逻辑地址划分为页号与页内偏移：

$$
\\text{物理地址} = \\text{页框号} \\times \\text{页大小} + \\text{页内偏移}
$$

### 页面置换算法

- [x] FIFO（先进先出）
- [ ] LRU（最近最少使用）
- [ ] OPT（最优置换）

| 算法 | 优点 | 缺点 |
| --- | --- | --- |
| FIFO | 实现简单 | 可能出现 Belady 异常 |
| LRU | 命中率较高 | 需要额外硬件支持 |
| OPT | 理论最优 | 无法实际实现 |

\`\`\`python
def fifo(pages, frames):
    queue, faults = [], 0
    for page in pages:
        if page not in queue:
            faults += 1
            if len(queue) >= frames:
                queue.pop(0)
            queue.append(page)
    return faults
\`\`\`

参见 [[#3.1 虚拟内存]]、[[图论]]、[[第4章 文件系统]]。行内标签 #操作系统 #考研。

## 3.2 页面置换算法

置换算法的目标是降低缺页率。$E = mc^2$ 只是一个占位公式 :)
`,
    ['操作系统', '考研'],
    '第3章 内存管理',
  );

  add(
    '操作系统/第4章-文件系统.md',
    t - 21 * DAY,
    t - 2 * DAY,
    `# 第4章 文件系统

## 4.1 文件的逻辑结构

文件系统负责把字节流映射到磁盘块。与 [[第3章 内存管理]] 的页表机制类似，inode 中记录了数据块指针。

## 4.2 目录与路径

- 绝对路径：从根目录开始
- 相对路径：从当前目录开始
- 硬链接与软链接的区别

\`\`\`text
/root
├── etc
├── home
│   └── monote
└── var
\`\`\`

#操作系统 #文件系统
`,
    ['操作系统', '文件系统'],
    '第4章 文件系统',
  );

  add(
    '计算机基础/数据结构/二叉树.md',
    t - 40 * DAY,
    t - 5 * h,
    `# 二叉树

二叉树是每个节点最多有两个子树的树结构。

## 遍历方式

1. 前序遍历：根 → 左 → 右
2. 中序遍历：左 → 根 → 右
3. 后序遍历：左 → 右 → 根

## 性质

- 第 $i$ 层最多有 $2^{i-1}$ 个节点
- 深度为 $k$ 的二叉树最多有 $2^k - 1$ 个节点

\`\`\`java
class TreeNode {
  int val;
  TreeNode left, right;
}
\`\`\`

关联：[[第3章 内存管理]]、[[图论]]。#数据结构 #考研
`,
    ['数据结构', '考研'],
    '二叉树',
  );

  add(
    '计算机基础/数据结构/图论.md',
    t - 33 * DAY,
    t - 3 * DAY,
    `# 图论

图 $G=(V,E)$ 由顶点集 $V$ 与边集 $E$ 组成。

## 存储结构

- 邻接矩阵：适合稠密图，空间 $O(n^2)$
- 邻接表：适合稀疏图，空间 $O(n+e)$

## 最短路

| 算法 | 适用场景 | 复杂度 |
| --- | --- | --- |
| Dijkstra | 非负权单源 | $O((n+e)\\log n)$ |
| Bellman-Ford | 可含负权 | $O(ne)$ |
| Floyd | 多源 | $O(n^3)$ |

与 [[二叉树]] 都常用于算法题的建模。#数据结构 #图论
`,
    ['数据结构', '图论'],
    '图论',
  );

  add(
    '读书笔记/深度工作.md',
    t - 14 * DAY,
    t - 26 * h,
    `# 深度工作：如何有效使用每一点脑力

作者：卡尔·纽波特（Cal Newport）

## 核心观点

> 深度工作是指在无干扰的状态下专注进行职业活动，使个人的认知能力达到极限。

## 四条准则

1. 工作要深入
2. 拥抱无聊
3. 远离社交媒体
4. 摒弃浮浅

## 我的行动清单

- [ ] 每天上午 9:00–11:30 关闭即时通讯
- [x] 建立固定的深度工作仪式
- [ ] 每周复盘一次

相关：[[考研计划]]。#读书 #效率
`,
    ['读书', '效率'],
    '深度工作',
  );

  add(
    '考研计划.md',
    t - 60 * DAY,
    t - 30 * h,
    `# 考研计划

## 阶段目标

- [x] 3–6 月：数学基础 + 英语单词
- [ ] 7–9 月：专业课第一轮
- [ ] 10–12 月：真题冲刺

## 每日清单

- [ ] 数学 2 小时
- [ ] 英语阅读 2 篇
- [ ] 专业课 1 章
- [ ] 复盘 15 分钟

参考笔记：[[第3章 内存管理]]、[[二叉树]]、[[深度工作]]。#考研 #计划
`,
    ['考研', '计划'],
    '考研计划',
  );

  add(
    'Markdown 语法速查.md',
    t - 5 * DAY,
    t - 4 * h,
    `# Markdown 语法速查

## 基础语法

**加粗**、*斜体*、~~删除线~~、\`行内代码\`

1. 有序列表
2. 第二项

- 无序列表
- 第二项

> 引用块

## GFM 表格与任务列表

| 语法 | 效果 |
| --- | --- |
| \`**文字**\` | 加粗 |
| \`$E=mc^2$\` | 公式 |

- [x] 已完成的任务
- [ ] 未完成的任务

## 公式

行内 $a^2 + b^2 = c^2$；

$$
\\int_0^1 x^2 \\, dx = \\frac{1}{3}
$$

公式写错时（例如 $E=mc^$）会以红色提示显示，而不会让整篇预览崩溃。

## 双链与标签

- 双链：[[二叉树]]、[[概率论]]（不存在时会显示缺失样式）
- 行内标签：#帮助 #Markdown
`,
    ['帮助', 'Markdown'],
    'Markdown 语法速查',
  );

  add(
    '模板/笔记模板.md',
    t - 90 * DAY,
    t - 20 * DAY,
    `# 笔记模板

## 概念

## 要点

## 疑问

## 关联

- [[欢迎使用 MoNote]]

#模板
`,
    ['模板'],
    '笔记模板',
  );

  const trash = [
    {
      id: 'trash-001',
      originalPath: '草稿/旧版-复习提纲.md',
      name: '旧版-复习提纲',
      deletedAt: iso(t - 3 * DAY),
      isDir: false,
      size: 2048,
    },
    {
      id: 'trash-002',
      originalPath: '旧文件夹/废弃笔记.md',
      name: '废弃笔记',
      deletedAt: iso(t - 26 * DAY),
      isDir: false,
      size: 512,
    },
  ];

  const result = {
    root: 'C:\\Users\\Demo\\Documents\\MoNote',
    name: 'MoNote 示例库',
    vaultOpened: !(QUERY.get('novault') === '1'),
    notes,
    folders: ['操作系统', '计算机基础', '计算机基础/数据结构', '读书笔记', '模板', '草稿', '旧文件夹'],
    trash,
    attachments: {},
    notifications: defaultNotifications(),
    backups: [
      { file: 'C:\\Users\\Demo\\Documents\\MoNote-Backup\\monote-backup-20261028-093000.zip', size: 2_458_112, mtime: t - 5 * DAY },
      { file: 'C:\\Users\\Demo\\Documents\\MoNote-Backup\\monote-backup-20261101-090500.zip', size: 2_611_430, mtime: t - 1 * DAY },
    ],
    sync: {
      configured: true,
      url: 'https://dav.example.com/monote',
      username: 'demo',
      passwordSet: true,
      autoOnStart: false,
      lastRun: iso(t - 2 * DAY),
    },
    config: {
      version: 1,
      vault: { root: 'C:\\Users\\Demo\\Documents\\MoNote' },
      ui: { theme: 'auto', language: 'zh-CN', zoom: 1.0, toolbarVisible: true, sidebarVisible: true, previewVisible: true },
      editor: { autosaveDebounceMs: 2000 },
      backup: { dir: 'C:\\Users\\Demo\\Documents\\MoNote-Backup', keep: 5, lastBackupAt: iso(t - 26 * h) },
      sync: { autoOnStart: false, url: 'https://dav.example.com/monote', username: 'demo' },
      update: { autoCheck: true, lastCheck: iso(t - 9 * DAY), manifestUrl: 'https://update.monote.app/manifest.json' },
      hotkeys: { ...DEFAULT_HOTKEYS },
      onboardingDone: false,
    },
    nextId: 100,
  };
  if (QUERY.get('novault') === '1') {
    result.config.vault.root = null;
  }
  return result;
}

// ---------------------------------------------------------------------------
// 数据加载 / 保存
// ---------------------------------------------------------------------------
let data = loadData();
const attachments = new Map(); // path -> dataUrl（不持久化，避免 localStorage 配额问题）

function loadData() {
  try {
    if (QUERY.get('reset') === '1') localStorage.removeItem(STORAGE_KEY);
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (parsed && parsed.notes && parsed.config) {
        parsed.attachments = {};
        if (!Array.isArray(parsed.folders)) parsed.folders = [];
        return parsed;
      }
    }
  } catch (err) {
    console.warn('[monote mock] 读取本地数据失败，使用初始数据', err);
  }
  const fresh = defaultData();
  saveData(fresh);
  return fresh;
}

function saveData(d) {
  try {
    const copy = { ...d, attachments: {} };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(copy));
  } catch (err) {
    console.warn('[monote mock] 保存本地数据失败（localStorage 不可用或已满）', err);
  }
}

function persist() {
  saveData(data);
}

// ---------------------------------------------------------------------------
// 数据查询辅助
// ---------------------------------------------------------------------------
/** @returns {Array<{path:string,content:string,rev:number,mtime:number,title:string,tags:string[]}>} */
function allNotes() {
  return Object.entries(data.notes).map(([path, n]) => {
    const fm = splitFrontMatter(n.content);
    const title = noteTitle(path, n.content);
    return { path, content: n.content, rev: n.rev, mtime: n.mtime, title, tags: tagsOf(n.content), body: fm.body, created: '', updated: '' };
  });
}

function findNote(path) {
  const p = normalizePath(path);
  return Object.prototype.hasOwnProperty.call(data.notes, p) ? { path: p, ...data.notes[p] } : null;
}

function noteNamesMap() {
  const map = new Map();
  for (const n of allNotes()) map.set(n.title.toLowerCase(), n);
  return map;
}

function nextId(prefix = 'id') {
  data.nextId += 1;
  return `${prefix}-${data.nextId}`;
}

function relativeRemainingDays(deletedAtIso) {
  const deleted = new Date(deletedAtIso).getTime();
  if (Number.isNaN(deleted)) return 30;
  return 30 - Math.floor((now() - deleted) / DAY);
}

function trashItems() {
  return data.trash
    .map((t) => ({
      id: t.id,
      originalPath: t.originalPath,
      name: t.name,
      deletedAt: t.deletedAt,
      remainingDays: relativeRemainingDays(t.deletedAt),
      isDir: !!t.isDir,
      size: t.size || 0,
    }))
    .sort((a, b) => new Date(b.deletedAt) - new Date(a.deletedAt));
}

function buildTree() {
  const rootNodes = [];
  const folders = new Map();
  const ensureFolder = (path) => {
    const norm = normalizePath(path);
    if (!norm) return null;
    if (folders.has(norm)) return folders.get(norm);
    const node = { path: norm, name: basename(norm), kind: 'folder', children: [] };
    folders.set(norm, node);
    const parentPath = dirname(norm);
    if (!parentPath) rootNodes.push(node);
    else {
      const parent = ensureFolder(parentPath);
      if (parent) parent.children.push(node);
    }
    return node;
  };

  for (const folder of data.folders || []) ensureFolder(folder);

  for (const [path, note] of Object.entries(data.notes)) {
    const parentPath = dirname(path);
    const noteNode = {
      path,
      name: basename(path),
      kind: 'note',
      title: noteTitle(path, note.content),
      mtime: note.mtime,
    };
    if (!parentPath) rootNodes.push(noteNode);
    else {
      const parent = ensureFolder(parentPath);
      if (parent) parent.children.push(noteNode);
    }
  }

  const sortNodes = (nodes) => {
    nodes.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === 'folder' ? -1 : 1;
      return a.name.localeCompare(b.name, 'zh-CN');
    });
    for (const n of nodes) if (n.kind === 'folder' && n.children) sortNodes(n.children);
  };
  sortNodes(rootNodes);
  return rootNodes;
}

function vaultInfo() {
  if (!data.vaultOpened) return null;
  return { root: data.root, name: data.name, noteCount: Object.keys(data.notes).length };
}

function openedVault() {
  return {
    root: data.root,
    name: data.name,
    noteCount: Object.keys(data.notes).length,
    indexOk: true,
    indexBuilding: false,
  };
}

// ---------------------------------------------------------------------------
// 检索
// ---------------------------------------------------------------------------
function parseQuery(raw) {
  const term = String(raw || '');
  const phrases = [];
  const tags = [];
  let rest = term;
  rest = rest.replace(/"([^"]*)"/g, (_, p) => {
    phrases.push(p);
    return ' ';
  });
  rest = rest.replace(/(?:^|\s)tag:([^\s]+)/g, (_, t) => {
    tags.push(t.replace(/^#/, ''));
    return ' ';
  });
  const words = rest.split(/\s+/).map((s) => s.trim()).filter(Boolean);
  return { words, phrases, tags, parsed: [
    ...words,
    ...phrases.map((p) => `"${p}"`),
    ...tags.map((t) => `tag:${t}`),
  ].join(' AND ') };
}

function makeSnippet(text, needles) {
  const src = String(text || '');
  let best = null;
  for (const needle of needles) {
    if (!needle) continue;
    const idx = src.toLowerCase().indexOf(needle.toLowerCase());
    if (idx >= 0 && (best == null || idx < best.idx)) best = { idx, len: needle.length };
  }
  if (!best) {
    const plain = src.replace(/\s+/g, ' ').trim().slice(0, 100);
    return { snippet: plain, hlOffset: null, hlLen: null };
  }
  const radius = 36;
  const start = Math.max(0, best.idx - radius);
  const end = Math.min(src.length, best.idx + best.len + radius);
  const folded = `${start > 0 ? '…' : ''}${src.slice(start, end).replace(/\s+/g, ' ')}${end < src.length ? '…' : ''}`;
  const needleText = src.slice(best.idx, best.idx + best.len);
  const hit = folded.toLowerCase().indexOf(needleText.toLowerCase());
  let offset;
  if (hit >= 0) {
    offset = [...folded.slice(0, hit)].length;
  } else {
    offset = (start > 0 ? 1 : 0) + [...src.slice(start, best.idx)].length;
  }
  return { snippet: folded, hlOffset: offset, hlLen: [...needleText].length };
}

function searchTitle(term, filters, sort, page, pageSize) {
  const q = String(term || '').trim().toLowerCase();
  let items = allNotes().filter((n) => {
    if (!q) return true;
    return n.title.toLowerCase().includes(q) || basename(n.path).toLowerCase().includes(q);
  });
  items = applyFilters(items, filters);
  items.sort((a, b) => b.mtime - a.mtime);
  if (sort === 'mtime' || sort === 'recent') items.sort((a, b) => b.mtime - a.mtime);
  const total = items.length;
  const start = page * pageSize;
  const pageItems = items.slice(start, start + pageSize).map((n) => {
    const idx = q ? n.title.toLowerCase().indexOf(q) : -1;
    return {
      path: n.path,
      title: n.title,
      snippet: n.title,
      hlOffset: idx >= 0 ? [...n.title.slice(0, idx)].length : null,
      hlLen: idx >= 0 ? [...q].length : null,
      mtime: n.mtime,
      score: 0,
    };
  });
  return { items: pageItems, total, degraded: false, tookMs: 1, parsed: null };
}

function applyFilters(items, filters) {
  const f = filters || {};
  return items.filter((n) => {
    if (f.folder) {
      const folder = normalizePath(f.folder);
      if (folder && !(n.path === folder || n.path.startsWith(`${folder}/`))) return false;
    }
    if (f.tag) {
      const tag = String(f.tag).replace(/^#/, '');
      if (!n.tags.includes(tag)) return false;
    }
    if (f.mtimeFrom != null && n.mtime < Number(f.mtimeFrom)) return false;
    if (f.mtimeTo != null && n.mtime > Number(f.mtimeTo)) return false;
    return true;
  });
}

function searchFull(term, filters, sort, page, pageSize) {
  const q = parseQuery(term);
  const tagsFromQuery = q.tags;
  const needles = [...q.phrases, ...q.words].filter(Boolean);
  // 与后端一致：没有任何关键词/tag: 时返回空集，避免把全库倒出来
  if (!needles.length && !tagsFromQuery.length) {
    return { items: [], total: 0, degraded: false, tookMs: 0, parsed: null };
  }
  let items = allNotes();
  items = applyFilters(items, filters);
  if (tagsFromQuery.length) {
    items = items.filter((n) => tagsFromQuery.every((t) => n.tags.includes(t)));
  }
  if (needles.length) {
    items = items.filter((n) => {
      const hay = `${n.title}\n${n.body}`.toLowerCase();
      return needles.every((needle) => hay.includes(needle.toLowerCase()));
    });
  }
  const scored = items.map((n) => {
    const hay = `${n.title}\n${n.body}`.toLowerCase();
    let occurrences = 0;
    for (const needle of needles) {
      let i = 0;
      const low = needle.toLowerCase();
      while ((i = hay.indexOf(low, i)) >= 0) {
        occurrences += 1;
        i += low.length;
      }
    }
    const first = makeSnippet(n.body || n.title, needles.length ? needles : [term].filter(Boolean));
    return { note: n, score: -occurrences, snippet: first };
  });
  if (sort === 'mtime') scored.sort((a, b) => b.note.mtime - a.note.mtime);
  else scored.sort((a, b) => a.score - b.score || b.note.mtime - a.note.mtime);
  const total = scored.length;
  const start = page * pageSize;
  const pageItems = scored.slice(start, start + pageSize).map((s) => ({
    path: s.note.path,
    title: s.note.title,
    snippet: s.snippet.snippet,
    hlOffset: s.snippet.hlOffset,
    hlLen: s.snippet.hlLen,
    mtime: s.note.mtime,
    score: s.score,
  }));
  return { items: pageItems, total, degraded: false, tookMs: 2, parsed: q.parsed || null };
}

// ---------------------------------------------------------------------------
// 命令实现
// ---------------------------------------------------------------------------
const handlers = {
  async vault_info() {
    return ok(vaultInfo());
  },

  async vault_open(args) {
    const root = String((args && args.rootPath) || data.root || '').trim();
    if (!root) return fail(1001, '库路径为空');
    data.root = root;
    data.name = basename(root) || 'MoNote 库';
    data.vaultOpened = true;
    data.config.vault = { ...(data.config.vault || {}), root };
    persist();
    const info = openedVault();
    mockBus.emit('vault:opened', info);
    mockBus.emit('vault:changed', { reason: 'opened' });
    return ok(info);
  },

  async vault_pick() {
    data.vaultOpened = true;
    if (!data.root) data.root = 'C:\\Users\\Demo\\Documents\\MoNote';
    const info = openedVault();
    data.config.vault = { ...(data.config.vault || {}), root: data.root };
    persist();
    mockBus.emit('vault:opened', info);
    return ok(info);
  },

  async vault_list() {
    return ok({ nodes: buildTree() });
  },

  async vault_read(args) {
    const note = findNote(args && args.path);
    if (!note) return fail(1002, '笔记不存在', args && args.path);
    const fm = parseFrontMatter(note.content);
    const title = noteTitle(note.path, note.content);
    return ok({
      path: note.path,
      title,
      content: note.content,
      rev: note.rev,
      mtime: note.mtime,
      tags: tagsOf(note.content),
      created: fm.fields.created || iso(note.mtime),
      updated: fm.fields.updated || iso(note.mtime),
    });
  },

  async vault_write(args) {
    const path = normalizePath(args && args.path);
    const content = String((args && args.content) || '');
    const note = findNote(path);
    if (!note) return fail(1002, '笔记不存在', path);
    if (args && args.expectRev != null && Number(args.expectRev) !== note.rev) {
      return fail(2004, '笔记已在其他地方修改（修订号冲突）', `期望 ${args.expectRev}，实际 ${note.rev}`);
    }
    const { body } = splitFrontMatter(content);
    note.rev += 1;
    note.mtime = now();
    data.notes[path] = { content, rev: note.rev, mtime: note.mtime };
    persist();
    mockBus.emit('notes:written', { path, rev: note.rev });
    mockBus.emit('vault:changed', { reason: 'written' });
    return ok({
      path,
      rev: note.rev,
      mtime: note.mtime,
      title: noteTitle(path, content),
      tags: tagsOf(content),
      size: new TextEncoder().encode(content).length,
    });
  },

  async vault_rename(args) {
    const oldPath = normalizePath(args && args.path);
    let newPath = normalizePath(args && args.newPath);
    if (!newPath) return fail(1001, '新路径为空');
    const note = findNote(oldPath);
    if (note) {
      if (!/\.md$/i.test(newPath)) newPath += '.md';
      if (newPath === oldPath) return ok({ path: oldPath, newPath, rewritten: [] });
      if (findNote(newPath)) return fail(1003, '目标路径已存在', newPath);
      const oldTitle = noteTitle(oldPath, note.content);
      const newTitle = noteTitle(newPath, note.content);
      const content = note.content;
      const rewritten = [];
      if (oldTitle !== newTitle) {
        const re = new RegExp(`\\[\\[${oldTitle.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(\\|[^\\]]*)?\\]\\]`, 'g');
        for (const [p, n] of Object.entries(data.notes)) {
          if (p === oldPath) continue;
          const after = n.content.replace(re, (m, alias) => `[[${newTitle}${alias || ''}]]`);
          if (after !== n.content) {
            n.content = after;
            n.rev += 1;
            n.mtime = now();
            rewritten.push({ file: p, count: 1 });
          }
        }
      }
      data.notes[newPath] = { content, rev: note.rev + 1, mtime: now() };
      delete data.notes[oldPath];
      persist();
      mockBus.emit('vault:changed', { reason: 'renamed' });
      return ok({ path: oldPath, newPath, rewritten });
    }
    // 文件夹重命名/移动
    const prefix = `${oldPath}/`;
    const isFolder = (data.folders || []).includes(oldPath) || Object.keys(data.notes).some((p) => p.startsWith(prefix));
    if (!isFolder) return fail(1002, '路径不存在', oldPath);
    for (const p of Object.keys(data.notes)) {
      if (p.startsWith(prefix)) {
        const rel = p.slice(prefix.length);
        const np = `${newPath}/${rel}`;
        data.notes[np] = data.notes[p];
        delete data.notes[p];
      }
    }
    data.folders = (data.folders || []).map((f) => {
      if (f === oldPath) return newPath;
      if (f.startsWith(prefix)) return `${newPath}/${f.slice(prefix.length)}`;
      return f;
    });
    if (!data.folders.includes(newPath)) data.folders.push(newPath);
    persist();
    mockBus.emit('vault:changed', { reason: 'renamed' });
    return ok({ path: oldPath, newPath, rewritten: [] });
  },

  async vault_trash(args) {
    const path = normalizePath(args && args.path);
    const isDir = (data.folders || []).includes(path) || (!findNote(path) && Object.keys(data.notes).some((p) => p.startsWith(`${path}/`)));
    if (isDir) {
      const snapshot = {};
      for (const p of Object.keys(data.notes)) {
        if (p === path || p.startsWith(`${path}/`)) {
          snapshot[p] = data.notes[p];
          delete data.notes[p];
        }
      }
      const foldersSnapshot = (data.folders || []).filter((f) => f === path || f.startsWith(`${path}/`));
      data.folders = (data.folders || []).filter((f) => f !== path && !f.startsWith(`${path}/`));
      const entry = {
        id: nextId('trash'),
        originalPath: path,
        name: basename(path),
        deletedAt: iso(),
        isDir: true,
        size: 0,
        snapshot,
        foldersSnapshot,
      };
      data.trash.unshift(entry);
      persist();
      mockBus.emit('vault:changed', { reason: 'trashed' });
      return ok({ trashId: entry.id });
    }
    const note = findNote(path);
    if (!note) return fail(1002, '笔记不存在', path);
    delete data.notes[path];
    const entry = {
      id: nextId('trash'),
      originalPath: path,
      name: stripExt(basename(path)),
      deletedAt: iso(),
      isDir: false,
      size: new TextEncoder().encode(note.content).length,
      snapshot: { content: note.content },
    };
    data.trash.unshift(entry);
    persist();
    mockBus.emit('vault:changed', { reason: 'trashed' });
    return ok({ trashId: entry.id });
  },

  async vault_trash_list() {
    return ok({ items: trashItems(), total: data.trash.length });
  },

  async vault_trash_restore(args) {
    const id = args && args.trashId;
    const idx = data.trash.findIndex((t) => t.id === id);
    if (idx < 0) return fail(1002, '回收站条目不存在', id);
    const entry = data.trash[idx];
    let targetPath = entry.originalPath;
    let renamedTo = null;
    if (entry.isDir) {
      const snap = entry.snapshot || {};
      for (const [p, n] of Object.entries(snap)) {
        const rel = p.slice(entry.originalPath.length).replace(/^\//, '');
        let tp = rel ? joinPath(entry.originalPath, rel) : entry.originalPath;
        if (Object.prototype.hasOwnProperty.call(data.notes, tp)) {
          tp = tp.replace(/\.md$/i, `-还原-${stamp()}.md`);
          renamedTo = tp;
        }
        data.notes[tp] = n;
      }
      for (const folder of entry.foldersSnapshot || []) {
        if (!data.folders.includes(folder)) data.folders.push(folder);
      }
      targetPath = entry.originalPath;
    } else {
      if (Object.prototype.hasOwnProperty.call(data.notes, targetPath)) {
        renamedTo = targetPath.replace(/\.md$/i, `-还原-${stamp()}.md`);
        targetPath = renamedTo;
      }
      const content = entry.snapshot && entry.snapshot.content ? entry.snapshot.content : noteContent({ title: stripExt(basename(targetPath)), tags: [], body: `# ${stripExt(basename(targetPath))}\n` });
      data.notes[targetPath] = { content, rev: 1, mtime: now() };
    }
    data.trash.splice(idx, 1);
    persist();
    mockBus.emit('vault:changed', { reason: 'restored' });
    return ok({ path: targetPath, renamedTo: renamedTo || undefined });
  },

  async vault_empty_trash() {
    const cleared = data.trash.length;
    data.trash = [];
    persist();
    mockBus.emit('vault:changed', { reason: 'trash-emptied' });
    return ok({ cleared });
  },

  async vault_create_note(args) {
    const parent = normalizePath((args && args.parent) || '');
    let name = String((args && args.name) || '').trim() || '未命名笔记';
    name = stripExt(basename(name.replace(/[\\/]/g, '-')));
    let path = joinPath(parent, `${name}.md`);
    let existed = false;
    let n = 2;
    while (Object.prototype.hasOwnProperty.call(data.notes, path)) {
      existed = true;
      path = joinPath(parent, `${name} ${n}.md`);
      n += 1;
    }
    data.notes[path] = {
      content: noteContent({ title: stripExt(basename(path)), tags: [], body: `# ${stripExt(basename(path))}\n\n` }),
      rev: 1,
      mtime: now(),
    };
    persist();
    mockBus.emit('vault:changed', { reason: 'created' });
    return ok({ path, existed });
  },

  async vault_create_folder(args) {
    const path = normalizePath(args && args.path);
    if (!path) return fail(1001, '文件夹路径为空');
    const existed = (data.folders || []).includes(path) || Object.keys(data.notes).some((p) => p.startsWith(`${path}/`)) || fsFolders().has(path);
    if (!existed) data.folders.push(path);
    persist();
    mockBus.emit('vault:changed', { reason: 'folder-created' });
    return ok({ path, existed });
  },

  async vault_create_empty(args) {
    const name = String((args && args.name) || '').trim();
    if (!name) return fail(1001, '名称不能为空');
    const clean = stripExt(basename(name.replace(/[\\/]/g, '-')));
    const path = `${clean}.md`;
    if (Object.prototype.hasOwnProperty.call(data.notes, path)) return ok({ path, existed: true });
    data.notes[path] = {
      content: noteContent({ title: clean, tags: [], body: `# ${clean}\n\n` }),
      rev: 1,
      mtime: now(),
    };
    persist();
    mockBus.emit('vault:changed', { reason: 'created-empty' });
    return ok({ path, existed: false });
  },

  async vault_save_attachment(args) {
    const fileName = String((args && args.fileName) || `IMG-${Date.now()}.png`);
    const base64 = String((args && args.dataBase64) || '');
    if (!base64) return fail(1001, '图片数据为空');
    const estSize = Math.floor((base64.length * 3) / 4);
    if (estSize > 10 * 1024 * 1024) return fail(1001, '图片超过 10 MB 限制');
    const path = joinPath('attachments', fileName);
    attachments.set(path, base64.startsWith('data:') ? base64 : `data:image/png;base64,${base64}`);
    return ok({ path });
  },

  async vault_image_data(args) {
    const path = normalizePath(args && args.path);
    const dataUrl = attachments.get(path);
    if (dataUrl) return ok({ dataUrl });
    const label = basename(path);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180"><rect width="100%" height="100%" fill="#e8eef7"/><text x="50%" y="50%" font-family="sans-serif" font-size="14" fill="#4d6a8a" text-anchor="middle">${escapeHtml(label)}（演示模式图片占位）</text></svg>`;
    return ok({ dataUrl: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}` });
  },

  async link_suggest(args) {
    const prefix = String((args && args.prefix) || '').toLowerCase();
    const items = allNotes()
      .filter((n) => n.title.toLowerCase().includes(prefix) || basename(n.path).toLowerCase().includes(prefix))
      .sort((a, b) => b.mtime - a.mtime)
      .slice(0, 10)
      .map((n) => ({ name: n.title, path: n.path, mtime: n.mtime }));
    return ok({ items });
  },

  async link_backlinks(args) {
    const path = normalizePath(args && args.path);
    const target = findNote(path);
    if (!target) return ok({ items: [] });
    const title = noteTitle(target.path, target.content);
    const re = new RegExp(`\\[\\[${title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(\\|[^\\]]*)?\\]\\]`);
    const items = [];
    for (const n of allNotes()) {
      if (n.path === path) continue;
      const lines = n.content.split(/\r\n|\r|\n/);
      lines.forEach((line, idx) => {
        if (re.test(line)) {
          const match = line.match(re);
          const pos = match ? line.indexOf(match[0]) : 0;
          const start = Math.max(0, pos - 80);
          const end = Math.min(line.length, pos + (match ? match[0].length : 0) + 80);
          items.push({
            srcPath: n.path,
            srcTitle: n.title,
            line: idx,
            context: line.slice(start, end).replace(/\s+/g, ' ').trim(),
          });
        }
      });
    }
    return ok({ items });
  },

  async tag_list() {
    const map = new Map();
    for (const n of allNotes()) {
      const fmTagsRaw = parseFrontMatter(n.content).fields.tags;
      const propertyTags = Array.isArray(fmTagsRaw)
        ? fmTagsRaw.map((t) => String(t).replace(/^#/, ''))
        : typeof fmTagsRaw === 'string'
          ? fmTagsRaw.split(/[,，\s]+/).filter(Boolean).map((t) => t.replace(/^#/, ''))
          : [];
      const inlineTags = (n.body.match(/(^|[\s(（])#([\p{L}\p{N}_-]+)/gu) || []).map((m) => m.replace(/^.*#/, ''));
      for (const t of unique(propertyTags.filter(Boolean))) {
        const cur = map.get(t) || { name: t, count: 0, propertyCount: 0, inlineCount: 0 };
        cur.propertyCount += 1;
        map.set(t, cur);
      }
      for (const t of unique(inlineTags.filter(Boolean))) {
        const cur = map.get(t) || { name: t, count: 0, propertyCount: 0, inlineCount: 0 };
        cur.inlineCount += 1;
        map.set(t, cur);
      }
    }
    const tags = [...map.values()].map((t) => ({ ...t, count: new Set([t.name]).size + (t.propertyCount + t.inlineCount > 1 ? 0 : 0) }))
      .map((t) => {
        const noteCount = allNotes().filter((n) => n.tags.includes(t.name)).length;
        return { ...t, count: noteCount };
      })
      .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name, 'zh-CN'));
    return ok({ tags });
  },

  async search_query(args) {
    const a = args || {};
    const kind = a.kind === 'title' ? 'title' : 'full';
    const term = String(a.term == null ? '' : a.term);
    const page = Math.max(0, Number(a.page) || 0);
    const pageSize = clamp(Number(a.pageSize) || (kind === 'title' ? 20 : 50), 1, 100);
    const sort = a.sort || 'relevance';
    const started = performance.now();
    const result = kind === 'title'
      ? searchTitle(term, a.filters, sort, page, pageSize)
      : searchFull(term, a.filters, sort, page, pageSize);
    result.tookMs = Math.max(1, Math.round(performance.now() - started));
    return ok(result);
  },

  async index_status() {
    const count = Object.keys(data.notes).length;
    return ok({ building: false, degraded: false, indexed: count, progressDone: count, progressTotal: count, ftsAvailable: true });
  },

  async index_rebuild() {
    const total = Object.keys(data.notes).length;
    const jobId = nextId('job');
    (async () => {
      for (let done = 1; done <= total; done += 1) {
        // eslint-disable-next-line no-await-in-loop
        await sleep(60);
        mockBus.emit('index:progress', { done, total, phase: 'rebuild' });
      }
      mockBus.emit('index:progress', { done: total, total, phase: 'done' });
    })();
    return ok({ jobId });
  },

  async io_import(args) {
    const src = String((args && args.srcPath) || '导入示例.md');
    const name = stripExt(basename(src.replace(/\.markdown$/i, '.md'))) || '导入的笔记';
    const targetPath = `${name}-导入-${stamp()}.md`;
    data.notes[targetPath] = {
      content: noteContent({ title: name, tags: [], body: `# ${name}\n\n（演示模式：从 ${src} 导入的内容占位）\n` }),
      rev: 1,
      mtime: now(),
    };
    persist();
    mockBus.emit('vault:changed', { reason: 'imported' });
    return ok({
      jobId: nextId('job'),
      imported: 1,
      targetDir: '',
      transcoded: [],
      skipped: [],
    });
  },

  async io_export_note(args) {
    await mockDelay(80, 200);
    const fmt = (args && args.fmt) || 'md';
    if (fmt === 'pdf') return fail(1005, 'PDF 导出由前端打印管线处理（演示模式不支持）');
    const path = (args && args.path) || '笔记.md';
    const outPath = (args && args.outPath) || `C:\\Users\\Demo\\Documents\\导出\\${stripExt(basename(path))}.${fmt === 'html' ? 'html' : 'md'}`;
    return ok({ outPath, size: 2048 });
  },

  async io_export_vault(args) {
    await mockDelay(150, 300);
    const outPath = (args && args.outPath) || `C:\\Users\\Demo\\Documents\\monote-vault-${stamp()}.zip`;
    return ok({ outPath, size: 1024 * 1024 });
  },

  async backup_now() {
    await mockDelay(120, 260);
    const file = `${data.config.backup.dir}\\monote-backup-${stamp()}.zip`;
    const entry = { file, size: 2_700_000 + Math.floor(Math.random() * 20000), mtime: now() };
    data.backups.unshift(entry);
    data.backups = data.backups.slice(0, clamp(Number(data.config.backup.keep) || 5, 1, 10));
    data.config.backup.lastBackupAt = iso();
    persist();
    mockBus.emit('backup:done', { file, size: entry.size });
    return ok({ file, size: entry.size, kept: data.backups.length });
  },

  async backup_list() {
    return ok({ items: [...data.backups].sort((a, b) => b.mtime - a.mtime) });
  },

  async sync_config() {
    return ok({ ...data.sync });
  },

  async sync_configure(args) {
    const a = args || {};
    const url = String(a.url || '').trim();
    data.sync.url = url;
    data.sync.username = String(a.username || '');
    if (a.password) data.sync.passwordSet = true;
    data.sync.configured = !!url;
    data.config.sync = { ...(data.config.sync || {}), url, username: data.sync.username };
    persist();
    mockBus.emit('config:changed', data.config);
    return ok({ ...data.sync });
  },

  async sync_run() {
    data.status = data.status || {};
    const phases = ['snapshot', 'propfind', 'compare', 'upload', 'download', 'done'];
    for (const phase of phases) {
      // eslint-disable-next-line no-await-in-loop
      await sleep(120);
      mockBus.emit('sync:progress', { phase, path: '' });
    }
    const plan = { up: 2, down: 1, conflict: 0, skipped: 4, failed: 0, lastRun: iso() };
    data.sync.lastRun = plan.lastRun;
    persist();
    return ok(plan);
  },

  async app_get_config() {
    return ok(JSON.parse(JSON.stringify(data.config)));
  },

  async app_set_config(args) {
    const patch = (args && args.patch) || {};
    data.config = deepMerge(data.config, patch);
    persist();
    mockBus.emit('config:changed', JSON.parse(JSON.stringify(data.config)));
    return ok(JSON.parse(JSON.stringify(data.config)));
  },

  async app_update_check(args) {
    const manual = !!(args && args.manual);
    data.config.update.lastCheck = iso();
    persist();
    if (!manual && data.config.update.lastCheck) {
      return ok({ current: '1.0.0', latest: null, hasUpdate: false, notes: null, url: null, checkedAt: data.config.update.lastCheck, checked: false });
    }
    await sleep(120);
    return ok({
      current: '1.0.0',
      latest: '1.1.0',
      hasUpdate: true,
      notes: '新增：大纲面板折叠、更多快捷键；修复：中文检索摘要偏移。',
      url: 'https://monote.app/download',
      checkedAt: iso(),
      checked: true,
    });
  },

  async notify_list(args) {
    const limit = clamp(Number((args && args.limit) || 100), 1, 100);
    const items = [...data.notifications].sort((a, b) => b.ts - a.ts).slice(0, limit);
    return ok({ items, unread: items.filter((n) => !n.read).length });
  },

  async notify_mark_read(args) {
    const ids = args && Array.isArray(args.ids) ? args.ids : null;
    let affected = 0;
    for (const n of data.notifications) {
      if ((!ids || ids.includes(n.id)) && !n.read) {
        n.read = true;
        affected += 1;
      }
    }
    persist();
    return ok({ affected });
  },

  async hotkey_set(args) {
    const a = args || {};
    const commandId = String(a.commandId || '');
    if (!commandId) return fail(1001, '缺少命令 id');
    const hotkeys = { ...DEFAULT_HOTKEYS, ...(data.config.hotkeys || {}) };
    if (a.reset) {
      hotkeys[commandId] = DEFAULT_HOTKEYS[commandId];
    } else if (a.combo) {
      const combo = String(a.combo);
      const conflictEntry = Object.entries(hotkeys).find(([id, c]) => id !== commandId && c === combo);
      if (conflictEntry) {
        return fail(1004, `快捷键「${combo}」已被「${conflictEntry[0]}」占用`, conflictEntry[0]);
      }
      hotkeys[commandId] = combo;
    }
    data.config.hotkeys = hotkeys;
    persist();
    mockBus.emit('config:changed', JSON.parse(JSON.stringify(data.config)));
    return ok({ ok: true, hotkeys: { ...hotkeys } });
  },

  async app_open_dialog(args) {
    const mode = (args && args.mode) || 'file';
    await sleep(50);
    if (mode === 'folder') return ok({ path: 'C:\\Users\\Demo\\Documents' });
    if (mode === 'save') return ok({ path: `C:\\Users\\Demo\\Documents\\导出\\笔记-${stamp()}.md` });
    return ok({ path: 'C:\\Users\\Demo\\Documents\\导入示例.md' });
  },

  async app_info() {
    return ok({
      version: '1.0.0',
      platform: 'win32 (Mock 演示)',
      vault: data.vaultOpened ? data.root : '',
      configPath: `${data.root}\\.monote\\config.json`,
      noteCount: Object.keys(data.notes).length,
    });
  },

  async app_log(args) {
    // eslint-disable-next-line no-console
    console.log(`[monote][${(args && args.level) || 'info'}]`, (args && args.message) || '');
    return ok({ ok: true });
  },
};

function fsFolders() {
  const set = new Set();
  for (const p of Object.keys(data.notes)) {
    let dir = dirname(p);
    while (dir) {
      set.add(dir);
      dir = dirname(dir);
    }
  }
  return set;
}

// 未知命令的统一兜底
const KNOWN = new Set(Object.keys(handlers));

/**
 * 执行 mock 命令，返回与 Tauri 一致的 envelope。
 * @param {string} command
 * @param {Record<string, any>} args
 */
export async function mockInvoke(command, args = {}) {
  if (!KNOWN.has(command)) {
    console.warn(`[monote mock] 未实现的命令: ${command}`);
    return fail(9001, `演示模式未实现命令 ${command}`, '后端未连接：该操作在真实 Tauri 环境下可用');
  }
  await mockDelay();
  try {
    const result = await handlers[command](args || {});
    return result;
  } catch (err) {
    console.error(`[monote mock] ${command} 执行异常`, err);
    return fail(2002, `演示模式执行失败：${err && err.message ? err.message : err}`);
  }
}

/** 重置 mock 数据（设置页「重置演示数据」使用）。 */
export function resetMockData() {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* ignore */
  }
  data = defaultData();
  // 重新加载页面以便所有模块看到新数据
  window.location.reload();
}

/** mock 当前根目录（调试用）。 */
export function mockRoot() {
  return data.root;
}

