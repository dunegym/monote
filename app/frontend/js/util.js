/**
 * MoNote 前端通用工具函数（无依赖、纯函数优先）。
 * 所有导出均为 ES Module 命名导出。
 */

/** 限制数值到 [min, max]。 */
export function clamp(value, min, max) {
  const n = Number(value);
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, n));
}

/** 防抖：返回包装函数，附带 cancel()/flush()。 */
export function debounce(fn, wait = 150) {
  let timer = null;
  let lastArgs = null;
  let lastThis = null;
  function invoke() {
    timer = null;
    const args = lastArgs;
    const self = lastThis;
    lastArgs = null;
    lastThis = null;
    fn.apply(self, args || []);
  }
  function wrapped(...args) {
    lastArgs = args;
    lastThis = this;
    if (timer) clearTimeout(timer);
    timer = setTimeout(invoke, Math.max(0, wait));
  }
  wrapped.cancel = () => {
    if (timer) clearTimeout(timer);
    timer = null;
    lastArgs = null;
    lastThis = null;
  };
  wrapped.flush = () => {
    if (timer) {
      clearTimeout(timer);
      invoke();
    }
  };
  wrapped.pending = () => timer !== null;
  return wrapped;
}

/** 节流（首次立即执行，尾部至多一次）。 */
export function throttle(fn, wait = 100) {
  let last = 0;
  let timer = null;
  let lastArgs = null;
  let lastThis = null;
  return function wrapped(...args) {
    const now = Date.now();
    lastArgs = args;
    lastThis = this;
    const remaining = wait - (now - last);
    if (remaining <= 0) {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      last = now;
      fn.apply(lastThis, lastArgs);
    } else if (!timer) {
      timer = setTimeout(() => {
        timer = null;
        last = Date.now();
        fn.apply(lastThis, lastArgs);
      }, remaining);
    }
  };
}

/** HTML 转义。 */
export function escapeHtml(value) {
  return String(value == null ? '' : value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** 转义正则元字符。 */
export function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** 补零。 */
function pad2(n) {
  return n < 10 ? `0${n}` : String(n);
}

/** 毫秒时间戳 → 本地 `YYYY-MM-DD HH:mm`。 */
export function formatDateTime(ms) {
  if (!ms && ms !== 0) return '—';
  const d = new Date(Number(ms));
  if (Number.isNaN(d.getTime())) return '—';
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

/** 毫秒时间戳 → 本地 `YYYY-MM-DD`。 */
export function formatDate(ms) {
  if (!ms && ms !== 0) return '—';
  const d = new Date(Number(ms));
  if (Number.isNaN(d.getTime())) return '—';
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** 相对时间：刚刚 / N 分钟前 / N 小时前 / 昨天 / YYYY-MM-DD。 */
export function formatRelative(ms) {
  if (!ms && ms !== 0) return '—';
  const t = Number(ms);
  if (!Number.isFinite(t)) return '—';
  const diff = Date.now() - t;
  if (diff < 0) return formatDateTime(t);
  if (diff < 60 * 1000) return '刚刚';
  if (diff < 60 * 60 * 1000) return `${Math.floor(diff / 60000)} 分钟前`;
  if (diff < 24 * 60 * 60 * 1000) return `${Math.floor(diff / 3600000)} 小时前`;
  if (diff < 48 * 60 * 60 * 1000) return '昨天';
  if (diff < 7 * 24 * 60 * 60 * 1000) return `${Math.floor(diff / 86400000)} 天前`;
  return formatDate(t);
}

/** 字节数 → 人类可读。 */
export function formatSize(bytes) {
  const n = Number(bytes) || 0;
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** 当天 00:00 的时间戳。 */
export function startOfDay(ts = Date.now()) {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** N 天前 00:00 的时间戳。 */
export function daysAgoStart(days, ts = Date.now()) {
  return startOfDay(ts) - days * 86400000;
}

/** 取 basename（兼容 / 与 \\）。 */
export function basename(path) {
  const s = String(path || '');
  const idx = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return idx >= 0 ? s.slice(idx + 1) : s;
}

/** 取目录（不含末尾分隔符）；根目录返回 ''。 */
export function dirname(path) {
  const s = String(path || '').replace(/\\/g, '/');
  const idx = s.lastIndexOf('/');
  return idx > 0 ? s.slice(0, idx) : '';
}

/** 扩展名（含点，小写）；无扩展名返回 ''。 */
export function extname(path) {
  const name = basename(path);
  const idx = name.lastIndexOf('.');
  return idx > 0 ? name.slice(idx).toLowerCase() : '';
}

/** 去掉扩展名。 */
export function stripExt(path) {
  const name = basename(path);
  const idx = name.lastIndexOf('.');
  return idx > 0 ? name.slice(0, idx) : name;
}

/** 拼接库内相对路径。 */
export function joinPath(parent, child) {
  const p = normalizePath(parent || '');
  const c = String(child || '').replace(/\\/g, '/').replace(/^\/+/, '');
  if (!p) return c;
  if (!c) return p;
  return `${p}/${c}`;
}

/** 规范化库内相对路径：统一 /、折叠 ./ 与 ../、去首尾 /。 */
export function normalizePath(path) {
  const raw = String(path || '').replace(/\\/g, '/');
  const parts = [];
  for (const seg of raw.split('/')) {
    if (!seg || seg === '.') continue;
    if (seg === '..') {
      if (parts.length) parts.pop();
      continue;
    }
    parts.push(seg);
  }
  return parts.join('/');
}

/** 路径是否为同一库内路径（大小写敏感，按 / 规范化）。 */
export function samePath(a, b) {
  return normalizePath(a) === normalizePath(b);
}

/** 是否为目标可编辑元素（输入框/文本域/contenteditable）。 */
export function isEditableTarget(target) {
  if (!target || !(target instanceof Element)) return false;
  const el = /** @type {HTMLElement} */ (target);
  const tag = el.tagName;
  if (tag === 'TEXTAREA' || tag === 'INPUT' || tag === 'SELECT') return true;
  if (el.isContentEditable) return true;
  return false;
}

/** 生成短 id。 */
let uidCounter = 0;
export function uid(prefix = 'id') {
  uidCounter += 1;
  return `${prefix}-${Date.now().toString(36)}-${uidCounter}`;
}

/**
 * 在纯文本片段中高亮 [offset, offset+len) 的字符（按 Unicode 码点计）。
 * 返回转义后的 HTML 字符串。
 */
export function highlightChars(text, offset, len) {
  const src = String(text == null ? '' : text);
  const chars = [...src];
  const total = chars.length;
  const start = clamp(offset == null ? 0 : offset, 0, total);
  const length = clamp(len == null ? 0 : len, 0, total - start);
  if (!length) return escapeHtml(src);
  const before = escapeHtml(chars.slice(0, start).join(''));
  const hit = escapeHtml(chars.slice(start, start + length).join(''));
  const after = escapeHtml(chars.slice(start + length).join(''));
  return `${before}<mark>${hit}</mark>${after}`;
}

/** 文本统计：字数（不含空白字符）、字符数、行数。 */
export function textStats(text) {
  const src = String(text || '');
  const chars = [...src].length;
  const nonSpace = [...src.replace(/\s+/g, '')].length;
  const lines = src.length ? src.split(/\r\n|\r|\n/).length : 0;
  return { chars, words: nonSpace, lines };
}

// ---------------------------------------------------------------------------
// front-matter（YAML 简化处理：只解析/维护 title、tags、created、updated）
// ---------------------------------------------------------------------------

const FM_RE = /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/;

/**
 * 拆分 front-matter。
 * @returns {{frontMatter: string|null, body: string, header: string}}
 */
export function splitFrontMatter(content) {
  const src = String(content || '');
  const m = FM_RE.exec(src);
  if (!m) return { frontMatter: null, body: src, header: '' };
  return { frontMatter: m[1], body: src.slice(m[0].length), header: m[0] };
}

/** 简单 YAML 值解析（字符串/数组）。 */
function parseYamlValue(raw) {
  let v = String(raw == null ? '' : raw).trim();
  if (!v) return '';
  if (v.startsWith('[') && v.endsWith(']')) {
    return v
      .slice(1, -1)
      .split(',')
      .map((s) => unquote(s.trim()))
      .filter((s) => s.length > 0);
  }
  if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) {
    return unquote(v);
  }
  return v;
}

function unquote(v) {
  const s = String(v || '').trim();
  if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) {
    try {
      return JSON.parse(s);
    } catch {
      return s.slice(1, -1);
    }
  }
  if (s.length >= 2 && s.startsWith("'") && s.endsWith("'")) return s.slice(1, -1);
  return s;
}

/**
 * 解析 front-matter 字段（title/tags/created/updated + 未知键原样）。
 * @returns {{fields: Record<string, any>, lines: string[], frontMatter: string|null, body: string}}
 */
export function parseFrontMatter(content) {
  const { frontMatter, body } = splitFrontMatter(content);
  const fields = Object.create(null);
  const lines = frontMatter == null ? [] : frontMatter.split(/\r?\n/);
  let pendingListKey = null;
  for (const line of lines) {
    const listItem = /^\s*-\s+(.*)$/.exec(line);
    if (pendingListKey && listItem) {
      const arr = Array.isArray(fields[pendingListKey]) ? fields[pendingListKey] : [];
      arr.push(unquote(listItem[1]));
      fields[pendingListKey] = arr;
      continue;
    }
    const m = /^([A-Za-z0-9_-]+)\s*:\s*(.*)$/.exec(line);
    if (m) {
      const key = m[1];
      const rest = m[2];
      if (rest === '') {
        pendingListKey = key;
        fields[key] = [];
      } else {
        pendingListKey = null;
        fields[key] = parseYamlValue(rest);
      }
    }
  }
  return { fields, lines, frontMatter, body };
}

/** 取标题：front-matter title → 第一个 H1 → 文件名。 */
export function titleOf(content, path) {
  const fm = parseFrontMatter(content);
  if (fm.fields.title) return String(fm.fields.title).trim();
  const h1 = /^\s*#\s+(.+?)\s*$/m.exec(fm.body || '');
  if (h1) return h1[1].replace(/#+\s*$/, '').trim();
  return stripExt(path || '');
}

/** 取 tags：front-matter tags + 正文行内 #标签（合并去重）。 */
export function tagsOf(content) {
  const fm = parseFrontMatter(content);
  const set = new Set();
  const fmTags = fm.fields.tags;
  if (Array.isArray(fmTags)) fmTags.forEach((t) => set.add(String(t).replace(/^#/, '').trim()));
  else if (typeof fmTags === 'string') fmTags.split(/[,，\s]+/).forEach((t) => { if (t.trim()) set.add(t.trim().replace(/^#/, '')); });
  const inline = /(^|[\s(（])#([\p{L}\p{N}_-]+)/gu;
  let m;
  // 去掉代码块/行内代码后再匹配行内标签
  const body = (fm.body || '').replace(/```[\s\S]*?```/g, '').replace(/`[^`\n]*`/g, '');
  while ((m = inline.exec(body))) set.add(m[2]);
  return [...set].filter(Boolean);
}

function formatYamlScalar(key, value) {
  if (Array.isArray(value)) {
    return `[${value.map((v) => formatYamlTag(String(v))).join(', ')}]`;
  }
  const s = String(value == null ? '' : value);
  if (/^[\p{L}\p{N}_./-]+$/u.test(s) && !/^(true|false|null|yes|no|\d+)$/i.test(s)) return s;
  return JSON.stringify(s);
}

function formatYamlTag(s) {
  return /^[\p{L}\p{N}_./-]+$/u.test(s) ? s : JSON.stringify(s);
}

/**
 * 就地更新 front-matter 字段，返回新的完整 content。
 * - 无 front-matter 且有字段时自动创建；
 * - 保留未知字段与注释行；
 * - 值为 null/undefined 时删除该字段。
 */
export function setFrontMatterFields(content, patch) {
  const src = String(content || '');
  const { frontMatter, body, header } = splitFrontMatter(src);
  const entries = Object.entries(patch || {});
  if (!frontMatter && !entries.some(([, v]) => v != null)) return src;

  const originalLines = frontMatter == null ? [] : frontMatter.split(/\r?\n/);
  const out = [];
  const handled = new Set();
  const keyRe = /^([A-Za-z0-9_-]+)\s*:/;

  for (let i = 0; i < originalLines.length; i += 1) {
    const line = originalLines[i];
    const m = keyRe.exec(line);
    if (!m || !(m[1] in patch)) {
      // 处理「key:」后跟列表项的多行值
      out.push(line);
      continue;
    }
    const key = m[1];
    if (handled.has(key)) continue; // 丢弃重复键（含其列表行）
    handled.add(key);
    const value = patch[key];
    if (value == null || (Array.isArray(value) && value.length === 0 && key === 'tags')) {
      // 删除该键：同时吞掉紧随其后的 `- ` 列表行
      while (i + 1 < originalLines.length && /^\s*-\s+/.test(originalLines[i + 1])) i += 1;
      continue;
    }
    out.push(`${key}: ${formatYamlScalar(key, value)}`);
    // 吞掉原有列表行（若该键原为多行数组）
    while (i + 1 < originalLines.length && /^\s*-\s+/.test(originalLines[i + 1])) i += 1;
  }
  for (const [key, value] of entries) {
    if (handled.has(key) || value == null) continue;
    if (Array.isArray(value) && value.length === 0 && key === 'tags') continue;
    out.push(`${key}: ${formatYamlScalar(key, value)}`);
  }

  const fmText = out.join('\n');
  const newHeader = `---\n${fmText}\n---\n`;
  if (frontMatter == null) {
    return `${newHeader}\n${body}`;
  }
  // 原 front-matter 与 body 之间可能有多余空行：保留一个空行分隔语义
  const rest = body.startsWith('\n') ? body : `\n${body}`;
  return `${newHeader}${rest === '\n' ? '' : rest}`.replace(/\n{3,}$/, '\n');
}

// ---------------------------------------------------------------------------
// 模糊匹配（命令面板 / 检索辅助）
// ---------------------------------------------------------------------------

/**
 * 子序列模糊匹配评分：匹配返回正分（越大越相关），不匹配返回 -1。
 * 支持中文直接子串匹配。
 */
export function fuzzyScore(query, text) {
  const q = String(query || '').trim().toLowerCase();
  const t = String(text || '').toLowerCase();
  if (!q) return 0;
  if (!t) return -1;
  const direct = t.indexOf(q);
  if (direct >= 0) {
    return 1000 - direct * 2 - Math.max(0, t.length - q.length) * 0.1;
  }
  let score = 0;
  let ti = 0;
  let consecutive = 0;
  for (let qi = 0; qi < q.length; qi += 1) {
    const ch = q[qi];
    const found = t.indexOf(ch, ti);
    if (found < 0) return -1;
    score += 10;
    if (found === ti) consecutive += 1;
    else consecutive = 0;
    score += consecutive * 5;
    if (found === 0 || t[found - 1] === ' ' || t[found - 1] === '-' || t[found - 1] === '/') score += 8;
    ti = found + 1;
  }
  return score - t.length * 0.05;
}

/** 系统是否偏好深色。 */
export function prefersDark() {
  try {
    return !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
  } catch {
    return false;
  }
}

/** 简易 sleep。 */
export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** 从文件名生成相对图片引用（统一 /）。 */
export function toPosix(path) {
  return String(path || '').replace(/\\/g, '/');
}
