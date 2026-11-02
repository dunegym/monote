//! M5 导入导出模块 IO（对应设计文档 4.5、5.7）。
//!
//! - 导入：UTF-8 严格校验 → GB18030 回退转码；二进制/损坏文件跳过并进报告；
//! - 单篇导出：Markdown / HTML（内联 base64 图片）；
//! - 整库导出：ZIP 快照（保持目录结构，排除 `.monote/`）；
//! - 复用给备份（M6 BKP）的库快照打包能力。
//!
//! 写入库内文件一律经 [`crate::core::vault::Vault`] 的原子写方法；本模块不更新索引
//! （由 watcher 事件自然触发，与外部编辑器放进来的文件同一条路径）。
//!
//! - PDF 导出由前端走系统打印管线（DD-6），不在核心实现。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::core::vault::Vault;
use crate::error::{code, AppError, Result};
use crate::types::{ExportResult, ImportReport, SkippedFile};

/// 导入单个文件或整个文件夹到库内 `target_dir`（库内相对路径，""=库根）。
/// 保留子目录结构；路径冲突自动 `<名>-导入副本-<ts>` 重命名。
///
/// 幂等/可重入：不维护跨调用状态；重复导入同一来源时按冲突命规则生成副本，
/// 不覆盖已有笔记。单个文件失败不中断批次，只记入 `skipped`。
pub fn import_path(vault: &Vault, src: &Path, target_dir: &str) -> Result<ImportReport> {
    let meta = std::fs::metadata(src)
        .map_err(|e| AppError::io_ctx(format!("无法读取导入源：{}", src.display()), &e))?;
    let target_dir = normalize_dir_rel(target_dir);

    // 提前校验目标目录合法（并创建）；非法路径尽早失败，避免导入一半才发现越界。
    if !target_dir.is_empty() {
        vault.create_folder(&target_dir)?;
    }

    let mut report = ImportReport {
        job_id: uuid::Uuid::new_v4().to_string(),
        imported: 0,
        target_dir: target_dir.clone(),
        transcoded: Vec::new(),
        skipped: Vec::new(),
    };

    if meta.is_file() {
        let file_name = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| AppError::invalid_arg("导入源文件名无效"))?;
        let note_name = note_file_name(&file_name);
        let rel = join_rel(&target_dir, &note_name);
        import_one(vault, src, &rel, &file_name, &mut report);
    } else if meta.is_dir() {
        let walker = walkdir::WalkDir::new(src)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                // 跳过隐藏目录（.monote/.trash/.git 等）与符号链接目录，避免导入库内部派生数据。
                if e.file_type().is_dir() {
                    return !e.file_name().to_string_lossy().starts_with('.');
                }
                true
            });
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    report.skipped.push(SkippedFile {
                        file: src.display().to_string(),
                        reason: format!("遍历失败：{e}"),
                    });
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let rel_src = match entry.path().strip_prefix(src) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let display = path_to_slash(rel_src);
            if !is_markdown(entry.path()) {
                // 文件夹导入只收 Markdown（图片等请走粘贴/附件通道，E-8）；
                // 但列入报告，让用户知道哪些文件没被导入（AC-016 的"跳过清单"）。
                report.skipped.push(SkippedFile {
                    file: display,
                    reason: "非 Markdown 文本（不导入）".into(),
                });
                continue;
            }
            let rel = join_rel(&target_dir, &display);
            import_one(vault, entry.path(), &rel, &display, &mut report);
        }
    } else {
        return Err(AppError::invalid_arg("仅支持导入 Markdown 文件或文件夹"));
    }

    Ok(report)
}

/// 单个文件的导入流程：读字节 → 二进制检测 → 编码解码 → 冲突消解 → 经 Vault 写库。
fn import_one(vault: &Vault, abs: &Path, desired_rel: &str, display: &str, report: &mut ImportReport) {
    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(e) => {
            report.skipped.push(SkippedFile { file: display.to_string(), reason: format!("读取失败：{e}") });
            return;
        }
    };
    if is_binary(&bytes) {
        report.skipped.push(SkippedFile { file: display.to_string(), reason: "二进制文件（含 NUL/控制字符）".into() });
        return;
    }
    let (text, transcoded) = match decode_text(&bytes) {
        Some(v) => v,
        None => {
            report.skipped.push(SkippedFile {
                file: display.to_string(),
                reason: "无法解码为文本（UTF-8 与 GB18030 均失败）".into(),
            });
            return;
        }
    };

    // 保证父目录存在（Vault 写入口不负责建目录）。
    if let Some((dir, _)) = split_rel(desired_rel) {
        if !dir.is_empty() {
            if let Err(e) = vault.create_folder(&dir) {
                report.skipped.push(SkippedFile {
                    file: display.to_string(),
                    reason: format!("创建目标目录失败：{}", err_text(&e)),
                });
                return;
            }
        }
    }

    let final_rel = match unique_rel(vault, desired_rel) {
        Ok(r) => r,
        Err(e) => {
            report.skipped.push(SkippedFile { file: display.to_string(), reason: err_text(&e) });
            return;
        }
    };
    if let Err(e) = vault.write_atomic(&final_rel, text.as_bytes()) {
        report.skipped.push(SkippedFile { file: display.to_string(), reason: format!("写入失败：{}", err_text(&e)) });
        return;
    }
    report.imported += 1;
    if transcoded {
        report.transcoded.push(final_rel);
    }
}

/// 解码导入文件字节：UTF-8 严格 → GB18030 回退；BOM 识别。
/// 返回 `(文本, 是否发生转码)`；无法解码返回 None（调用方跳过并记录原因）。
///
/// 三级检测（设计 4.5 `EncodingSniffer`）：
/// 1. BOM：UTF-8 BOM 去 BOM 后按 UTF-8 处理；UTF-16/UTF-32 BOM 不在 v1 支持范围（返回 None）；
/// 2. UTF-8 严格校验（`std::str::from_utf8`）；
/// 3. GB18030 回退（含 4 字节区），解码报错则判定为损坏/二进制。
pub fn decode_text(bytes: &[u8]) -> Option<(String, bool)> {
    if bytes.is_empty() {
        return Some((String::new(), false));
    }
    // 含 NUL 的字节不按文本处理（UTF-16 支持范围外，且此类文件由 is_binary 先拦截）。
    if bytes.contains(&0) {
        return None;
    }
    // UTF-16/UTF-32 BOM：v1 不导入，避免被 GB18030 误解码成乱码。
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return None;
    }
    // UTF-8 BOM（EF BB BF）：去 BOM，仍按 UTF-8 处理（BOM 去除不算转码）。
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(body) {
        return Some((s.to_string(), false));
    }
    // GB18030 回退（FR-401：非 UTF-8 中文文件转码导入并标记）。
    let (decoded, _encoding, had_errors) = encoding_rs::GB18030.decode(bytes);
    if had_errors {
        return None;
    }
    let mut text = decoded.into_owned();
    if text.starts_with('\u{FEFF}') {
        text.remove(0);
    }
    Some((text, true))
}

/// 是否二进制文件（含 NUL 字节或大面积非法控制字符）。
///
/// 采样前 8 KB：出现 NUL 即判二进制；控制字符（除 TAB/LF/CR）占比 > 10% 亦判二进制。
/// GB18030/UTF-8 多字节序列的高位字节不会被误判。
pub fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.contains(&0) {
        return true;
    }
    let control = sample
        .iter()
        .filter(|&&b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r' && b != 0x0C)
        .count();
    control * 100 / sample.len() > 10
}

/// 导出单篇 HTML（单文件，图片 base64 内联）。`rendered_html` 为前端渲染的正文 HTML。
///
/// - 完整 HTML 文档（`<!doctype html>` + `<meta charset="utf-8">` + 内联 CSS），离线可看；
/// - 相对 `<img src="...">` 指向库内图片时，经 [`Vault::read_file_bytes`] 读字节转 data URL；
///   网络图片 / 已是 data URL 的保持原样；读取失败的图片保持原 src（不中断导出）。
pub fn export_note_html(
    vault: &Vault,
    rel: &str,
    title: &str,
    rendered_html: &str,
    out_path: &Path,
) -> Result<ExportResult> {
    let body = inline_images(vault, rel, rendered_html)?;
    let html = build_html_document(title, &body);
    write_output(out_path, html.as_bytes())?;
    Ok(ExportResult {
        out_path: out_path.display().to_string(),
        size: html.as_bytes().len() as u64,
    })
}

/// 导出单篇 Markdown（保持原文件内容，UTF-8 无 BOM / LF）。
pub fn export_note_md(vault: &Vault, rel: &str, out_path: &Path) -> Result<ExportResult> {
    let (content, _mtime) = vault.read_note_raw(rel)?;
    // 统一换行为 LF、去 UTF-8 BOM（NFR-107：标准 UTF-8 无 BOM .md）。
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized.strip_prefix('\u{FEFF}').unwrap_or(&normalized).to_string();
    write_output(out_path, normalized.as_bytes())?;
    Ok(ExportResult {
        out_path: out_path.display().to_string(),
        size: normalized.as_bytes().len() as u64,
    })
}

/// 整库导出 ZIP（保持目录结构，排除 `.monote/` 与备份包自身）。
pub fn export_vault_zip(vault: &Vault, out_path: &Path) -> Result<ExportResult> {
    let size = zip_snapshot(vault.root(), out_path)?;
    Ok(ExportResult {
        out_path: out_path.display().to_string(),
        size,
    })
}

/// 通用库快照 ZIP 打包（M5 导出与 M6 备份复用）：
/// 包含笔记 + attachments + `.trash/`（含 meta.json），排除 `.monote/`。
///
/// - 目录条目一并写入，保证解压后目录结构与库一致（AC-018）；
/// - 排除崩溃残留 `*.tmp-*` 与目标 zip 自身（备份目录可能位于库内）；
/// - deflate 压缩；返回生成的 zip 文件字节数。
pub fn zip_snapshot(root: &Path, out_path: &Path) -> Result<u64> {
    if !root.is_dir() {
        return Err(AppError::invalid_arg(format!("库目录不存在：{}", root.display())));
    }
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::io_ctx(format!("创建输出目录失败：{}", parent.display()), &e))?;
        }
    }

    let out_abs = lexical_abs(out_path);
    let file = std::fs::File::create(out_path)
        .map_err(|e| AppError::io_ctx(format!("创建 ZIP 失败：{}", out_path.display()), &e))?;
    let mut writer = zip::ZipWriter::new(file);
    let file_opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let dir_opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);

    let mut wrote = 0u32;
    let walker = walkdir::WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker {
        let entry = entry.map_err(|e| {
            AppError::new(code::IO_ERROR, "扫描库目录失败").with_detail(e.to_string())
        })?;
        if entry.depth() == 0 {
            continue;
        }
        let rel = match entry.path().strip_prefix(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let rel_slash = path_to_slash(rel);
        if snapshot_excluded(&rel_slash) {
            continue;
        }
        if entry.file_type().is_symlink() {
            continue; // 不跟随符号链接，避免把库外内容打进快照/循环
        }
        if entry.file_type().is_dir() {
            writer
                .add_directory(format!("{rel_slash}/"), dir_opts)
                .map_err(zip_err)?;
            wrote += 1;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if lexical_abs(entry.path()) == out_abs {
            continue; // 目标 zip 自身（备份/导出目录位于库内时）
        }
        let mut input = std::fs::File::open(entry.path()).map_err(|e| {
            AppError::io_ctx(format!("读取文件失败：{}", entry.path().display()), &e)
        })?;
        writer.start_file(rel_slash, file_opts).map_err(zip_err)?;
        std::io::copy(&mut input, &mut writer).map_err(|e| {
            AppError::io_ctx(format!("写入 ZIP 失败：{}", entry.path().display()), &e)
        })?;
        wrote += 1;
    }
    writer.finish().map_err(zip_err)?;
    let _ = wrote;
    let size = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    Ok(size)
}

// ===========================================================================
// 私有辅助
// ===========================================================================

fn zip_err(e: zip::result::ZipError) -> AppError {
    AppError::new(code::IO_ERROR, "ZIP 打包失败").with_detail(e.to_string())
}

fn err_text(e: &AppError) -> String {
    match &e.detail {
        Some(d) => format!("{}（{d}）", e.msg),
        None => e.msg.clone(),
    }
}

/// 快照排除规则：任意层级 `.monote` 目录、崩溃残留临时文件。
fn snapshot_excluded(rel_slash: &str) -> bool {
    if rel_slash.split('/').any(|part| part == ".monote") {
        return true;
    }
    let name = rel_slash.rsplit('/').next().unwrap_or(rel_slash);
    name.contains(".tmp-")
}

/// 词法绝对路径（不解析符号链接，仅用于“是否同一个目标文件”的比较）。
fn lexical_abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Markdown 判定（`.md` / `.markdown`，大小写不敏感）。
fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("md") | Some("markdown")
    )
}

/// 单文件导入时的目标文件名：非 Markdown 扩展名统一改为 `.md`（设计 E-8）。
fn note_file_name(name: &str) -> String {
    let p = Path::new(name);
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()) {
        Some(ext) if ext == "md" || ext == "markdown" => name.to_string(),
        Some(_) => {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "导入笔记".into());
            format!("{stem}.md")
        }
        None => format!("{name}.md"),
    }
}

/// 规范化库内相对目录（去首尾空白与 `/`；空串表示库根）。
fn normalize_dir_rel(target_dir: &str) -> String {
    let t = target_dir.trim().trim_matches('/').replace('\\', "/");
    let parts: Vec<&str> = t.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    parts.join("/")
}

fn path_to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// 拼接两个库内相对路径（`/` 分隔）。
fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.trim_start_matches('/').to_string()
    } else {
        format!("{dir}/{}", name.trim_start_matches('/'))
    }
}

/// 拆分为 `(目录, 文件名)`；无目录时目录为空串。
fn split_rel(rel: &str) -> Option<(String, String)> {
    match rel.rsplit_once('/') {
        Some((d, n)) => Some((d.to_string(), n.to_string())),
        None => Some((String::new(), rel.to_string())),
    }
}

/// 目标路径冲突检测：存在时返回 `<stem>-导入副本-<YYYYMMDD-HHmmss>.<ext>`（必要时追加序号）。
fn unique_rel(vault: &Vault, rel: &str) -> Result<String> {
    if !vault_rel_exists(vault, rel) {
        return Ok(rel.to_string());
    }
    let (dir, name) = split_rel(rel).ok_or_else(|| AppError::invalid_arg("目标路径无效"))?;
    let (stem, ext) = split_name_ext(&name);
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    for n in 0..1000u32 {
        let base = if n == 0 {
            format!("{stem}-导入副本-{ts}{ext}")
        } else {
            format!("{stem}-导入副本-{ts}-{n}{ext}")
        };
        let cand = join_rel(&dir, &base);
        if !vault_rel_exists(vault, &cand) {
            return Ok(cand);
        }
    }
    Err(AppError::new(code::IO_ERROR, "无法生成不冲突的导入路径"))
}

fn split_name_ext(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    }
}

fn vault_rel_exists(vault: &Vault, rel: &str) -> bool {
    vault.abs_path(rel).map(|abs| abs.exists()).unwrap_or(false)
}

/// 原子写导出目标文件（同目录临时文件 + rename），失败清理临时文件。
fn write_output(out_path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::io_ctx(format!("创建导出目录失败：{}", parent.display()), &e))?;
        }
    }
    let file_name = out_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::invalid_arg("导出路径无效"))?;
    let tmp = out_path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let write_all = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = write_all() {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("写导出文件失败：{}", out_path.display()), &e));
    }
    if let Err(e) = std::fs::rename(&tmp, out_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::io_ctx(format!("写导出文件失败：{}", out_path.display()), &e));
    }
    Ok(())
}

/// HTML 文档骨架（内联 CSS，单文件离线可看；亮色主题，打印友好）。
fn build_html_document(title: &str, body: &str) -> String {
    let title_esc = escape_html(title);
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"generator\" content=\"MoNote\">\n\
<title>{title_esc}</title>\n<style>\n{}\n</style>\n</head>\n\
<body>\n<article class=\"monote-note\">\n<h1 class=\"note-title\">{title_esc}</h1>\n{body}\n</article>\n</body>\n</html>\n",
        export_css()
    )
}

fn export_css() -> &'static str {
    r#"html{background:#f6f7f9;}
body{margin:0;padding:32px 16px;font-family:-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;color:#24292f;background:#f6f7f9;line-height:1.75;}
.monote-note{max-width:820px;margin:0 auto;background:#fff;padding:40px 48px;border-radius:8px;box-shadow:0 1px 4px rgba(0,0,0,.08);}
.note-title{margin-top:0;font-size:2em;line-height:1.3;border-bottom:1px solid #e5e7eb;padding-bottom:.4em;}
h1,h2,h3,h4{line-height:1.35;margin:1.4em 0 .6em;}
p{margin:.6em 0;}a{color:#0969da;}
img{max-width:100%;height:auto;}
blockquote{margin:1em 0;padding:.2em 1em;border-left:4px solid #d0d7de;color:#57606a;background:#f6f8fa;}
code{font-family:Consolas,"Courier New",monospace;background:#f3f4f6;border-radius:4px;padding:.15em .35em;font-size:.92em;}
pre{background:#f6f8fa;border:1px solid #e5e7eb;border-radius:6px;padding:14px 16px;overflow:auto;}
pre code{background:none;padding:0;}
table{border-collapse:collapse;margin:1em 0;width:100%;}
th,td{border:1px solid #d0d7de;padding:6px 12px;text-align:left;}
th{background:#f6f8fa;}
hr{border:none;border-top:1px solid #e5e7eb;margin:1.6em 0;}
ul,ol{padding-left:1.6em;}li{margin:.2em 0;}
:target{scroll-margin-top:12px;}
@media print{body{background:#fff;padding:0;}.monote-note{box-shadow:none;padding:0;max-width:none;}}@media (prefers-color-scheme:dark){html{background:#0d1117;}body{background:#0d1117;color:#c9d1d9;}.monote-note{background:#161b22;box-shadow:none;}a{color:#58a6ff;}.note-title{border-color:#30363d;}code{background:#21262d;}pre{background:#0d1117;border-color:#30363d;}th{background:#21262d;}th,td{border-color:#30363d;}blockquote{background:#161b22;border-color:#30363d;color:#8b949e;}}"#
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 把正文 HTML 中的相对图片地址替换为库内图片的 base64 data URL。
fn inline_images(vault: &Vault, note_rel: &str, rendered_html: &str) -> Result<String> {
    let re = regex::Regex::new(r#"(?is)<img\b[^>]*?\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)')"#)
        .map_err(|e| AppError::new(code::IO_ERROR, "图片内联正则初始化失败").with_detail(e.to_string()))?;
    let mut out = String::with_capacity(rendered_html.len());
    let mut last = 0usize;
    for caps in re.captures_iter(rendered_html) {
        let value_match = match (caps.get(1), caps.get(2)) {
            (Some(a), _) => a,
            (_, Some(b)) => b,
            _ => continue,
        };
        let src = value_match.as_str();
        let replacement = match inline_image_data_url(vault, note_rel, src) {
            Some(url) => url,
            None => continue,
        };
        out.push_str(&rendered_html[last..value_match.start()]);
        out.push_str(&replacement);
        last = value_match.end();
    }
    out.push_str(&rendered_html[last..]);
    Ok(out)
}

/// 计算单个图片 src 的 data URL；非库内相对图片/读取失败返回 None（保持原样）。
fn inline_image_data_url(vault: &Vault, note_rel: &str, src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("data:")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with("mailto:")
        || lower.starts_with('#')
    {
        return None;
    }
    for rel in image_candidates(note_rel, src) {
        let Some(mime) = image_mime(&rel) else { continue };
        if let Ok(bytes) = vault.read_file_bytes(&rel) {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Some(format!("data:{mime};base64,{encoded}"));
        }
    }
    None
}

/// 图片候选库内路径（按优先级）：
/// 1. 以笔记所在目录为基准（Markdown 标准语义）；
/// 2. 以库根为基准（本应用粘贴图片插入的是库内相对路径，笔记在子目录时用此回退）；
/// 均同时尝试原始写法与百分号解码写法（前端渲染可能把空格/中文编码）。
fn image_candidates(note_rel: &str, src: &str) -> Vec<String> {
    // Markdown 图片地址可能带 #锚点 / ?查询；图片读取时去掉。
    let src = src.split(['#', '?']).next().unwrap_or(src).trim();
    let decoded = percent_decode(src);
    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: Option<String>| {
        if let Some(c) = candidate {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    };
    push(resolve_vault_rel(note_rel, src));
    push(resolve_vault_rel(note_rel, &decoded));
    push(resolve_vault_rel("", src));
    push(resolve_vault_rel("", &decoded));
    out
}

/// 简单的百分号解码（仅用于图片相对路径，非法序列原样保留）。
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 以笔记所在目录为基准解析图片相对路径 → 库内相对路径（拒绝 `..` 越界）。
fn resolve_vault_rel(note_rel: &str, src: &str) -> Option<String> {
    let note_dir = note_rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let combined = if src.starts_with('/') {
        src.trim_start_matches('/').to_string()
    } else if note_dir.is_empty() {
        src.to_string()
    } else {
        format!("{note_dir}/{src}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return None; // 越过库根
                }
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn image_mime(rel: &str) -> Option<&'static str> {
    let ext = Path::new(rel).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        _ => return None,
    })
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn read_zip_entries(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).expect("open zip");
        let archive = zip::ZipArchive::new(file).expect("zip archive");
        archive.file_names().map(|s| s.to_string()).collect()
    }

    fn read_zip_file(path: &Path, name: &str) -> Vec<u8> {
        let file = std::fs::File::open(path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let mut f = archive.by_name(name).expect("entry exists");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read entry");
        buf
    }

    #[test]
    fn decode_text_utf8_plain() {
        let (text, transcoded) = decode_text("# 标题\n正文".as_bytes()).expect("utf8 decodes");
        assert_eq!(text, "# 标题\n正文");
        assert!(!transcoded);
    }

    #[test]
    fn decode_text_utf8_bom_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("笔记".as_bytes());
        let (text, transcoded) = decode_text(&bytes).expect("bom utf8 decodes");
        assert_eq!(text, "笔记");
        assert!(!transcoded, "UTF-8 BOM 去除不算转码");
    }

    #[test]
    fn decode_text_gb18030_fallback() {
        // "中文笔记" 的 GB18030 字节
        let gb: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB1, 0xCA, 0xBC, 0xC7];
        let (text, transcoded) = decode_text(gb).expect("gb18030 decodes");
        assert_eq!(text, "中文笔记");
        assert!(transcoded, "GB18030 回退应标记为转码");
    }

    #[test]
    fn decode_text_invalid_and_binary() {
        // 0xFF 在 GB18030 中不是合法起始字节
        assert!(decode_text(&[0xFF, 0xFF, 0xFF]).is_none());
        // 含 NUL 的二进制内容不按文本解码
        assert!(decode_text(&[0x89, 0x50, 0x4E, 0x47, 0x00, 0x01]).is_none());
        // UTF-16 LE BOM（超出 v1 支持范围）
        assert!(decode_text(&[0xFF, 0xFE, 0x41, 0x00]).is_none());
    }

    #[test]
    fn is_binary_detects_nul_and_controls() {
        assert!(!is_binary("普通文本\n第二行\t制表".as_bytes()));
        assert!(is_binary(&[0x00, 0x01, 0x02, 0x03]));
        assert!(is_binary(&[0x01; 100]));
        assert!(!is_binary(&[0xEF, 0xBB, 0xBF, b'a']));
    }

    #[test]
    fn zip_snapshot_includes_notes_trash_attachments_excludes_monote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::create_dir_all(root.join(".trash")).unwrap();
        std::fs::create_dir_all(root.join(".monote")).unwrap();
        std::fs::write(root.join("notes/a.md"), "# A\n").unwrap();
        std::fs::write(root.join("attachments/img.png"), [0x89u8, 0x50, 0x4E, 0x47]).unwrap();
        std::fs::write(root.join(".trash/meta.json"), "[]").unwrap();
        std::fs::write(root.join(".monote/index.db"), "db").unwrap();
        std::fs::write(root.join("note.tmp-123"), "residue").unwrap();

        let out = dir.path().join("export.zip");
        let size = zip_snapshot(&root, &out).expect("zip succeeds");
        assert!(size > 0);
        assert_eq!(size, std::fs::metadata(&out).unwrap().len());

        let names = read_zip_entries(&out);
        assert!(names.contains(&"notes/a.md".to_string()), "names={names:?}");
        assert!(names.contains(&"attachments/img.png".to_string()));
        assert!(names.contains(&".trash/meta.json".to_string()));
        assert!(!names.iter().any(|n| n.starts_with(".monote")), "必须排除 .monote");
        assert!(!names.iter().any(|n| n.contains(".tmp-")), "必须排除临时残留");
        assert_eq!(read_zip_file(&out, "notes/a.md"), b"# A\n".to_vec());
    }

    #[test]
    fn zip_snapshot_excludes_target_zip_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        let out = root.join("snapshot.zip");
        zip_snapshot(&root, &out).expect("zip succeeds");
        let names = read_zip_entries(&out);
        assert!(
            !names.iter().any(|n| n.ends_with("snapshot.zip")),
            "目标 zip 自身不能被收进快照：names={names:?}"
        );
        assert!(names.contains(&"a.md".to_string()));
    }

    #[test]
    fn export_result_size_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("sub/out.md");
        write_output(&out, b"hello").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"hello".to_vec());
        // 覆盖已有文件（原子替换）不应失败
        write_output(&out, b"hello2").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"hello2".to_vec());
    }

    #[test]
    fn html_resolve_image_relative_path() {
        assert_eq!(resolve_vault_rel("docs/a.md", "img.png").as_deref(), Some("docs/img.png"));
        assert_eq!(resolve_vault_rel("a.md", "attachments/img.png").as_deref(), Some("attachments/img.png"));
        assert_eq!(resolve_vault_rel("docs/a.md", "../attachments/x.png").as_deref(), Some("attachments/x.png"));
        assert_eq!(resolve_vault_rel("a.md", "../../etc/passwd"), None);
        assert_eq!(resolve_vault_rel("a.md", "/attachments/x.png").as_deref(), Some("attachments/x.png"));
    }

    #[test]
    fn image_candidates_cover_note_relative_and_vault_root() {
        // 笔记在子目录：先按笔记相对解析，再回退库根相对（本应用粘贴附件插入的是库内相对路径）
        let cands = image_candidates("docs/a.md", "attachments/IMG-1.png");
        assert_eq!(cands[0], "docs/attachments/IMG-1.png");
        assert!(cands.contains(&"attachments/IMG-1.png".to_string()), "cands={cands:?}");

        // ../ 相对路径解析后即库根相对 → 去重
        assert_eq!(
            image_candidates("docs/a.md", "../attachments/x.png"),
            vec!["attachments/x.png".to_string()]
        );
        // 以 / 开头 → 库根
        assert_eq!(
            image_candidates("docs/a.md", "/attachments/x.png"),
            vec!["attachments/x.png".to_string()]
        );
        // 百分号编码（空格/中文）
        let cands = image_candidates("docs/a.md", "attachments/IMG%20x.png");
        assert!(cands.contains(&"docs/attachments/IMG x.png".to_string()), "cands={cands:?}");
        assert!(cands.contains(&"attachments/IMG x.png".to_string()), "cands={cands:?}");
        // #锚点 / ?query 先剥掉
        assert_eq!(
            image_candidates("a.md", "attachments/x.png#frag"),
            vec!["attachments/x.png".to_string()]
        );
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("bad%2"), "bad%2", "非法百分号序列原样保留");
    }

    #[test]
    fn html_document_has_charset_and_inline_style() {
        let html = build_html_document("我的<笔记>", "<p>正文</p>");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<meta charset=\"utf-8\">"));
        assert!(html.contains("<style>"));
        assert!(html.contains("我的&lt;笔记&gt;"), "标题需 HTML 转义");
        assert!(html.contains("<p>正文</p>"));
    }

    #[test]
    fn conflict_name_helper_generates_import_copy() {
        let (stem, ext) = split_name_ext("note.md");
        assert_eq!(stem, "note");
        assert_eq!(ext, ".md");
        assert_eq!(note_file_name("readme.txt"), "readme.md");
        assert_eq!(note_file_name("NOTE.MD"), "NOTE.MD");
        assert_eq!(normalize_dir_rel("/docs/sub/"), "docs/sub");
        assert_eq!(join_rel("docs", "a.md"), "docs/a.md");
        assert_eq!(join_rel("", "a.md"), "a.md");
    }
}
