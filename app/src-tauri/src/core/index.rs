//! M4 检索模块 Search（对应设计文档 4.4、5.3、7.2）。
//!
//! SQLite + FTS5 全文索引（派生数据，可全量重建）：
//! - `notes` 登记表（path/name/title/mtime/size/hash/opened_at/deleted_at/content）
//! - `notes_fts` FTS5 虚拟表（title/content，中文按字索引：写入前把 CJK 字符用空格分隔，
//!   查询侧同样转换；见 DD-4）
//! - `links` 双链表、`tags` 标签表（与笔记同事务维护）
//!
//! 一致性策略（7.2）：`notes.hash` 是唯一裁决字段；单笔记重索引 = 单事务。
//!
//! ## 对设计文档 7.2 schema 的扩展（均为派生字段，可随 index.db 一起重建）
//!
//! 1. `notes.content`：保存原始正文，用于检索命中的 snippet 与反链上下文
//!    （设计 7.2 的 notes 表没有正文字段；正文属权威文件的副本，索引损坏可重建）。
//! 2. `notes.name`：文件名去扩展名（派生列），与 `title` 分开建索引，
//!    用于双链 target_id 解析时「按展示标题 / 文件名匹配」并避免全表 LIKE 扫描。
//! 3. `links.alias` / `links.is_anchor`：保留 `[[目标|别名]]` 的别名与 `[[#锚点]]` 标记
//!    （M3 解析结果已提供，原设计只存 target_name/line 会丢失这两项派生信息）。
//! 4. `tags.source`：区分 `property`（front-matter）与 `inline`（正文 `#标签`）两种来源，
//!    供标签面板合并计数（FR-202；原来只有 (note_id, tag) 无法区分来源）。
//! 5. `notes_fts` 使用 `content=''`（contentless，同设计 7.2）+ `contentless_delete=1`：
//!    后者允许用普通 `DELETE FROM notes_fts WHERE rowid=?` 精确删除单行（SQLite ≥3.43；
//!    bundled 的 3.46 已支持）。如运行环境 FTS5 不可用/建表失败则 `fts_available=false`，
//!    检索回退 SQL LIKE 扫描（`degraded=false`，可用性单独由 `fts_available()` 暴露）。
//!
//! ## 中文检索（DD-4，按字索引）
//!
//! unicode61 分词器会把一串连续汉字当作**一个** token（`虚拟内存原理` → `虚拟内存原理`），
//! 因此写入前用 [`segment_cjk`] 把每个 Han 字符用空格分隔（`虚 拟 内 存 原 理`），
//! 查询侧把每个关键词做同样转换并包成 FTS5 短语 `"内 存"`：既保证「子串可命中」，
//! 又避免中文单词被拆成散字后误命中（"内存"不会命中"内容/存储"这类非连续组合）。

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};

use crate::error::{AppError, Result};
use crate::types::{ParsedNote, SearchFilters, SearchItem, SearchResult, SuggestItem, TagCount};

/// 索引登记行的轻量快照（用于启动对账）。
#[derive(Debug, Clone)]
pub struct IndexedStub {
    pub path: String,
    pub hash: String,
    pub mtime: i64,
}

/// 反链查询中间行。
#[derive(Debug, Clone)]
pub struct BacklinkRow {
    pub src_path: String,
    pub src_title: String,
    pub line: u32,
    pub target_name: String,
    /// 源笔记正文（用于生成上下文摘要）
    pub src_content: String,
}

/// 单条双链入库行。
#[derive(Debug, Clone)]
pub struct LinkRow {
    pub target_name: String,
    pub line: u32,
    pub alias: Option<String>,
    pub is_anchor: bool,
}

/// 全文检索索引句柄。`conn` 由索引线程串行使用（调用方持有锁）。
pub struct SearchIndex {
    pub(crate) conn: rusqlite::Connection,
    pub(crate) fts_available: bool,
}

// ===========================================================================
// SQL（schema 见设计文档 7.2 + 本模块文档头上的扩展说明）
// ===========================================================================

/// 基础 schema。`IF NOT EXISTS` 保证重复打开幂等。
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
  id        INTEGER PRIMARY KEY,
  path      TEXT NOT NULL UNIQUE,        -- 库内相对路径（/ 分隔）
  name      TEXT NOT NULL DEFAULT '',    -- 派生：文件名去扩展名（双链解析用）
  title     TEXT NOT NULL,               -- front-matter.title 缺省取文件名
  mtime     INTEGER NOT NULL,            -- unix ms
  size      INTEGER NOT NULL,
  hash      TEXT NOT NULL,               -- 内容 hash（变更检测裁决字段）
  opened_at INTEGER,                     -- 最近打开（FR-301 排序键）
  deleted_at INTEGER,                    -- 软删标记（回收站/外删；软删不参与检索）
  content   TEXT NOT NULL DEFAULT ''     -- 派生：原始正文（snippet/反链上下文）
);
CREATE INDEX IF NOT EXISTS idx_notes_mtime ON notes(mtime DESC);
CREATE INDEX IF NOT EXISTS idx_notes_title ON notes(title);
CREATE INDEX IF NOT EXISTS idx_notes_name ON notes(name);

CREATE TABLE IF NOT EXISTS links (
  src_id      INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
  target_name TEXT NOT NULL,             -- [[目标名]]（已规范化：去 .md、去首尾空白）
  target_id   INTEGER REFERENCES notes(id) ON DELETE SET NULL, -- 可空：缺失链接
  line        INTEGER NOT NULL,
  alias       TEXT,
  is_anchor   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (src_id, target_name, line)
);
CREATE INDEX IF NOT EXISTS idx_links_target ON links(target_name);
CREATE INDEX IF NOT EXISTS idx_links_target_id ON links(target_id);

CREATE TABLE IF NOT EXISTS tags (
  note_id INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
  tag     TEXT NOT NULL,
  source  TEXT NOT NULL DEFAULT 'inline',  -- property | inline
  PRIMARY KEY (note_id, tag, source)
);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
"#;

/// contentless FTS5 表（设计 7.2 的 `content=''`）+ 可删除扩展。
const FTS_CONTENTLESS_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
  title, content,
  content = '',
  contentless_delete = 1,
  tokenize = 'unicode61 tokenchars ''_'''
);
"#;

/// FTS5 兜底建表（旧版 SQLite 不认识 contentless_delete 时使用；普通表同样支持 DELETE）。
const FTS_PLAIN_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
  title, content,
  tokenize = 'unicode61 tokenchars ''_'''
);
"#;

/// 单笔记 upsert：opened_at 保留既有值；重索引意味着文件存在，清空软删标记。
const UPSERT_NOTE_SQL: &str = r#"
INSERT INTO notes(path, name, title, mtime, size, hash, opened_at, deleted_at, content)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7)
ON CONFLICT(path) DO UPDATE SET
  name       = excluded.name,
  title      = excluded.title,
  mtime      = excluded.mtime,
  size       = excluded.size,
  hash       = excluded.hash,
  deleted_at = NULL,
  content    = excluded.content
"#;

/// 解析「按展示标题 / 文件名 / 路径」的 target_id：重名时取路径层级最浅者
/// （深度 = path 中 `/` 的个数，同时以路径长度与字典序兜底，保证确定性）。
const RESOLVE_TARGET_SQL: &str = r#"
SELECT id FROM notes
 WHERE deleted_at IS NULL
   AND (title = ?1 OR name = ?2 OR path = ?3)
 ORDER BY (length(path) - length(replace(path, '/', ''))) ASC,
          length(path) ASC,
          path ASC
 LIMIT 1
"#;

/// 新的目标笔记入库后，把它此前「缺失链接（target_id IS NULL）」的引用补上解析结果。
/// 只处理 target_name 恰好等于该笔记标题/文件名的链接，命中 idx_links_target。
const RE_RESOLVE_LINKS_SQL: &str = r#"
UPDATE links
   SET target_id = (
       SELECT n.id FROM notes n
        WHERE n.deleted_at IS NULL
          AND (n.title = links.target_name
               OR n.name = links.target_name
               OR n.path = (links.target_name || '.md'))
        ORDER BY (length(n.path) - length(replace(n.path, '/', ''))) ASC,
                 length(n.path) ASC,
                 n.path ASC
        LIMIT 1)
 WHERE target_id IS NULL
   AND (target_name = ?1 OR target_name = ?2)
"#;

/// 软删恢复时全量补解析缺失链接（恢复是低频操作，可接受一次全表扫描 NULL 行）。
const RE_RESOLVE_ALL_LINKS_SQL: &str = r#"
UPDATE links
   SET target_id = (
       SELECT n.id FROM notes n
        WHERE n.deleted_at IS NULL
          AND (n.title = links.target_name
               OR n.name = links.target_name
               OR n.path = (links.target_name || '.md'))
        ORDER BY (length(n.path) - length(replace(n.path, '/', ''))) ASC,
                 length(n.path) ASC,
                 n.path ASC
        LIMIT 1)
 WHERE target_id IS NULL
"#;

impl SearchIndex {
    /// 打开/创建索引库（WAL 模式）。schema 见设计文档 7.2。
    ///
    /// 索引是派生数据：若 db 文件损坏（"not a database"/malformed 等），
    /// 自动删除后重建一次（IndexDoctor 的自愈路径，设计 10.1 X-2）；
    /// 重建仍失败则返回 [`AppError::index_degraded`]。
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| AppError::io_ctx("创建索引目录失败", &e))?;
            }
        }
        match Self::open_and_init(db_path) {
            Ok(idx) => Ok(idx),
            Err(err) if is_corruption_error(&err) => {
                remove_db_files(db_path)
                    .map_err(|e| AppError::io_ctx("清理损坏的索引文件失败", &e))?;
                Self::open_and_init(db_path).map_err(|e| {
                    AppError::index_degraded("索引数据库损坏且无法重建").with_detail(e.to_string())
                })
            }
            Err(err) => Err(err),
        }
    }

    /// 内存索引（单元测试用）。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_conn(conn)
    }

    pub fn fts_available(&self) -> bool {
        self.fts_available
    }

    // -- 私有：打开与建表 ---------------------------------------------------

    fn open_and_init(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> Result<Self> {
        // 索引全是派生数据：WAL 提升读写并发，NORMAL 足够（设计 7.2）。
        // 网络盘等不支持 WAL 的目录下允许失败，退化为默认 journal 模式。
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL;");
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(SCHEMA_SQL)?;
        let fts_available = Self::init_fts(&conn);
        Ok(Self { conn, fts_available })
    }

    /// 探测并创建 FTS5 表；不可用时返回 false（检索降级为 LIKE 扫描）。
    fn init_fts(conn: &Connection) -> bool {
        // 探测：临时表失败说明该 SQLite 构建未启用 fts5 模块。
        if conn
            .execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS temp.monote_fts_probe USING fts5(x);")
            .is_err()
        {
            return false;
        }
        let _ = conn.execute_batch("DROP TABLE IF EXISTS temp.monote_fts_probe;");
        if conn.execute_batch(FTS_CONTENTLESS_SQL).is_ok() {
            return true;
        }
        // 兼容不支持 contentless_delete 的较老 SQLite：退化为普通 FTS5 表。
        conn.execute_batch(FTS_PLAIN_SQL).is_ok()
    }

    // -- 索引维护 -----------------------------------------------------------

    /// 单笔记重索引（notes + fts + links + tags 同事务提交）。
    pub fn reindex_note(&mut self, note: &ParsedNote) -> Result<()> {
        let tx = self.conn.transaction()?;
        let name = file_stem(&note.path);

        tx.execute(
            UPSERT_NOTE_SQL,
            params![
                note.path,
                name,
                note.title,
                note.mtime,
                note.size as i64,
                note.hash,
                note.content
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM notes WHERE path = ?1",
            params![note.path],
            |r| r.get(0),
        )?;

        if self.fts_available {
            tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])?;
            tx.execute(
                "INSERT INTO notes_fts(rowid, title, content) VALUES (?1, ?2, ?3)",
                params![
                    id,
                    segment_cjk(&note.title),
                    segment_cjk(&note.content)
                ],
            )?;
        }

        // 链接：先清空源笔记的全部出链，再按解析结果重建（单事务保证不半新半旧）。
        tx.execute("DELETE FROM links WHERE src_id = ?1", params![id])?;
        for link in &note.links {
            let target_name = normalize_target_name(&link.target_name);
            if target_name.is_empty() {
                continue;
            }
            let target_id = resolve_target_id(&tx, &target_name, id, link.is_anchor)?;
            tx.execute(
                "INSERT OR IGNORE INTO links(src_id, target_name, target_id, line, alias, is_anchor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    target_name,
                    target_id,
                    link.line as i64,
                    link.alias,
                    link.is_anchor as i64
                ],
            )?;
        }
        // 该笔记（作为目标）入库后，补解析此前指向它的缺失链接。
        tx.execute(RE_RESOLVE_LINKS_SQL, params![note.title, name])?;

        // 标签：行内 + front-matter 双来源去重后重建，保留 source 供面板分来源计数。
        tx.execute("DELETE FROM tags WHERE note_id = ?1", params![id])?;
        for (tag, source) in collect_tags(note) {
            tx.execute(
                "INSERT OR IGNORE INTO tags(note_id, tag, source) VALUES (?1, ?2, ?3)",
                params![id, tag, source],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// 软删/恢复（回收站、外删）。
    pub fn set_deleted(&mut self, path: &str, deleted: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        let deleted_at = if deleted {
            Some(now_ms())
        } else {
            None
        };
        tx.execute(
            "UPDATE notes SET deleted_at = ?1 WHERE path = ?2",
            params![deleted_at, path],
        )?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM notes WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            if deleted {
                // 软删的笔记不参与检索：连带移除 FTS 行，并让指向它的双链显示为缺失。
                if self.fts_available {
                    tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])?;
                }
                tx.execute(
                    "UPDATE links SET target_id = NULL WHERE target_id = ?1",
                    params![id],
                )?;
            } else {
                // 恢复：按 notes 中保存的正文重建 FTS 行（正文是派生副本，可直接复用）。
                if self.fts_available {
                    let (title, content): (String, String) = tx.query_row(
                        "SELECT title, content FROM notes WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])?;
                    tx.execute(
                        "INSERT INTO notes_fts(rowid, title, content) VALUES (?1, ?2, ?3)",
                        params![id, segment_cjk(&title), segment_cjk(&content)],
                    )?;
                }
                tx.execute(RE_RESOLVE_ALL_LINKS_SQL, [])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 物理移除索引条目（文件确已不存在）。
    pub fn remove_note(&mut self, path: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM notes WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            if self.fts_available {
                tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id])?;
            }
            // links/tags 由外键 ON DELETE CASCADE 一并清理。
            tx.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 更新最近打开时间（FR-301 排序键）。
    pub fn touch_opened(&mut self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET opened_at = ?1 WHERE path = ?2 AND deleted_at IS NULL",
            params![now_ms(), path],
        )?;
        Ok(())
    }

    /// 已索引笔记数。
    pub fn note_count(&self) -> Result<u32> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n.clamp(0, u32::MAX as i64) as u32)
    }

    /// 全量索引快照（path/hash/mtime），供启动/批量对账。
    ///
    /// 只返回未软删的条目：对账对象是磁盘上真实存在的笔记，回收站里的软删行
    /// 不应被当成「索引里有、磁盘上没有」的孤儿。
    pub fn list_indexed(&self) -> Result<Vec<IndexedStub>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, hash, mtime FROM notes WHERE deleted_at IS NULL ORDER BY path ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(IndexedStub { path: r.get(0)?, hash: r.get(1)?, mtime: r.get(2)? })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 清空全部索引数据（全量重建前调用）。
    pub fn clear(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        if self.fts_available {
            tx.execute("DELETE FROM notes_fts", [])?;
        }
        tx.execute("DELETE FROM tags", [])?;
        tx.execute("DELETE FROM links", [])?;
        tx.execute("DELETE FROM notes", [])?;
        tx.commit()?;
        Ok(())
    }

    // -- 检索 ---------------------------------------------------------------

    /// 标题检索（FR-301）：模糊匹配标题/文件名，按最近打开降序。
    pub fn title_search(
        &self,
        term: &str,
        filters: &SearchFilters,
        page: usize,
        page_size: usize,
    ) -> Result<SearchResult> {
        let started = Instant::now();
        let term = term.trim();
        let (page, page_size) = normalize_paging(page, page_size);
        let offset = page.saturating_mul(page_size);

        let mut clauses = vec!["n.deleted_at IS NULL".to_string()];
        let mut values: Vec<Value> = Vec::new();
        if !term.is_empty() {
            let pattern = format!("%{}%", escape_like(term));
            clauses.push("(n.title LIKE ? ESCAPE '\\' OR n.path LIKE ? ESCAPE '\\')".to_string());
            values.push(Value::Text(pattern.clone()));
            values.push(Value::Text(pattern));
        }
        append_filters(&mut clauses, &mut values, filters);
        let where_sql = clauses.join(" AND ");

        let count_sql = format!("SELECT COUNT(*) FROM notes n WHERE {where_sql}");
        let total: i64 = self
            .conn
            .query_row(&count_sql, params_from_iter(values.iter()), |r| r.get(0))?;

        let select_sql = format!(
            "SELECT n.path, n.title, n.mtime FROM notes n WHERE {where_sql}
             ORDER BY COALESCE(n.opened_at, 0) DESC, n.mtime DESC
             LIMIT ? OFFSET ?"
        );
        let mut args = values;
        args.push(Value::Integer(page_size as i64));
        args.push(Value::Integer(offset as i64));
        let mut stmt = self.conn.prepare(&select_sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (path, title, mtime) = row?;
            items.push(build_title_item(path, title, mtime, term));
        }

        Ok(SearchResult {
            items,
            total: total.max(0) as usize,
            degraded: false,
            took_ms: started.elapsed().as_millis() as u64,
            parsed: if term.is_empty() {
                None
            } else {
                Some(format!("title contains \"{term}\""))
            },
        })
    }

    /// 全文检索（FR-302/303）：FTS5 MATCH、多关键词 AND、精确短语、tag: 过滤；
    /// `sort` ∈ {"relevance" | "mtime"}。
    pub fn full_search(
        &self,
        term: &str,
        filters: &SearchFilters,
        sort: &str,
        page: usize,
        page_size: usize,
    ) -> Result<SearchResult> {
        let started = Instant::now();
        let parsed = parse_query(term);
        if parsed.terms.is_empty() && parsed.phrases.is_empty() && parsed.tags.is_empty() {
            // 空查询：返回空集（前端展示搜索提示），不把全库倒出来。
            return Ok(SearchResult {
                items: Vec::new(),
                total: 0,
                degraded: false,
                took_ms: started.elapsed().as_millis() as u64,
                parsed: None,
            });
        }
        let (page, page_size) = normalize_paging(page, page_size);

        let page_result = if self.fts_available {
            match build_fts_query(&parsed) {
                Some(match_query) => {
                    match self.fts_full_page(
                        &match_query,
                        &parsed,
                        filters,
                        sort,
                        page,
                        page_size,
                    ) {
                        Ok(p) => p,
                        // FTS 查询在运行期异常（如索引损坏）：降级为 LIKE 扫描继续服务
                        // （FR-302 异常处理；degraded=true 供前端提示"正在重建索引"）。
                        Err(_) => self.like_full_page(&parsed, filters, page, page_size, true)?,
                    }
                }
                // 只有 tag:/过滤条件、没有关键词：走 notes 表扫描即可。
                None => self.like_full_page(&parsed, filters, page, page_size, false)?,
            }
        } else {
            // FTS5 构建期不可用：LIKE 降级是常态路径，degraded 仍为 false
            // （可用性由 fts_available() 单独暴露，避免误报索引损坏）。
            self.like_full_page(&parsed, filters, page, page_size, false)?
        };

        Ok(SearchResult {
            items: page_result.items,
            total: page_result.total,
            degraded: page_result.degraded,
            took_ms: started.elapsed().as_millis() as u64,
            parsed: Some(describe_query(&parsed)),
        })
    }

    /// 标签聚合（FR-202）：合并行内与 front-matter 两来源，按笔记数降序。
    pub fn tags(&self) -> Result<Vec<TagCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.tag,
                    COUNT(DISTINCT t.note_id) AS cnt,
                    SUM(CASE WHEN t.source = 'property' THEN 1 ELSE 0 END) AS pc,
                    SUM(CASE WHEN t.source = 'inline' THEN 1 ELSE 0 END) AS ic
               FROM tags t
               JOIN notes n ON n.id = t.note_id
              WHERE n.deleted_at IS NULL
              GROUP BY t.tag
              ORDER BY cnt DESC, t.tag ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TagCount {
                name: r.get(0)?,
                count: r.get::<_, i64>(1)?.clamp(0, u32::MAX as i64) as u32,
                property_count: r.get::<_, i64>(2)?.clamp(0, u32::MAX as i64) as u32,
                inline_count: r.get::<_, i64>(3)?.clamp(0, u32::MAX as i64) as u32,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 反链查询（FR-204）：按目标名集合取全部引用。
    pub fn backlinks(&self, target_names: &[String]) -> Result<Vec<BacklinkRow>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut names: Vec<&str> = Vec::new();
        for raw in target_names {
            let name = normalize_target_name(raw);
            if name.is_empty() {
                continue;
            }
            if seen.insert(name.clone()) {
                names.push(Box::leak(name.into_boxed_str()));
            }
        }
        let mut out = Vec::new();
        for name in names {
            let mut stmt = self.conn.prepare(
                "SELECT n.path, n.title, l.line, l.target_name, n.content
                   FROM links l
                   JOIN notes n ON n.id = l.src_id
                  WHERE n.deleted_at IS NULL AND l.target_name = ?1 COLLATE NOCASE
                  ORDER BY n.mtime DESC, n.path ASC, l.line ASC",
            )?;
            let rows = stmt.query_map(params![name], |r| {
                Ok(BacklinkRow {
                    src_path: r.get(0)?,
                    src_title: r.get(1)?,
                    line: r.get::<_, i64>(2)?.max(0) as u32,
                    target_name: r.get(3)?,
                    src_content: r.get(4)?,
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// 双链补全（FR-203）：按标题前缀/子串匹配，按最近打开/修改时间排序，至多 `limit` 项。
    pub fn suggest(&self, prefix: &str, limit: usize) -> Result<Vec<SuggestItem>> {
        let prefix = sanitize_suggest_prefix(prefix);
        // FR-203：补全列表至多 10 项（limit<=0 视为默认 10）。
        let limit = if limit == 0 { 10 } else { limit.min(10) };
        let pattern = format!("%{}%", escape_like(&prefix));
        let mut stmt = self.conn.prepare(
            "SELECT title, path, mtime FROM notes
              WHERE deleted_at IS NULL
                AND (title LIKE ? ESCAPE '\\' OR path LIKE ? ESCAPE '\\')
              ORDER BY COALESCE(opened_at, 0) DESC, mtime DESC
              LIMIT ?",
        )?;
        let rows = stmt.query_map(
            params![pattern.clone(), pattern, limit as i64],
            |r| {
                Ok(SuggestItem {
                    name: r.get(0)?,
                    path: r.get(1)?,
                    mtime: r.get(2)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 单篇笔记的标题与 path（供链接解析/反链展示）。软删笔记视为不存在。
    pub fn note_title(&self, path: &str) -> Result<Option<String>> {
        let title = self
            .conn
            .query_row(
                "SELECT title FROM notes WHERE path = ?1 AND deleted_at IS NULL",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        Ok(title)
    }

    // -- 私有查询路径 -------------------------------------------------------

    /// FTS5 MATCH 路径：JOIN notes 取元数据，bm25 相关度排序。
    fn fts_full_page(
        &self,
        match_query: &str,
        parsed: &ParsedQuery,
        filters: &SearchFilters,
        sort: &str,
        page: usize,
        page_size: usize,
    ) -> Result<SearchPage> {
        let offset = page.saturating_mul(page_size);
        let mut clauses = vec![
            "n.deleted_at IS NULL".to_string(),
            "notes_fts MATCH ?".to_string(),
        ];
        let mut values = vec![Value::Text(match_query.to_string())];
        append_tag_filters(&mut clauses, &mut values, &parsed.tags);
        append_filters(&mut clauses, &mut values, filters);
        let where_sql = clauses.join(" AND ");

        let count_sql = format!(
            "SELECT COUNT(*) FROM notes n JOIN notes_fts ON notes_fts.rowid = n.id
              WHERE {where_sql}"
        );
        let total: i64 = self
            .conn
            .query_row(&count_sql, params_from_iter(values.iter()), |r| r.get(0))?;

        // bm25 越小越相关（SQLite 返回负值，升序即最相关在前）。
        let order_sql = if sort == "mtime" {
            "n.mtime DESC, n.path ASC"
        } else {
            "bm25(notes_fts) ASC, n.mtime DESC, n.path ASC"
        };
        let select_sql = format!(
            "SELECT n.path, n.title, n.mtime, n.content, bm25(notes_fts) AS score
               FROM notes n JOIN notes_fts ON notes_fts.rowid = n.id
              WHERE {where_sql}
              ORDER BY {order_sql}
              LIMIT ? OFFSET ?"
        );
        let mut args = values;
        args.push(Value::Integer(page_size as i64));
        args.push(Value::Integer(offset as i64));

        let mut stmt = self.conn.prepare(&select_sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (path, title, mtime, content, score) = row?;
            items.push(build_search_item(path, title, mtime, &content, parsed, score));
        }
        Ok(SearchPage { items, total: total.max(0) as usize, degraded: false })
    }

    /// LIKE 扫描路径：FTS5 不可用 / 运行期降级 / 仅 tag 过滤时使用。
    fn like_full_page(
        &self,
        parsed: &ParsedQuery,
        filters: &SearchFilters,
        page: usize,
        page_size: usize,
        degraded: bool,
    ) -> Result<SearchPage> {
        let offset = page.saturating_mul(page_size);
        let mut clauses = vec!["n.deleted_at IS NULL".to_string()];
        let mut values: Vec<Value> = Vec::new();
        // 关键词与短语都按「子串必须出现」（多关键词 AND）匹配 title/content。
        for term in parsed.phrases.iter().chain(parsed.terms.iter()) {
            let pattern = format!("%{}%", escape_like(term));
            clauses.push("(n.title LIKE ? ESCAPE '\\' OR n.content LIKE ? ESCAPE '\\')".to_string());
            values.push(Value::Text(pattern.clone()));
            values.push(Value::Text(pattern));
        }
        append_tag_filters(&mut clauses, &mut values, &parsed.tags);
        append_filters(&mut clauses, &mut values, filters);
        let where_sql = clauses.join(" AND ");

        let count_sql = format!("SELECT COUNT(*) FROM notes n WHERE {where_sql}");
        let total: i64 = self
            .conn
            .query_row(&count_sql, params_from_iter(values.iter()), |r| r.get(0))?;

        let select_sql = format!(
            "SELECT n.path, n.title, n.mtime, n.content FROM notes n WHERE {where_sql}
              ORDER BY n.mtime DESC, n.path ASC
              LIMIT ? OFFSET ?"
        );
        let mut args = values;
        args.push(Value::Integer(page_size as i64));
        args.push(Value::Integer(offset as i64));
        let mut stmt = self.conn.prepare(&select_sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (path, title, mtime, content) = row?;
            items.push(build_search_item(path, title, mtime, &content, parsed, 0.0));
        }
        Ok(SearchPage { items, total: total.max(0) as usize, degraded })
    }
}

/// 内部：一页检索结果（具体检索路径填充，外层统一计时与组装 SearchResult）。
struct SearchPage {
    items: Vec<SearchItem>,
    total: usize,
    degraded: bool,
}

// ===========================================================================
// 查询语法解析（FR-302）
// ===========================================================================

/// 解析后的检索查询（供单元测试与调试回显）。
#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    /// 普通关键词（AND）
    pub terms: Vec<String>,
    /// 精确短语
    pub phrases: Vec<String>,
    /// tag: 过滤
    pub tags: Vec<String>,
}

/// 解析全文检索语法（多关键词 AND / 英文双引号精确短语 / `tag:标签名`）。
///
/// 规则：
/// - 空白分隔的 token 为普通关键词，多个关键词之间 AND；
/// - `"..."`（同时兼容全角 `“...”`）内为精确短语，短语内可含空格；
/// - `tag:标签名` / `tag：标签名`（中文冒号兼容）为标签过滤，标签名可带 `#` 前缀；
/// - 重复关键词/短语/标签自动去重；未闭合引号按到字符串末尾的短语处理（宽容解析）。
pub fn parse_query(term: &str) -> ParsedQuery {
    let mut q = ParsedQuery::default();
    let mut chars = term.chars().peekable();

    while let Some(&c) = chars.peek() {
        if is_quote(c) {
            chars.next();
            let mut phrase = String::new();
            let mut closed = false;
            while let Some(c2) = chars.next() {
                if is_quote(c2) {
                    closed = true;
                    break;
                }
                phrase.push(c2);
            }
            push_unique(&mut q.phrases, &phrase);
            if !closed {
                break;
            }
        } else if c.is_whitespace() {
            chars.next();
        } else {
            let mut token = String::new();
            while let Some(&c2) = chars.peek() {
                if c2.is_whitespace() || is_quote(c2) {
                    break;
                }
                token.push(c2);
                chars.next();
            }
            classify_token(&mut q, &token);
        }
    }
    q
}

fn is_quote(c: char) -> bool {
    c == '"' || c == '\u{201C}' || c == '\u{201D}'
}

fn classify_token(q: &mut ParsedQuery, token: &str) {
    if let Some(tag) = strip_tag_prefix(token) {
        let tag = normalize_tag_name(tag);
        if !tag.is_empty() {
            push_unique(&mut q.tags, &tag);
        }
    } else {
        push_unique(&mut q.terms, token);
    }
}

/// 识别 `tag:` / `tag：` 前缀（大小写不敏感；兼容中文冒号）。
fn strip_tag_prefix(token: &str) -> Option<&str> {
    if token.len() < 4 {
        return None;
    }
    let prefix = token.get(..3)?;
    if !prefix.eq_ignore_ascii_case("tag") {
        return None;
    }
    let rest = &token[3..];
    rest.strip_prefix(':').or_else(|| rest.strip_prefix('\u{FF1A}'))
}

fn push_unique(list: &mut Vec<String>, raw: &str) {
    let s = raw.trim();
    if s.is_empty() {
        return;
    }
    if !list.iter().any(|x| x == s) {
        list.push(s.to_string());
    }
}

/// 把解析结果转成 FTS5 MATCH 表达式；没有关键词/短语时返回 None（纯 tag 过滤）。
///
/// 每个关键词都做 CJK 按字切分并包成短语（引用原词内部双引号），
/// 关键词之间用 `AND` 连接（FR-302「多关键词 AND」）。
fn build_fts_query(parsed: &ParsedQuery) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for raw in parsed.terms.iter().chain(parsed.phrases.iter()) {
        let segmented = segment_cjk(raw);
        let segmented = segmented.trim();
        if segmented.is_empty() {
            continue;
        }
        parts.push(format!("\"{}\"", segmented.replace('"', "\"\"")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

/// 调试回显：如 `terms: 内存 优化; phrases: "虚拟内存"; tags: #考研`。
fn describe_query(parsed: &ParsedQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !parsed.terms.is_empty() {
        parts.push(format!("terms: {}", parsed.terms.join(" ")));
    }
    if !parsed.phrases.is_empty() {
        let phrases: Vec<String> = parsed.phrases.iter().map(|p| format!("\"{p}\"")).collect();
        parts.push(format!("phrases: {}", phrases.join(" ")));
    }
    if !parsed.tags.is_empty() {
        let tags: Vec<String> = parsed.tags.iter().map(|t| format!("#{t}")).collect();
        parts.push(format!("tags: {}", tags.join(" ")));
    }
    if parts.is_empty() {
        "(empty)".to_string()
    } else {
        parts.join("; ")
    }
}

// ===========================================================================
// 检索辅助
// ===========================================================================

/// 规范化分页：page 为 0-based；pageSize=0 取默认 20，上限 50（设计 6.2 `page≤50`）。
fn normalize_paging(page: usize, page_size: usize) -> (usize, usize) {
    let size = if page_size == 0 { 20 } else { page_size.min(50) };
    (page, size)
}

/// 追加 SearchFilters 的 SQL 条件（folder/tag/mtime），并同步绑定参数。
fn append_filters(clauses: &mut Vec<String>, values: &mut Vec<Value>, filters: &SearchFilters) {
    if let Some(folder) = filters.folder.as_deref() {
        let folder = folder.trim().trim_matches('/').replace('\\', "/");
        if !folder.is_empty() {
            clauses.push("(n.path = ? OR n.path LIKE ? ESCAPE '\\')".to_string());
            values.push(Value::Text(folder.clone()));
            values.push(Value::Text(format!("{}/%", escape_like(&folder))));
        }
    }
    if let Some(tag) = filters.tag.as_deref() {
        let tag = normalize_tag_name(tag);
        if !tag.is_empty() {
            clauses.push(
                "EXISTS (SELECT 1 FROM tags ft WHERE ft.note_id = n.id AND ft.tag = ?)"
                    .to_string(),
            );
            values.push(Value::Text(tag));
        }
    }
    if let Some(from) = filters.mtime_from {
        clauses.push("n.mtime >= ?".to_string());
        values.push(Value::Integer(from));
    }
    if let Some(to) = filters.mtime_to {
        clauses.push("n.mtime <= ?".to_string());
        values.push(Value::Integer(to));
    }
}

/// 追加 `tag:` 语法条件（多标签 AND）。
fn append_tag_filters(clauses: &mut Vec<String>, values: &mut Vec<Value>, tags: &[String]) {
    for tag in tags {
        let tag = normalize_tag_name(tag);
        if tag.is_empty() {
            continue;
        }
        clauses.push(
            "EXISTS (SELECT 1 FROM tags ft WHERE ft.note_id = n.id AND ft.tag = ?)".to_string(),
        );
        values.push(Value::Text(tag));
    }
}

/// 标题检索结果条目：snippet = 标题本身，hl_offset 为命中词在标题中的字符偏移。
fn build_title_item(path: String, title: String, mtime: i64, term: &str) -> SearchItem {
    let snippet = if title.is_empty() { path.clone() } else { title.clone() };
    let (hl_offset, hl_len) = highlight_in(&snippet, term);
    SearchItem { path, title, snippet, hl_offset, hl_len, mtime, score: 0.0 }
}

/// 全文检索结果条目：优先在正文中取命中词 ±60 字符，其次标题命中，最后正文开头摘要。
fn build_search_item(
    path: String,
    title: String,
    mtime: i64,
    content: &str,
    parsed: &ParsedQuery,
    score: f64,
) -> SearchItem {
    // snippet 只展示正文（front-matter 是元数据，展示在摘要里对用户无意义）。
    let (_, body) = crate::core::links::split_front_matter(content);
    let body = if body.trim().is_empty() { content } else { body };
    if let Some((byte_idx, byte_len)) = find_hit(body, parsed) {
        let (snippet, hl_offset, hl_len) = make_snippet(body, byte_idx, byte_len);
        return SearchItem {
            path,
            title,
            snippet,
            hl_offset: Some(hl_offset),
            hl_len: Some(hl_len),
            mtime,
            score,
        };
    }
    if let Some((idx, len)) = find_hit(&title, parsed) {
        let hl_offset = title.get(..idx).map(|s| s.chars().count()).unwrap_or(0);
        let hl_len = title.get(idx..idx + len).map(|s| s.chars().count()).unwrap_or(0);
        return SearchItem {
            path,
            title: title.clone(),
            snippet: title,
            hl_offset: Some(hl_offset),
            hl_len: Some(hl_len),
            mtime,
            score,
        };
    }
    // 无关键词命中（仅 tag:/过滤条件）：返回正文开头摘要，无高亮。
    let head: String = body.chars().take(120).collect();
    let snippet = collapse_whitespace(&head);
    let snippet = if snippet.trim().is_empty() { title.clone() } else { snippet };
    SearchItem { path, title, snippet, hl_offset: None, hl_len: None, mtime, score }
}

/// 在文本中查找查询词（先短语后关键词，先精确后 ASCII 大小写不敏感）。
fn find_hit(text: &str, parsed: &ParsedQuery) -> Option<(usize, usize)> {
    for pattern in parsed.phrases.iter().chain(parsed.terms.iter()) {
        if pattern.is_empty() {
            continue;
        }
        if let Some(idx) = text.find(pattern.as_str()) {
            return Some((idx, pattern.len()));
        }
        let lower_text = text.to_ascii_lowercase();
        let lower_pattern = pattern.to_ascii_lowercase();
        if let Some(idx) = lower_text.find(&lower_pattern) {
            // to_ascii_lowercase 不改变字节长度与边界，字节索引可 1:1 映射回原文。
            return Some((idx, lower_pattern.len().min(text.len().saturating_sub(idx))));
        }
    }
    None
}

/// 单关键词高亮位置（字符偏移、字符长度）。
fn highlight_in(text: &str, term: &str) -> (Option<usize>, Option<usize>) {
    let term = term.trim();
    if term.is_empty() || text.is_empty() {
        return (None, None);
    }
    let mut found: Option<(usize, usize)> = None;
    if let Some(idx) = text.find(term) {
        found = Some((idx, term.len()));
    } else {
        let lower_text = text.to_ascii_lowercase();
        let lower_term = term.to_ascii_lowercase();
        if let Some(idx) = lower_text.find(&lower_term) {
            found = Some((idx, lower_term.len()));
        }
    }
    match found {
        Some((idx, len)) if text.is_char_boundary(idx) && text.is_char_boundary((idx + len).min(text.len())) => {
            let end = (idx + len).min(text.len());
            let offset = text[..idx].chars().count();
            let hl_len = text[idx..end].chars().count();
            (Some(offset), Some(hl_len))
        }
        _ => (None, None),
    }
}

/// 取命中词前后各至多 `AROUND` 字符的摘要：折叠换行、截断处加 `…`，
/// 返回 `(snippet, 命中词在 snippet 中的字符偏移, 命中词字符长度)`。
fn make_snippet(content: &str, byte_idx: usize, byte_len: usize) -> (String, usize, usize) {
    const AROUND: usize = 60;
    let hit_char_idx = content[..byte_idx].chars().count();
    let hit_char_len = content[byte_idx..byte_idx + byte_len].chars().count();
    let total_chars = content.chars().count();
    let start_char = hit_char_idx.saturating_sub(AROUND);
    let end_char = (hit_char_idx + hit_char_len + AROUND).min(total_chars);

    let start_byte = char_to_byte(content, start_char);
    let end_byte = char_to_byte(content, end_char);

    let prefix = collapse_whitespace(&content[start_byte..byte_idx]);
    let hit = collapse_whitespace(&content[byte_idx..byte_idx + byte_len]);
    let suffix = collapse_whitespace(&content[byte_idx + byte_len..end_byte]);

    let mut snippet = String::new();
    let mut offset = 0usize;
    if start_char > 0 {
        snippet.push('\u{2026}'); // …
        offset += 1;
    }
    offset += prefix.chars().count();
    snippet.push_str(&prefix);
    snippet.push_str(&hit);
    snippet.push_str(&suffix);
    if end_char < total_chars {
        snippet.push('\u{2026}');
    }
    (snippet, offset, hit.chars().count())
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len())
}

/// 折叠空白（snippet 用）：连续空白压成一个空格。
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

// ===========================================================================
// 文本 / 路径 / 标签辅助
// ===========================================================================

/// 是否 CJK 统一表意文字（含扩展 A/B/G 等区段）。中文按字索引只切分这些字符。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF     // CJK 扩展 A
        | 0x4E00..=0x9FFF   // CJK 基本区
        | 0xF900..=0xFAFF   // 兼容表意文字
        | 0x20000..=0x2EBEF // 扩展 B~F
        | 0x30000..=0x323AF // 扩展 G/H
    )
}

/// 中文按字切分（DD-4）：每个 CJK 字符前/后补空格，供 unicode61 分词器按单字建 token。
/// 非 CJK 字符原样保留（英文按词、数字/下划线由 tokenchars 处理）。
fn segment_cjk(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    for c in text.chars() {
        if is_cjk(c) {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// LIKE 模式转义（配合 `ESCAPE '\'`）。
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 标签名规范化：去 `#` 前缀与首尾空白。
fn normalize_tag_name(raw: &str) -> String {
    raw.trim().trim_start_matches('#').trim().to_string()
}

/// 链接目标名规范化：去首尾空白、去 `.md`/`.markdown` 后缀。
fn normalize_target_name(raw: &str) -> String {
    let raw = raw.trim();
    strip_md_suffix(raw).trim().to_string()
}

fn strip_md_suffix(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".markdown") {
        return name.get(..name.len() - 9).unwrap_or(name);
    }
    if lower.ends_with(".md") {
        return name.get(..name.len() - 3).unwrap_or(name);
    }
    name
}

/// 文件名去扩展名（派生列 `notes.name`）。
fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    strip_md_suffix(name).to_string()
}

/// 解析链接目标 id；`is_anchor`（`[[#标题]]`）指向当前笔记自身。
fn resolve_target_id(
    tx: &Transaction<'_>,
    target_name: &str,
    own_id: i64,
    is_anchor: bool,
) -> rusqlite::Result<Option<i64>> {
    if is_anchor {
        return Ok(Some(own_id));
    }
    let path_guess = format!("{target_name}.md");
    tx.query_row(
        RESOLVE_TARGET_SQL,
        params![target_name, target_name, path_guess],
        |r| r.get(0),
    )
    .optional()
}

/// 收集笔记的去重标签及其来源（property / inline），用于写入 tags 表。
fn collect_tags(note: &ParsedNote) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let mut seen: HashSet<(String, &'static str)> = HashSet::new();
    let mut add = |raws: &[String], source: &'static str| {
        for raw in raws {
            let tag = normalize_tag_name(raw);
            if tag.is_empty() {
                continue;
            }
            if seen.insert((tag.clone(), source)) {
                out.push((tag, source));
            }
        }
    };
    add(&note.property_tags, "property");
    add(&note.inline_tags, "inline");
    // 兼容只填了合并后 `tags`、未填来源列表的调用方（视为行内标签）。
    for raw in &note.tags {
        let tag = normalize_tag_name(raw);
        if tag.is_empty() || seen.iter().any(|(t, _)| t == &tag) {
            continue;
        }
        if seen.insert((tag.clone(), "inline")) {
            out.push((tag, "inline"));
        }
    }
    out
}

/// 双链补全前缀清理：去首尾空白，去掉用户已输入的 `[[` / `]]`。
fn sanitize_suggest_prefix(prefix: &str) -> String {
    let mut p = prefix.trim();
    while let Some(rest) = p.strip_prefix('[') {
        p = rest.trim_start();
    }
    while let Some(rest) = p.strip_suffix(']') {
        p = rest.trim_end();
    }
    p.to_string()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 判断是否是「索引库损坏」类错误（只有这类错误才允许删库重建）。
fn is_corruption_error(err: &AppError) -> bool {
    let detail = err.detail.as_deref().unwrap_or("");
    let text = format!("{} {}", err.msg, detail).to_ascii_lowercase();
    text.contains("not a database")
        || text.contains("malformed")
        || text.contains("corrupt")
        || text.contains("file is encrypted")
        || text.contains("database disk image")
}

/// 删除 index.db 及其 WAL/SHM 附属文件（索引为派生数据，可安全重建）。
fn remove_db_files(db_path: &Path) -> std::io::Result<()> {
    fn with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut os = path.as_os_str().to_os_string();
        os.push(suffix);
        std::path::PathBuf::from(os)
    }
    for path in [
        db_path.to_path_buf(),
        with_suffix(db_path, "-wal"),
        with_suffix(db_path, "-shm"),
    ] {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

// ===========================================================================
// 单元测试（内存索引 + tempfile 文件索引）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParsedLink;
    use tempfile::tempdir;

    // -- 构造辅助 -----------------------------------------------------------

    fn content_hash(content: &str) -> String {
        // 测试用简单 hash：只要求「内容变 → hash 变」，与 M3 的 xxhash64 口径无关。
        let mut h: u64 = 1469598103934665603;
        for b in content.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        format!("{h:016x}")
    }

    fn note(path: &str, title: &str, content: &str, mtime: i64) -> ParsedNote {
        ParsedNote {
            path: path.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            front_matter: None,
            tags: Vec::new(),
            inline_tags: Vec::new(),
            property_tags: Vec::new(),
            links: Vec::new(),
            created: None,
            updated: None,
            hash: content_hash(content),
            mtime,
            size: content.len() as u64,
        }
    }

    fn tagged(mut n: ParsedNote, inline: &[&str], property: &[&str]) -> ParsedNote {
        n.inline_tags = inline.iter().map(|s| s.to_string()).collect();
        n.property_tags = property.iter().map(|s| s.to_string()).collect();
        let mut all: Vec<String> = n
            .inline_tags
            .iter()
            .chain(n.property_tags.iter())
            .cloned()
            .collect();
        all.sort();
        all.dedup();
        n.tags = all;
        n
    }

    fn linked(mut n: ParsedNote, targets: &[(&str, u32)]) -> ParsedNote {
        n.links = targets
            .iter()
            .map(|(name, line)| ParsedLink {
                target_name: (*name).to_string(),
                alias: None,
                line: *line,
                is_anchor: false,
            })
            .collect();
        n
    }

    fn scalar_i64(idx: &SearchIndex, sql: &str) -> i64 {
        idx.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn filters() -> SearchFilters {
        SearchFilters::default()
    }

    fn index_all(idx: &mut SearchIndex, notes: &[ParsedNote]) {
        for n in notes {
            idx.reindex_note(n).unwrap();
        }
    }

    // -- 打开 / schema ------------------------------------------------------

    #[test]
    fn open_in_memory_exposes_fts5_and_empty_state() {
        let idx = SearchIndex::open_in_memory().unwrap();
        assert!(idx.fts_available(), "bundled SQLite 应支持 FTS5");
        assert_eq!(idx.note_count().unwrap(), 0);
        assert!(idx.list_indexed().unwrap().is_empty());
        assert!(idx.tags().unwrap().is_empty());
        assert!(idx.note_title("nope.md").unwrap().is_none());
    }

    #[test]
    fn schema_has_design_tables() {
        let idx = SearchIndex::open_in_memory().unwrap();
        for table in ["notes", "notes_fts", "links", "tags"] {
            let n = scalar_i64(
                &idx,
                &format!("SELECT COUNT(*) FROM sqlite_master WHERE name = '{table}'"),
            );
            assert_eq!(n, 1, "缺少表 {table}");
        }
        // 派生列与扩展列存在
        let n = scalar_i64(
            &idx,
            "SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name IN ('content','name','opened_at','deleted_at')",
        );
        assert_eq!(n, 4);
        let n = scalar_i64(
            &idx,
            "SELECT COUNT(*) FROM pragma_table_info('tags') WHERE name = 'source'",
        );
        assert_eq!(n, 1);
    }

    // -- 文本辅助 -----------------------------------------------------------

    #[test]
    fn cjk_segmentation_splits_han_only() {
        assert_eq!(segment_cjk("虚拟内存"), "虚 拟 内 存");
        assert_eq!(segment_cjk("FTS5 检索"), "FTS5 检 索");
        assert_eq!(segment_cjk("hello world"), "hello world");
        assert_eq!(segment_cjk("第3章：内存"), "第 3 章 ： 内 存");
        assert_eq!(file_stem("docs/操作系统.md"), "操作系统");
        assert_eq!(file_stem("a/b/c.markdown"), "c");
        assert_eq!(normalize_target_name("  二叉树.md "), "二叉树");
        assert_eq!(escape_like("a%b_c"), "a\\%b\\_c");
    }

    #[test]
    fn parse_query_supports_and_phrases_tags() {
        let q = parse_query("内存  优化 \"虚拟内存\" tag:考研 tag：#操作系统");
        assert_eq!(q.terms, vec!["内存", "优化"]);
        assert_eq!(q.phrases, vec!["虚拟内存"]);
        assert_eq!(q.tags, vec!["考研", "操作系统"]);

        // 中文冒号 + 全角引号 + 去重
        let q2 = parse_query("“虚拟 内存” 内存 内存 tag：考研");
        assert_eq!(q2.phrases, vec!["虚拟 内存"]);
        assert_eq!(q2.terms, vec!["内存"]);
        assert_eq!(q2.tags, vec!["考研"]);

        // 空 / 未闭合引号
        assert!(parse_query("   ").terms.is_empty());
        assert_eq!(parse_query("\"未闭合").phrases, vec!["未闭合"]);
    }

    #[test]
    fn fts_query_builder_segments_and_ands() {
        let q = parse_query("内存 tag:考研 \"虚拟内存\"");
        let match_query = build_fts_query(&q).expect("有关键词就应生成 MATCH");
        assert_eq!(match_query, "\"内 存\" AND \"虚 拟 内 存\"");
        assert!(build_fts_query(&parse_query("tag:考研")).is_none());
        // 关键词里的双引号被转义（""），避免把用户输入当 FTS 语法
        let mut tricky = ParsedQuery::default();
        tricky.terms.push("a\"b".to_string());
        assert_eq!(build_fts_query(&tricky), Some("\"a\"\"b\"".to_string()));
    }

    #[test]
    fn normalize_paging_applies_defaults_and_cap() {
        assert_eq!(normalize_paging(0, 0), (0, 20));
        assert_eq!(normalize_paging(2, 100), (2, 50));
        assert_eq!(normalize_paging(1, 10), (1, 10));
    }

    // -- 索引维护 -----------------------------------------------------------

    #[test]
    fn reindex_registers_note_meta() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let n = note("docs/操作系统.md", "操作系统", "正文", 1234);
        idx.reindex_note(&n).unwrap();

        assert_eq!(idx.note_count().unwrap(), 1);
        let stubs = idx.list_indexed().unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].path, "docs/操作系统.md");
        assert_eq!(stubs[0].hash, n.hash);
        assert_eq!(stubs[0].mtime, 1234);
        assert_eq!(idx.note_title("docs/操作系统.md").unwrap().as_deref(), Some("操作系统"));
        // FTS 行已写入（按字 token 可命中）
        assert_eq!(
            scalar_i64(&idx, r#"SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH '"操 作"'"#),
            1
        );
    }

    #[test]
    fn reindex_is_idempotent() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let n = linked(
            tagged(note("a.md", "A", "虚拟内存是操作系统的核心概念", 100), &["考研"], &[]),
            &[("操作系统", 2)],
        );
        idx.reindex_note(&n).unwrap();
        idx.reindex_note(&n).unwrap();

        assert_eq!(idx.note_count().unwrap(), 1);
        let r = idx.full_search("内存", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items.len(), 1);
        // 链接/标签没有重复行
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM links"), 1);
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM tags"), 1);
    }

    #[test]
    fn reindex_updates_hash_and_fts_content() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let old = note("a.md", "A", "旧版本的存储管理", 100);
        idx.reindex_note(&old).unwrap();
        assert_eq!(idx.list_indexed().unwrap()[0].hash, old.hash);

        let new = note("a.md", "A", "全新版本包含虚拟内存概念", 200);
        idx.reindex_note(&new).unwrap();
        let stubs = idx.list_indexed().unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].hash, new.hash);
        assert_eq!(stubs[0].mtime, 200);
        assert_eq!(idx.note_count().unwrap(), 1);

        // 新内容可命中；旧词不应命中（验证 FTS 旧行已删除）
        assert_eq!(idx.full_search("虚拟内存", &filters(), "", 0, 20).unwrap().total, 1);
        assert_eq!(idx.full_search("旧版本", &filters(), "", 0, 20).unwrap().total, 0);
    }

    #[test]
    fn remove_note_cascades_links_and_tags() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let n = linked(
            tagged(note("a.md", "A", "内存 内容", 100), &["考研"], &["工作"]),
            &[("目标", 1)],
        );
        idx.reindex_note(&n).unwrap();
        assert_eq!(idx.note_count().unwrap(), 1);

        idx.remove_note("a.md").unwrap();
        assert_eq!(idx.note_count().unwrap(), 0);
        assert!(idx.list_indexed().unwrap().is_empty());
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM links"), 0);
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM tags"), 0);
        assert_eq!(
            scalar_i64(&idx, r#"SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH '"内 存"'"#),
            0
        );
        // 幂等：移除不存在的条目返回 Ok
        idx.remove_note("a.md").unwrap();
    }

    #[test]
    fn clear_removes_everything() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                linked(note("a.md", "A", "内存", 1), &[("B", 1)]),
                tagged(note("b.md", "B", "内容", 2), &["考研"], &[]),
            ],
        );
        idx.clear().unwrap();
        assert_eq!(idx.note_count().unwrap(), 0);
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM notes"), 0);
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM links"), 0);
        assert_eq!(scalar_i64(&idx, "SELECT COUNT(*) FROM tags"), 0);
        assert_eq!(
            scalar_i64(&idx, r#"SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH '"内 存"'"#),
            0
        );
    }

    #[test]
    fn touch_opened_updates_sort_key() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(&mut idx, &[note("a.md", "A", "正文", 100)]);
        assert_eq!(
            scalar_i64(&idx, "SELECT COALESCE(opened_at, 0) FROM notes WHERE path = 'a.md'"),
            0
        );
        idx.touch_opened("a.md").unwrap();
        assert!(
            scalar_i64(&idx, "SELECT COALESCE(opened_at, 0) FROM notes WHERE path = 'a.md'") > 0
        );
        // 不存在的路径是 no-op
        idx.touch_opened("missing.md").unwrap();
    }

    // -- 标题检索（FR-301） -------------------------------------------------

    #[test]
    fn title_search_matches_chinese_and_files_positions() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("os/操作系统原理.md", "操作系统原理", "正文", 1000),
                note("ops/系统操作手册.md", "系统操作手册", "正文", 2000),
                note("misc/英语学习.md", "英语学习", "正文", 3000),
            ],
        );
        let r = idx.title_search("操作", &filters(), 0, 20).unwrap();
        assert_eq!(r.total, 2);
        assert_eq!(r.items.len(), 2);
        // opened_at 均为 0 → mtime 降序：系统操作手册在前
        assert_eq!(r.items[0].path, "ops/系统操作手册.md");
        assert_eq!(r.items[0].snippet, "系统操作手册");
        assert_eq!(r.items[0].hl_offset, Some(2));
        assert_eq!(r.items[0].hl_len, Some(2));
        assert_eq!(r.items[0].score, 0.0);
        assert_eq!(r.items[1].hl_offset, Some(0));

        assert_eq!(idx.title_search("不存在", &filters(), 0, 20).unwrap().total, 0);

        // 命中路径（文件夹/文件名）时 snippet 仍是标题，无标题内高亮
        idx.reindex_note(&note("操作指南/索引.md", "索引", "正文", 4000)).unwrap();
        let by_path = idx.title_search("操作指南", &filters(), 0, 20).unwrap();
        assert_eq!(by_path.total, 1);
        assert_eq!(by_path.items[0].snippet, "索引");
        assert_eq!(by_path.items[0].hl_offset, None);
    }

    #[test]
    fn title_search_applies_filters_and_paging() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                tagged(note("docs/a.md", "项目计划", "正文", 1000), &["考研"], &[]),
                tagged(note("docs/b.md", "项目总结", "正文", 5000), &[], &["工作"]),
                note("notes/c.md", "项目笔记", "正文", 9000),
                note("notes/d.md", "项目复盘", "正文", 11000),
            ],
        );

        let folder_only = SearchFilters { folder: Some("docs".into()), ..Default::default() };
        let r = idx.title_search("项目", &folder_only, 0, 20).unwrap();
        assert_eq!(r.total, 2);
        assert!(r.items.iter().all(|i| i.path.starts_with("docs/")));

        let tag_only = SearchFilters { tag: Some("#考研".into()), ..Default::default() };
        let r = idx.title_search("项目", &tag_only, 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "docs/a.md");

        let mtime_only = SearchFilters {
            mtime_from: Some(2000),
            mtime_to: Some(8000),
            ..Default::default()
        };
        let r = idx.title_search("项目", &mtime_only, 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "docs/b.md");

        // 分页：page 0 / 1，pageSize=2
        let p0 = idx.title_search("项目", &filters(), 0, 2).unwrap();
        assert_eq!(p0.total, 4);
        assert_eq!(p0.items.len(), 2);
        let p1 = idx.title_search("项目", &filters(), 1, 2).unwrap();
        assert_eq!(p1.items.len(), 2);
        let p2 = idx.title_search("项目", &filters(), 2, 2).unwrap();
        assert_eq!(p2.items.len(), 0);
        assert_eq!(p2.total, 4);
    }

    #[test]
    fn title_search_orders_by_opened_then_mtime() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("a.md", "项目A", "正文", 300),
                note("b.md", "项目B", "正文", 200),
                note("c.md", "项目C", "正文", 100),
            ],
        );
        // 直接写 opened_at，避免同毫秒时间戳造成断言不稳定
        idx.conn
            .execute("UPDATE notes SET opened_at = 1000 WHERE path = 'a.md'", [])
            .unwrap();
        idx.conn
            .execute("UPDATE notes SET opened_at = 2000 WHERE path = 'b.md'", [])
            .unwrap();
        let r = idx.title_search("项目", &filters(), 0, 20).unwrap();
        assert_eq!(
            r.items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            vec!["b.md", "a.md", "c.md"]
        );

        // 空关键词 = 列最近打开/修改的全部笔记（Ctrl+P 初始面板）
        let all = idx.title_search("", &filters(), 0, 20).unwrap();
        assert_eq!(all.total, 3);
    }

    // -- 全文检索（FR-302/303） --------------------------------------------

    #[test]
    fn full_search_hits_chinese_by_character() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&note(
            "os.md",
            "操作系统",
            "虚拟内存是操作系统的核心概念，内存管理非常重要。",
            1000,
        ))
        .unwrap();
        let r = idx.full_search("内存", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        let item = &r.items[0];
        assert!(item.snippet.contains("内存"), "snippet = {}", item.snippet);
        let offset = item.hl_offset.expect("应有高亮偏移");
        let len = item.hl_len.expect("应有高亮长度");
        let highlighted: String = item.snippet.chars().skip(offset).take(len).collect();
        assert_eq!(highlighted, "内存");
        assert_eq!(len, 2);
        // bm25 为负值（越小越相关）
        assert!(item.score <= 0.0);
    }

    #[test]
    fn full_search_requires_all_terms() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("a.md", "A", "内存优化与性能调优", 1000),
                note("b.md", "B", "内存管理基础", 2000),
            ],
        );
        let r = idx.full_search("内存 优化", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "a.md");
    }

    #[test]
    fn full_search_exact_phrase_is_contiguous() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("a.md", "A", "虚拟内存的管理机制", 1000),
                note("b.md", "B", "虚拟化内存管理机制", 2000),
            ],
        );
        let phrase = idx.full_search("\"虚拟内存\"", &filters(), "", 0, 20).unwrap();
        assert_eq!(phrase.total, 1);
        assert_eq!(phrase.items[0].path, "a.md");
        // 未加引号的中文关键词同样按「连续子串」匹配（按字短语）
        let plain = idx.full_search("虚拟内存", &filters(), "", 0, 20).unwrap();
        assert_eq!(plain.total, 1);
        assert_eq!(plain.items[0].path, "a.md");
        // 只查「内存」时两篇都命中（b.md 含连续"内存"二字）
        assert_eq!(idx.full_search("内存", &filters(), "", 0, 20).unwrap().total, 2);
    }

    #[test]
    fn full_search_tag_syntax_and_filter() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                tagged(note("a.md", "A", "内存优化方法", 1000), &["考研"], &[]),
                tagged(note("b.md", "B", "内存管理原理", 2000), &[], &["工作"]),
            ],
        );
        // 语法过滤：内存 tag:考研
        let r = idx.full_search("内存 tag:考研", &filters(), "", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "a.md");
        assert!(r.parsed.as_deref().unwrap().contains("#考研"));

        // filters.tag 同样生效
        let f = SearchFilters { tag: Some("工作".into()), ..Default::default() };
        let r = idx.full_search("内存", &f, "", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "b.md");

        // 只有 tag: 没有关键词：列出该标签全部笔记（按 mtime 降序）
        let r = idx.full_search("tag:考研", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "a.md");
        // 不存在的标签
        assert_eq!(idx.full_search("tag:不存在", &filters(), "", 0, 20).unwrap().total, 0);
    }

    #[test]
    fn full_search_folder_and_mtime_filters() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("docs/a.md", "A", "内存 观念", 1000),
                note("docs/b.md", "B", "内存 概念", 5000),
                note("notes/c.md", "C", "内存 纪要", 9000),
            ],
        );
        let folder = SearchFilters { folder: Some("docs/".into()), ..Default::default() };
        let r = idx.full_search("内存", &folder, "", 0, 20).unwrap();
        assert_eq!(r.total, 2);

        let range = SearchFilters {
            mtime_from: Some(3000),
            mtime_to: Some(10_000),
            ..Default::default()
        };
        let r = idx.full_search("内存", &range, "mtime", 0, 20).unwrap();
        assert_eq!(r.total, 2);
        // mtime 降序
        assert_eq!(r.items[0].path, "notes/c.md");

        // 组合过滤
        let both = SearchFilters {
            folder: Some("docs".into()),
            mtime_from: Some(3000),
            ..Default::default()
        };
        let r = idx.full_search("内存", &both, "", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].path, "docs/b.md");
    }

    #[test]
    fn full_search_paginates_with_total() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        for i in 1..=5 {
            idx.reindex_note(&note(
                &format!("n{i}.md"),
                &format!("笔记{i}"),
                &format!("第{i}篇 内存 内容"),
                i * 100,
            ))
            .unwrap();
        }
        let p0 = idx.full_search("内存", &filters(), "mtime", 0, 2).unwrap();
        assert_eq!(p0.total, 5);
        assert_eq!(p0.items.len(), 2);
        assert_eq!(p0.items[0].path, "n5.md");
        let p2 = idx.full_search("内存", &filters(), "mtime", 2, 2).unwrap();
        assert_eq!(p2.total, 5);
        assert_eq!(p2.items.len(), 1);
        assert_eq!(p2.items[0].path, "n1.md");
        // 空查询返回空集
        let empty = idx.full_search("   ", &filters(), "", 0, 20).unwrap();
        assert_eq!(empty.total, 0);
        assert!(empty.parsed.is_none());
    }

    #[test]
    fn full_search_sort_by_mtime_and_relevance() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        // 老笔记：命中次数多（相关度更高）；新笔记：修改时间更近
        idx.reindex_note(&note("old.md", "旧笔记", "内存 内存 内存 内存 内存", 1000))
            .unwrap();
        idx.reindex_note(&note("new.md", "新笔记", "内存", 9000)).unwrap();

        let by_relevance = idx.full_search("内存", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(by_relevance.items[0].path, "old.md");
        let by_time = idx.full_search("内存", &filters(), "mtime", 0, 20).unwrap();
        assert_eq!(by_time.items[0].path, "new.md");
    }

    #[test]
    fn full_search_like_fallback_when_fts_unavailable() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&note("a.md", "A", "虚拟内存是操作系统的核心", 1000))
            .unwrap();
        // 模拟 FTS5 构建期不可用
        idx.fts_available = false;

        let r = idx.full_search("内存", &filters(), "relevance", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        assert!(!r.degraded, "FTS 不可用是常态降级路径，不是索引损坏");
        assert!(r.items[0].snippet.contains("内存"));
        assert!(r.items[0].hl_offset.is_some());

        // 多关键词 AND 在 LIKE 路径同样成立
        assert_eq!(idx.full_search("内存 操作系统", &filters(), "", 0, 20).unwrap().total, 1);
        assert_eq!(idx.full_search("内存 不存在", &filters(), "", 0, 20).unwrap().total, 0);
    }

    // -- 软删 / 恢复 --------------------------------------------------------

    #[test]
    fn soft_delete_hides_from_all_queries_and_restore_brings_back() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let n = tagged(note("a.md", "操作系统", "虚拟内存是操作系统的核心概念", 1000), &["考研"], &[]);
        idx.reindex_note(&n).unwrap();

        idx.set_deleted("a.md", true).unwrap();
        assert_eq!(idx.note_count().unwrap(), 0);
        assert!(idx.list_indexed().unwrap().is_empty());
        assert_eq!(idx.title_search("操作", &filters(), 0, 20).unwrap().total, 0);
        assert_eq!(idx.full_search("内存", &filters(), "", 0, 20).unwrap().total, 0);
        assert!(idx.tags().unwrap().is_empty());
        assert!(idx.note_title("a.md").unwrap().is_none());
        // FTS 行已移除
        assert_eq!(
            scalar_i64(&idx, r#"SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH '"内 存"'"#),
            0
        );

        idx.set_deleted("a.md", false).unwrap();
        assert_eq!(idx.note_count().unwrap(), 1);
        assert_eq!(idx.full_search("内存", &filters(), "", 0, 20).unwrap().total, 1);
        assert_eq!(idx.title_search("操作", &filters(), 0, 20).unwrap().total, 1);
        assert_eq!(idx.tags().unwrap().len(), 1);
    }

    #[test]
    fn soft_delete_nulls_backlink_target_id_and_restore_resolves() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&note("b.md", "二叉树", "二叉树正文", 1000)).unwrap();
        idx.reindex_note(&linked(note("a.md", "A", "引用内容", 2000), &[("二叉树", 1)]))
            .unwrap();
        let target_id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = 'b.md'");
        assert_eq!(
            scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"),
            target_id
        );

        idx.set_deleted("b.md", true).unwrap();
        assert_eq!(scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"), -1);
        idx.set_deleted("b.md", false).unwrap();
        assert_eq!(
            scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"),
            target_id
        );
    }

    // -- 双链 / 标签 / 反链 / 补全 ------------------------------------------

    #[test]
    fn link_target_resolution_prefers_shallow_path() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&note("deep/algo/二叉树.md", "二叉树", "深层副本", 1000))
            .unwrap();
        idx.reindex_note(&note("二叉树.md", "二叉树", "浅层副本", 1000)).unwrap();
        idx.reindex_note(&linked(note("a.md", "A", "正文", 1000), &[("二叉树", 1), ("不存在", 2)]))
            .unwrap();

        let shallow_id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = '二叉树.md'");
        let deep_id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = 'deep/algo/二叉树.md'");
        assert_ne!(shallow_id, deep_id);
        let resolved: i64 = idx
            .conn
            .query_row(
                "SELECT COALESCE(target_id, -1) FROM links WHERE target_name = '二叉树'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, shallow_id, "重名时取路径层级最浅者");
        // 缺失链接 target_id 为 NULL
        assert_eq!(
            scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links WHERE target_name = '不存在'"),
            -1
        );
        // 文件名（与 title 不同）也能解析
        idx.reindex_note(&note("docs/操作系统.md", "OS 课程笔记", "正文", 1000)).unwrap();
        idx.reindex_note(&linked(note("b.md", "B", "正文", 1000), &[("操作系统", 1)]))
            .unwrap();
        let os_id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = 'docs/操作系统.md'");
        assert_eq!(
            scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links WHERE target_name = '操作系统'"),
            os_id
        );
    }

    #[test]
    fn late_created_target_gets_resolved() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&linked(note("a.md", "A", "正文", 1000), &[("后建笔记", 1)]))
            .unwrap();
        assert_eq!(scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"), -1);
        idx.reindex_note(&note("后建笔记.md", "后建笔记", "正文", 1000)).unwrap();
        let id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = '后建笔记.md'");
        assert_eq!(scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"), id);
    }

    #[test]
    fn anchor_link_targets_self() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let mut n = note("a.md", "A", "[[#小节]]", 1000);
        n.links = vec![ParsedLink {
            target_name: "#小节".into(),
            alias: None,
            line: 0,
            is_anchor: true,
        }];
        idx.reindex_note(&n).unwrap();
        let id = scalar_i64(&idx, "SELECT id FROM notes WHERE path = 'a.md'");
        assert_eq!(scalar_i64(&idx, "SELECT COALESCE(target_id, -1) FROM links"), id);
        assert_eq!(scalar_i64(&idx, "SELECT is_anchor FROM links"), 1);
    }

    #[test]
    fn backlinks_join_sources_with_context() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        idx.reindex_note(&note("b.md", "二叉树", "二叉树正文", 1000)).unwrap();
        idx.reindex_note(&linked(note("a.md", "A 笔记", "这是引用二叉树的上下文", 2000), &[("二叉树", 3)]))
            .unwrap();

        let rows = idx.backlinks(&["二叉树".into()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].src_path, "a.md");
        assert_eq!(rows[0].src_title, "A 笔记");
        assert_eq!(rows[0].line, 3);
        assert_eq!(rows[0].target_name, "二叉树");
        assert!(rows[0].src_content.contains("引用二叉树"));

        // 重复的目标名只查一次；空集合返回空；目标名可带 .md
        assert_eq!(idx.backlinks(&["二叉树".into(), "二叉树".into()]).unwrap().len(), 1);
        assert_eq!(idx.backlinks(&["二叉树.md".into()]).unwrap().len(), 1);
        assert!(idx.backlinks(&[]).unwrap().is_empty());

        // 源笔记软删后反链消失；恢复后回来
        idx.set_deleted("a.md", true).unwrap();
        assert!(idx.backlinks(&["二叉树".into()]).unwrap().is_empty());
        idx.set_deleted("a.md", false).unwrap();
        assert_eq!(idx.backlinks(&["二叉树".into()]).unwrap().len(), 1);
    }

    #[test]
    fn tags_aggregate_merges_two_sources() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                tagged(note("a.md", "A", "正文", 1000), &["考研"], &["考研", "操作系统"]),
                tagged(note("b.md", "B", "正文", 2000), &["考研"], &[]),
            ],
        );
        let tags = idx.tags().unwrap();
        assert_eq!(tags.len(), 2);
        let kaoyan = tags.iter().find(|t| t.name == "考研").unwrap();
        assert_eq!(kaoyan.count, 2, "同一笔记双来源合并计数只算一次");
        assert_eq!(kaoyan.property_count, 1);
        assert_eq!(kaoyan.inline_count, 2);

        let os = tags.iter().find(|t| t.name == "操作系统").unwrap();
        assert_eq!(os.count, 1);
        assert_eq!(os.property_count, 1);
        assert_eq!(os.inline_count, 0);

        // 软删笔记不计入
        idx.set_deleted("b.md", true).unwrap();
        let kaoyan = idx.tags().unwrap().into_iter().find(|t| t.name == "考研").unwrap();
        assert_eq!(kaoyan.count, 1);
        assert_eq!(kaoyan.inline_count, 1);
    }

    #[test]
    fn suggest_matches_prefix_and_orders_by_recent() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        index_all(
            &mut idx,
            &[
                note("algo/算法导论.md", "算法导论", "正文", 100),
                note("algo/算法图解.md", "算法图解", "正文", 200),
                note("os/操作系统.md", "操作系统", "正文", 300),
            ],
        );
        idx.conn
            .execute("UPDATE notes SET opened_at = 5000 WHERE path = 'algo/算法导论.md'", [])
            .unwrap();
        idx.conn
            .execute("UPDATE notes SET opened_at = 4000 WHERE path = 'algo/算法图解.md'", [])
            .unwrap();

        let items = idx.suggest("算法", 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "算法导论");
        assert_eq!(items[1].name, "算法图解");

        // 支持子串、[[ 前缀、limit 截断
        assert_eq!(idx.suggest("图解", 10).unwrap()[0].path, "algo/算法图解.md");
        assert_eq!(idx.suggest("[[算法", 10).unwrap().len(), 2);
        assert_eq!(idx.suggest("算法", 1).unwrap().len(), 1);
        // limit 上限 10（FR-203）
        assert_eq!(idx.suggest("", 0).unwrap().len(), 3);
        assert!(idx.suggest("不存在", 10).unwrap().is_empty());
    }

    // -- 文件索引 / 损坏自愈 ------------------------------------------------

    #[test]
    fn file_index_uses_wal_and_persists() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(".monote").join("index.db");
        {
            let mut idx = SearchIndex::open(&db).unwrap();
            let mode: String = idx
                .conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode.to_ascii_lowercase(), "wal");
            assert!(idx.fts_available());
            idx.reindex_note(&note("a.md", "操作系统", "虚拟内存", 1000)).unwrap();
        }
        // 重新打开：schema 幂等、数据仍在
        let idx = SearchIndex::open(&db).unwrap();
        assert_eq!(idx.note_count().unwrap(), 1);
        assert_eq!(idx.full_search("内存", &filters(), "", 0, 20).unwrap().total, 1);
    }

    #[test]
    fn corrupted_db_is_rebuilt_automatically() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("index.db");
        std::fs::write(&db, b"this file is definitely not a sqlite database").unwrap();

        let idx = SearchIndex::open(&db).unwrap();
        assert_eq!(idx.note_count().unwrap(), 0, "损坏库应被清理并重建");
        assert!(idx.fts_available());
        // 重建后可以正常写入/检索
        let mut idx = idx;
        idx.reindex_note(&note("a.md", "A", "内存", 1000)).unwrap();
        assert_eq!(idx.full_search("内存", &filters(), "", 0, 20).unwrap().total, 1);
    }

    #[test]
    fn snippet_skips_front_matter() {
        let mut idx = SearchIndex::open_in_memory().unwrap();
        let content = "---\ntitle: 操作系统\ntags: [操作系统]\n---\n\n# 操作系统\n\n虚拟内存通过页表映射实现。\n";
        idx.reindex_note(&note("os.md", "操作系统", content, 1000)).unwrap();

        let r = idx.full_search("内存", &filters(), "", 0, 20).unwrap();
        assert_eq!(r.total, 1);
        let item = &r.items[0];
        assert!(item.snippet.contains("虚拟内存"), "snippet={}", item.snippet);
        assert!(
            !item.snippet.contains("title:") && !item.snippet.contains("tags:"),
            "snippet 不应展示 front-matter：{}",
            item.snippet
        );
    }
}
