//! MoNote 端到端集成测试（对应设计文档 13.3 集成测试层）。
//!
//! 用临时库跑通核心闭环：
//! 打开库 → 写入（front-matter 维护）→ 双链/反链/标签 → 标题/全文检索 →
//! 重命名链接改写 → 回收站还原 → 备份 → 整库导出 → 索引重建 → 错误码。
//!
//! 运行：`cargo test --test integration -- --test-threads=1`（避免多个测试并行写真实备份目录）

use std::path::{Path, PathBuf};

use monote_lib::state::AppState;
use tempfile::TempDir;

struct Ctx {
    _tmp: TempDir,
    vault_dir: PathBuf,
    state: AppState,
}

fn setup() -> Ctx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vault_dir = tmp.path().join("vault");
    let cfg = tmp.path().join("config.json");
    let mut state = AppState::new_with_config(cfg);
    // 备份目录也放到临时目录，避免污染用户文档目录
    let backup_dir = tmp.path().join("backup");
    {
        let mut inner = state.inner.lock();
        inner.config.config.backup.dir = backup_dir.to_string_lossy().to_string();
        inner.config.save().expect("save test config");
    }
    state.open_vault(&vault_dir).expect("open vault");
    Ctx { _tmp: tmp, vault_dir: vault_dir.clone(), state }
}

#[test]
fn open_vault_creates_structure() {
    let ctx = setup();
    assert!(ctx.vault_dir.join("attachments").is_dir());
    assert!(ctx.vault_dir.join(".trash").is_dir());
    assert!(ctx.vault_dir.join(".monote").is_dir());
    assert!(ctx.vault_dir.join(".monote").join("index.db").exists());
    let info = ctx.state.vault_info().expect("vault info");
    assert!(info.note_count <= 1); // 可能为空
    assert!(Path::new(&info.root).exists());
}

#[test]
fn write_read_roundtrip_with_frontmatter() {
    let ctx = setup();
    let content = "# 操作系统-第3章\n\n虚拟内存是操作系统的重要概念。\n";
    let w = ctx
        .state
        .write_note("操作系统-第3章.md", content, Some(0))
        .expect("write");
    assert_eq!(w.rev, 1);
    assert!(w.size > 0);

    let read = ctx.state.read_note("操作系统-第3章.md").expect("read");
    assert!(read.content.contains("虚拟内存"));
    // 系统维护 front-matter
    assert!(read.content.starts_with("---"), "front-matter missing: {:?}", read.content);
    assert!(read.content.contains("updated:"));
    assert!(read.content.contains("title: 操作系统-第3章"));
    assert_eq!(read.rev, 1);
    assert!(!read.title.is_empty());
}

#[test]
fn revise_conflict_and_rev_increment() {
    let ctx = setup();
    ctx.state.write_note("a.md", "one", Some(0)).expect("write1");
    let w2 = ctx.state.write_note("a.md", "two", Some(1)).expect("write2");
    assert_eq!(w2.rev, 2);
    // 过期修订号 → 2004
    let e = ctx.state.write_note("a.md", "three", Some(1)).unwrap_err();
    assert_eq!(e.code, 2004);
    // 越界路径 → 1001
    let e = ctx.state.write_note("../escape.md", "x", None).unwrap_err();
    assert_eq!(e.code, 1001);
}

#[test]
fn wiki_links_backlinks_and_tags() {
    let ctx = setup();
    ctx.state
        .write_note("二叉树.md", "# 二叉树\n\n一种树形数据结构。\n", Some(0))
        .expect("write tree");
    ctx.state
        .write_note(
            "复习笔记.md",
            "# 复习\n\n今天复习了 [[二叉树]]，重点看遍历。\n\n#考研 #数据结构\n",
            Some(0),
        )
        .expect("write daily");

    let backlinks = ctx.state.backlinks("二叉树.md").expect("backlinks");
    assert!(
        backlinks.iter().any(|b| b.src_path == "复习笔记.md"),
        "backlinks: {backlinks:?}"
    );
    assert!(backlinks.iter().any(|b| b.context.contains("二叉树")));

    let tags = ctx.state.tags().expect("tags");
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"考研"), "tags: {tags:?}");
    assert!(names.contains(&"数据结构"), "tags: {tags:?}");

    let suggest = ctx.state.suggest("二叉").expect("suggest");
    assert!(suggest.iter().any(|s| s.name.contains("二叉树")));
}

#[test]
fn rename_rewrites_links() {
    let ctx = setup();
    ctx.state
        .write_note("二叉树.md", "# 二叉树\n", Some(0))
        .expect("write");
    ctx.state
        .write_note("引用.md", "见 [[二叉树]]。\n", Some(0))
        .expect("write ref");

    let r = ctx
        .state
        .rename("二叉树.md", "二叉树结构.md", true)
        .expect("rename");
    assert_eq!(r.new_path, "二叉树结构.md");
    assert!(r.rewritten.iter().any(|w| w.file == "引用.md" && w.count >= 1));

    let read = ctx.state.read_note("引用.md").expect("read ref");
    assert!(read.content.contains("[[二叉树结构]]"), "content: {}", read.content);
}

#[test]
fn trash_and_restore() {
    let ctx = setup();
    ctx.state.write_note("临时.md", "待删除", Some(0)).expect("write");
    let t = ctx.state.trash("临时.md").expect("trash");
    assert!(!t.trash_id.is_empty());
    let list = ctx.state.trash_list().expect("list");
    assert_eq!(list.total, 1);
    assert!(list.items[0].original_path.contains("临时.md"));

    let r = ctx.state.trash_restore(&t.trash_id).expect("restore");
    assert!(r.path.contains("临时.md"));
    assert!(ctx.vault_dir.join("临时.md").exists());
    assert_eq!(ctx.state.trash_list().expect("list2").total, 0);
}

#[test]
fn title_and_fulltext_search() {
    let ctx = setup();
    ctx.state
        .write_note(
            "操作系统-第3章.md",
            "# 操作系统-第3章\n\n虚拟内存通过页表映射实现。\n\n#考研\n",
            Some(0),
        )
        .expect("write");
    ctx.state
        .write_note("计算机网络.md", "# 计算机网络\n\nTCP 三次握手。\n", Some(0))
        .expect("write");

    let r = ctx
        .state
        .search("title", "操作", Default::default(), "relevance", 0, 20)
        .expect("title search");
    assert!(r.items.iter().any(|i| i.path.contains("操作系统")), "{r:?}");

    let r = ctx
        .state
        .search("full", "虚拟内存", Default::default(), "relevance", 0, 20)
        .expect("full search");
    assert!(r.items.iter().any(|i| i.path.contains("操作系统")), "{r:?}");
    assert!(r.items[0].snippet.contains("虚拟内存"));

    // tag: 过滤语法
    let r = ctx
        .state
        .search("full", "内存 tag:考研", Default::default(), "relevance", 0, 20)
        .expect("tag search");
    assert!(r.items.iter().any(|i| i.path.contains("操作系统")), "{r:?}");
}

#[test]
fn backup_export_and_index_rebuild() {
    let ctx = setup();
    ctx.state.write_note("备份测试.md", "# 备份\n", Some(0)).expect("write");
    let b = ctx.state.backup_now().expect("backup");
    assert!(Path::new(&b.file).exists(), "backup file missing: {}", b.file);
    assert!(b.size > 0);
    let list = ctx.state.backup_list().expect("backup list");
    assert!(!list.is_empty());

    let out = ctx._tmp.path().join("vault-export.zip");
    let e = ctx.state.export_vault(Some(&out)).expect("export");
    assert!(Path::new(&e.out_path).exists());
    assert!(e.size > 0);

    let job = ctx.state.rebuild_index().expect("rebuild start");
    assert!(job.get("jobId").is_some());
    // 等待后台重建完成
    for _ in 0..100 {
        if !ctx.state.index_status().building {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!ctx.state.index_status().building);
    let r = ctx
        .state
        .search("full", "备份", Default::default(), "relevance", 0, 20)
        .expect("search after rebuild");
    assert!(!r.items.is_empty());
}

#[test]
fn import_files_with_encoding_fallback() {
    let ctx = setup();
    let src_dir = ctx._tmp.path().join("import-src");
    std::fs::create_dir_all(src_dir.join("sub")).expect("mkdir");
    std::fs::write(src_dir.join("a.md"), "# A\n\nimported\n").expect("write a");
    std::fs::write(src_dir.join("sub").join("b.md"), "# B\n").expect("write b");
    // GB18030 编码文件
    let (gb, _, _) = encoding_rs::GB18030.encode("中文编码测试");
    std::fs::write(src_dir.join("gb.md"), &gb).expect("write gb");
    // 二进制垃圾文件
    std::fs::write(src_dir.join("bin.dat"), [0u8, 159, 146, 150, 0, 1]).expect("write bin");

    let report = ctx.state.import(&src_dir, Some("导入")).expect("import");
    assert_eq!(report.imported, 3, "report: {report:?}");
    assert!(report.transcoded.iter().any(|f| f.contains("gb.md")), "{report:?}");
    assert!(!report.skipped.is_empty(), "binary should be skipped: {report:?}");
    assert!(ctx.vault_dir.join("导入").join("a.md").exists());
    assert!(ctx.vault_dir.join("导入").join("sub").join("b.md").exists());
}

#[test]
fn config_patch_and_hotkey_conflict() {
    let ctx = setup();
    let cfg = ctx
        .state
        .set_config(&serde_json::json!({ "ui": { "theme": "dark", "zoom": 1.25 } }))
        .expect("set config");
    assert_eq!(cfg.ui.theme, "dark");
    assert!((cfg.ui.zoom - 1.25).abs() < f64::EPSILON);

    // 默认 command.palette = Ctrl+Shift+P；把 search.title 改成相同 → 1004
    let e = ctx
        .state
        .hotkey_set("search.title", "Ctrl+Shift+P", false)
        .unwrap_err();
    assert_eq!(e.code, 1004);
    // 不冲突则成功
    let ok = ctx.state.hotkey_set("search.title", "Ctrl+K", false).expect("hotkey");
    assert!(ok.get("hotkeys").is_some());
}

#[test]
fn notifications_and_app_info() {
    let ctx = setup();
    let n = ctx.state.notify("warn", "测试通知", Some("detail".into()));
    assert_eq!(n.level, "warn");
    let list = ctx.state.notify_list(None);
    assert!(list.items.iter().any(|i| i.title == "测试通知"));
    let affected = ctx.state.notify_mark_read(None);
    assert!(affected >= 1);

    let info = ctx.state.app_info();
    assert!(info.get("version").is_some());
    assert!(info.get("configPath").is_some());
}

#[test]
fn normalize_inline_tags_to_front_matter() {
    let ctx = setup();
    // 写入时系统会补 front-matter（title/created/updated），但正文行内标签原样保留
    ctx.state
        .write_note("标签笔记.md", "# 标签笔记\n\n正文 #考研 #操作系统 。\n\n#末尾\n", Some(0))
        .expect("write");
    let path = ctx.vault_dir.join("标签笔记.md");
    // 直接写盘的裸文件（无 front-matter）：归位时应先补齐 title/created/updated
    let raw_path = ctx.vault_dir.join("裸文件.md");
    std::fs::write(&raw_path, "# 裸文件\n\n直接写盘的文件 #裸标签\n").expect("raw write");

    // 预览：报告会变更，但不写盘
    let preview = ctx.state.normalize_tags(None, false).expect("preview");
    assert!(
        preview.changed.iter().any(|c| c.path == "标签笔记.md"),
        "preview: {preview:?}"
    );
    assert!(
        preview.changed.iter().any(|c| c.path == "裸文件.md"),
        "裸文件应被归位: {preview:?}"
    );
    let before = std::fs::read_to_string(&path).expect("read before");
    assert!(before.contains("#考研"), "预览不应写盘: {before}");
    let raw_before = std::fs::read_to_string(&raw_path).expect("read raw before");
    assert!(raw_before.starts_with("# 裸文件"), "预览不应改裸文件: {raw_before}");

    // 应用：标签迁移到 front-matter，正文不再含 #标签
    let report = ctx.state.normalize_tags(None, true).expect("apply");
    assert!(report.changed.iter().any(|c| c.path == "标签笔记.md"));
    let after = std::fs::read_to_string(&path).expect("read after");
    assert!(after.contains("tags: [考研, 操作系统, 末尾]"), "content: {after}");
    assert!(!after.contains("#考研") && !after.contains("#操作系统") && !after.contains("#末尾"));
    assert!(after.contains("正文"), "正文内容应保留: {after}");

    // 裸文件：归位后文件头应有 title 与 tags，正文行内标签被移除
    let raw_after = std::fs::read_to_string(&raw_path).expect("read raw after");
    assert!(raw_after.starts_with("---\ntitle: 裸文件\ntags: [裸标签]\n"), "{raw_after}");
    assert!(raw_after.contains("created:") && raw_after.contains("# 裸文件"));
    assert!(!raw_after.contains("#裸标签"));

    // 幂等：再次预览无变化
    let again = ctx.state.normalize_tags(None, false).expect("preview2");
    assert!(again.changed.is_empty(), "应为幂等: {again:?}");

    // 标签仍然可检索/聚合
    let tags = ctx.state.tags().expect("tags");
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"考研"), "tags: {tags:?}");
    assert!(names.contains(&"裸标签"), "tags: {tags:?}");
}
