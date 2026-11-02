//! M3 链接解析模块（对应设计文档 4.3、5.2）。
//!
//! 职责：front-matter 解析/维护、双链解析、标签提取、链接改写计划、内容 hash。
//! 本模块是**纯函数优先**的（NFR-110：便于单测），不直接做文件 IO。

use std::hash::Hasher;

use crate::types::ParsedLink;
use crate::types::ParsedNote;

/// front-matter 解析结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    /// 原始行（用于维护时保留用户字段）
    pub lines: Vec<String>,
}

// ===========================================================================
// front-matter：拆分 / 解析 / 维护
// ===========================================================================

/// 拆分 front-matter 与正文。返回 `(front_matter_raw, body)`；
/// 无 front-matter 时返回 `(None, content)`。
///
/// 约定：文件以独占一行的 `---` 开始，且存在后续独占一行的 `---` 结束标记
/// （未闭合的 `---` 视为普通正文，避免把半截文件误当元数据）。返回的 raw
/// 不含首尾 `---` 行，body 从结束标记行之后开始。
pub fn split_front_matter(content: &str) -> (Option<&str>, &str) {
    match front_matter_bounds(content) {
        Some((raw_start, raw_end, body_start)) => {
            (Some(&content[raw_start..raw_end]), &content[body_start..])
        }
        None => (None, content),
    }
}

/// 定位 front-matter：`(raw_start, raw_end, body_start)`（字节偏移）。
fn front_matter_bounds(content: &str) -> Option<(usize, usize, usize)> {
    let (_, first_end, first_line) = next_line(content, 0)?;
    let first = first_line.trim_start_matches('\u{feff}').trim();
    if first != "---" {
        return None;
    }
    let raw_start = first_end;
    let mut pos = first_end;
    while let Some((line_start, line_end, line)) = next_line(content, pos) {
        if line.trim() == "---" {
            return Some((raw_start, line_start, line_end));
        }
        pos = line_end;
    }
    None
}

/// 读取 `pos` 处的一行：返回 `(行起始, 下一行起始, 不含行尾换行的行文本)`。
fn next_line(content: &str, pos: usize) -> Option<(usize, usize, &str)> {
    if pos >= content.len() {
        return None;
    }
    let rest = &content[pos..];
    let (line, next) = match rest.find('\n') {
        Some(i) => (&rest[..i], pos + i + 1),
        None => (rest, content.len()),
    };
    let line = line.strip_suffix('\r').unwrap_or(line);
    Some((pos, next, line))
}

/// 解析 front-matter 原始文本（不含首尾 `---`）。
///
/// 只解析本应用用到的少量 YAML 子集（`title` / `tags` / `created` / `updated`），
/// 其余字段原样保存在 [`FrontMatter::lines`] 中，供维护时原样写回。
pub fn parse_front_matter(raw: &str) -> FrontMatter {
    let mut fm = FrontMatter {
        lines: raw.lines().map(str::to_string).collect(),
        ..FrontMatter::default()
    };
    let mut in_tags_block = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 块状 tags 列表：`tags:` 之后每行 `- 标签`
        if in_tags_block {
            if let Some(item) = trimmed.strip_prefix("- ") {
                push_tag(&mut fm.tags, item);
                continue;
            }
            if trimmed == "-" {
                continue;
            }
            in_tags_block = false;
        }
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        match key.as_str() {
            "title" => {
                let v = parse_scalar(value);
                fm.title = if v.is_empty() { None } else { Some(v) };
            }
            "created" => fm.created = non_empty(parse_scalar(value)),
            "updated" => fm.updated = non_empty(parse_scalar(value)),
            "tags" => {
                let v = parse_scalar(value);
                if v.is_empty() {
                    in_tags_block = true;
                } else if let Some(list) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    for item in list.split(',') {
                        push_tag(&mut fm.tags, item);
                    }
                } else {
                    push_tag(&mut fm.tags, &v);
                }
            }
            _ => {}
        }
    }
    fm
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn push_tag(tags: &mut Vec<String>, raw: &str) {
    if let Some(tag) = normalize_tag(raw) {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
}

/// 拆出 `key: value`（只认顶层键；键仅允许 ASCII 字母/数字/`_`/`-`）。
fn split_key_value(line: &str) -> Option<(String, &str)> {
    let idx = line.find(':')?;
    let key = line[..idx].trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((key.to_string(), &line[idx + 1..]))
}

/// 解析标量：去 YAML 行内注释、去首尾空白、去成对引号。
fn parse_scalar(value: &str) -> String {
    let value = strip_yaml_comment(value);
    let value = value.trim();
    if value.len() >= 2 {
        if let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return inner.replace("\\\"", "\"").replace("\\\\", "\\");
        }
        if let Some(inner) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            return inner.replace("''", "'");
        }
    }
    value.to_string()
}

/// 去掉 YAML 行内注释（引号外的 ` #` 起）。
fn strip_yaml_comment(value: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_whitespace = true;
    for (idx, c) in value.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '#' && prev_whitespace {
                    return &value[..idx];
                }
            }
        }
        prev_whitespace = c.is_whitespace();
    }
    value
}

/// 读取内容中的 title/tags/created/updated（无 front-matter 时为空）。
pub fn read_meta(content: &str) -> FrontMatter {
    let (raw, _) = split_front_matter(content);
    raw.map(parse_front_matter).unwrap_or_default()
}

/// 维护 front-matter：确保 `updated` 字段为 `now_iso`；无 front-matter 时补建
/// （title 取 `fallback_title`、created=now_iso）。仅改动 front-matter 区，不动正文。
///
/// 已存在 front-matter 时保留用户字段（含未知字段与字段顺序），只替换 `updated`；
/// 缺失 `created` 时补写。设计文档 7.5：`updated` 每次系统写入都刷新，`created` 只维护一次。
pub fn touch_front_matter(content: &str, fallback_title: &str, now_iso: &str) -> String {
    match split_front_matter(content) {
        (Some(raw), body) => {
            let fm = parse_front_matter(raw);
            let mut out_lines: Vec<String> = Vec::new();
            let mut seen_created = fm.created.is_some();
            let mut seen_updated = false;
            for line in raw.lines() {
                if let Some((key, _)) = split_key_value(line) {
                    match key.as_str() {
                        // 系统维护字段：updated 覆盖、重复行丢弃
                        "updated" => {
                            if !seen_updated {
                                out_lines.push(format!("updated: {now_iso}"));
                                seen_updated = true;
                            }
                            continue;
                        }
                        // created 保留用户值（A.2：用户手改后以用户值为准）
                        "created" => {
                            seen_created = true;
                            out_lines.push(line.to_string());
                            continue;
                        }
                        _ => {}
                    }
                }
                out_lines.push(line.to_string());
            }
            if !seen_created {
                out_lines.push(format!("created: {now_iso}"));
            }
            if !seen_updated {
                out_lines.push(format!("updated: {now_iso}"));
            }
            let mut out = String::with_capacity(content.len() + 64);
            out.push_str("---\n");
            if !out_lines.is_empty() {
                out.push_str(&out_lines.join("\n"));
                out.push('\n');
            }
            out.push_str("---\n");
            out.push_str(body);
            out
        }
        (None, _) => {
            let mut title: String = fallback_title.replace(['\r', '\n'], " ");
            title = title.trim().to_string();
            let quoted = yaml_quote(&title);
            let mut out = String::with_capacity(content.len() + 128);
            out.push_str("---\n");
            out.push_str(&format!("title: {quoted}\n"));
            out.push_str(&format!("created: {now_iso}\n"));
            out.push_str(&format!("updated: {now_iso}\n"));
            out.push_str("---\n");
            if !content.is_empty() {
                // 与正文之间保留一个空行，符合 A.2 示例
                out.push('\n');
                out.push_str(content);
            }
            out
        }
    }
}

/// 需要时给 YAML 标量加双引号（含特殊字符/首尾空白/空串）。
fn yaml_quote(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.chars().any(|c| {
            matches!(
                c,
                ':' | '#'
                    | '"'
                    | '\''
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '@'
                    | '`'
                    | '%'
            )
        })
        || s.chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit() || c == '-' || c == '?');
    if needs_quote {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

// ===========================================================================
// 行内标签
// ===========================================================================

/// 从正文提取行内 `#标签`（跳过围栏代码块、行内代码、URL 片段与标题标记）。
pub fn extract_inline_tags(body: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let mut fence = FenceState::default();
    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if fence.update(line) || fence.inside() {
            continue;
        }
        if is_atx_heading(line) {
            continue;
        }
        let masked = mask_wikilinks(mask_inline_code(line));
        let mut i = 0;
        while i < masked.len() {
            if masked[i] == '#' && tag_can_start(&masked, i) {
                let mut k = i + 1;
                while k < masked.len()
                    && (masked[k].is_alphanumeric() || masked[k] == '_' || masked[k] == '-')
                {
                    k += 1;
                }
                if k > i + 1 {
                    let raw: String = masked[i + 1..k].iter().collect();
                    if let Some(tag) = normalize_tag(&raw) {
                        if !tags.contains(&tag) {
                            tags.push(tag);
                        }
                    }
                }
                i = k.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    tags
}

/// 把一行中的双链 `[[...]]` 整体替换为空格（标签提取不应把 `[[#小节]]` 的锚点当标签）。
fn mask_wikilinks(mut chars: Vec<char>) -> Vec<char> {
    for (start, end) in find_wikilinks(&chars) {
        // start/end 是内部文本区间，左右各含两个方括号
        for c in chars.iter_mut().take(end + 2).skip(start - 2) {
            *c = ' ';
        }
    }
    chars
}

/// `#` 前一个字符是否允许作为标签起始（避免 `C#`、URL 片段等误判）。
fn tag_can_start(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = chars[i - 1];
    if prev.is_whitespace() {
        return true;
    }
    !(prev.is_alphanumeric()
        || matches!(prev, '_' | '-' | '#' | '/' | '\\' | '&' | '%' | '.' | '~'))
}

/// 形如 `# 标题` 的 ATX 标题行（`#tag` 不是标题）。
fn is_atx_heading(line: &str) -> bool {
    let t = line.trim_start();
    let n = t.chars().take_while(|&c| c == '#').count();
    if n == 0 || n > 6 {
        return false;
    }
    match t.chars().nth(n) {
        None => true,
        Some(c) => c.is_whitespace(),
    }
}

// ===========================================================================
// 双链
// ===========================================================================

/// 解析全部双链：`[[目标]]`、`[[目标|别名]]`、`[[#标题]]`。
///
/// 行号为**传入文本内**的 0-based 行号；围栏代码块与行内代码中的 `[[...]]`
/// 不算链接。`[[#标题]]` 的 `target_name` 为 `#标题`（补充当前笔记标题由
/// [`parse_note`] 完成）。
pub fn parse_links(body: &str) -> Vec<ParsedLink> {
    let mut links = Vec::new();
    let mut fence = FenceState::default();
    for (line_no, raw_line) in body.split('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if fence.update(line) || fence.inside() {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let masked = mask_inline_code(line);
        for (start, end) in find_wikilinks(&masked) {
            let inner: String = chars[start..end].iter().collect();
            let Some(link) = parse_link_inner(&inner, line_no as u32) else {
                continue;
            };
            links.push(link);
        }
    }
    links
}

/// 在字符数组中查找 `[[...]]` 的内部区间（不处理嵌套，跳过 `\\[[` 转义）。
fn find_wikilinks(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '[' && chars[i + 1] == '[' {
            if i > 0 && chars[i - 1] == '\\' {
                i += 2;
                continue;
            }
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < chars.len() {
                if chars[j] == ']' && chars[j + 1] == ']' {
                    if j > 0 && chars[j - 1] == '\\' {
                        j += 2;
                        continue;
                    }
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            match close {
                Some(end) => {
                    out.push((i + 2, end));
                    i = end + 2;
                }
                None => i += 2,
            }
        } else {
            i += 1;
        }
    }
    out
}

/// 解析单个 `[[...]]` 的内部文本。
fn parse_link_inner(inner: &str, line: u32) -> Option<ParsedLink> {
    let (target_raw, alias) = match inner.find('|') {
        Some(idx) => {
            let alias = inner[idx + 1..].trim();
            (
                inner[..idx].trim(),
                if alias.is_empty() {
                    None
                } else {
                    Some(alias.to_string())
                },
            )
        }
        None => (inner.trim(), None),
    };
    if target_raw.is_empty() {
        return None;
    }
    let is_anchor = target_raw.starts_with('#');
    let target_name = if is_anchor {
        let heading = target_raw.trim_start_matches('#').trim();
        if heading.is_empty() {
            return None;
        }
        format!("#{heading}")
    } else {
        let name = normalize_target(target_raw);
        if name.is_empty() {
            return None;
        }
        name
    };
    Some(ParsedLink { target_name, alias, line, is_anchor })
}

/// 将内容中对 `old_name` 的双链改写为 `new_name`（保留别名与锚点），
/// 返回 `(新内容, 改写次数)`。只改双链，不碰普通文本、行内代码与围栏代码块。
///
/// 目标匹配以文件名（去扩展名）为准，忽略 ASCII 大小写；`[[目标#小节]]`
/// 形式的内部锚点会原样保留。
pub fn rewrite_links(content: &str, old_name: &str, new_name: &str) -> (String, u32) {
    let old = normalize_target(old_name);
    let new = normalize_target(new_name);
    if old.is_empty() || new.is_empty() {
        return (content.to_string(), 0);
    }
    let mut count = 0u32;
    let mut fence = FenceState::default();
    let mut out = String::with_capacity(content.len());
    for piece in content.split_inclusive('\n') {
        let (body, eol) = if let Some(b) = piece.strip_suffix("\r\n") {
            (b, "\r\n")
        } else if let Some(b) = piece.strip_suffix('\n') {
            (b, "\n")
        } else {
            (piece, "")
        };
        if fence.update(body) || fence.inside() {
            out.push_str(piece);
            continue;
        }
        let chars: Vec<char> = body.chars().collect();
        let masked = mask_inline_code(body);
        let links = find_wikilinks(&masked);
        if links.is_empty() {
            out.push_str(piece);
            continue;
        }
        let mut last = 0usize;
        for (start, end) in links {
            let inner: String = chars[start..end].iter().collect();
            if let Some(replaced) = rewrite_inner(&inner, &old, &new) {
                out.extend(chars[last..start].iter());
                out.push_str(&replaced);
                last = end;
                count += 1;
            }
        }
        out.extend(chars[last..].iter());
        out.push_str(eol);
    }
    (out, count)
}

/// 若 `inner` 的目标名与 `old` 匹配，返回替换后的内部文本（保留别名/锚点原文）。
fn rewrite_inner(inner: &str, old: &str, new: &str) -> Option<String> {
    let (target_raw, rest) = match inner.find('|') {
        Some(idx) => (&inner[..idx], &inner[idx..]), // rest 含 `|`，保留别名原文
        None => (inner, ""),
    };
    let target = target_raw.trim();
    if target.is_empty() || target.starts_with('#') {
        return None; // 纯锚点链接不指向其他笔记
    }
    let (name_part, anchor) = match target.find('#') {
        Some(idx) => (&target[..idx], &target[idx..]),
        None => (target, ""),
    };
    let normalized = normalize_target(name_part);
    if !normalized.eq_ignore_ascii_case(old) {
        return None;
    }
    Some(format!("{new}{anchor}{rest}"))
}

// ===========================================================================
// hash / 规范化 / 展示名 / 文件名
// ===========================================================================

/// 内容 hash（xxhash64，十六进制小写，seed = 0）。
pub fn content_hash(content: &str) -> String {
    let mut hasher = twox_hash::XxHash64::with_seed(0);
    hasher.write(content.as_bytes());
    format!("{:016x}", hasher.finish())
}

/// 标签规范化：去 `#`/首尾空白，仅允许 [中文/字母/数字/_/-]，非法返回 None。
pub fn normalize_tag(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut has_alnum = false;
    for c in trimmed.chars() {
        if c.is_alphanumeric() {
            has_alnum = true;
        } else if c != '_' && c != '-' {
            return None;
        }
    }
    if !has_alnum {
        return None;
    }
    Some(trimmed.to_string())
}

/// 链接目标名规范化：去首尾空白、去 `.md`/`.markdown` 后缀。
pub fn normalize_target(raw: &str) -> String {
    let s = raw.trim();
    let lower = s.to_ascii_lowercase();
    let stripped = if lower.ends_with(".markdown") {
        &s[..s.len() - ".markdown".len()]
    } else if lower.ends_with(".md") {
        &s[..s.len() - ".md".len()]
    } else {
        s
    };
    stripped.trim().to_string()
}

/// 由 front-matter title 与路径推导展示标题（title 缺省取文件名去扩展名）。
pub fn display_title(title: Option<&str>, path: &str) -> String {
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    let file_name = path
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(path);
    let stem = strip_md_ext(file_name).trim();
    if stem.is_empty() {
        "未命名".to_string()
    } else {
        stem.to_string()
    }
}

fn strip_md_ext(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".markdown") {
        &name[..name.len() - ".markdown".len()]
    } else if lower.ends_with(".md") {
        &name[..name.len() - ".md".len()]
    } else {
        name
    }
}

/// 把任意名称转为安全的文件名（去掉 Windows 非法字符与路径分隔符）。
pub fn safe_file_name(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() {
            continue;
        }
        if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            continue;
        }
        cleaned.push(c);
    }
    // Windows 不允许文件名以点/空格结尾
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    let mut name = if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "未命名".to_string()
    } else {
        cleaned
    };
    let stem = name.split('.').next().unwrap_or(name.as_str()).trim().to_string();
    if is_windows_reserved_name(&stem) {
        name = format!("_{name}");
    }
    name
}

fn is_windows_reserved_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

// ===========================================================================
// 完整解析
// ===========================================================================

/// 完整解析一篇笔记。
///
/// - `title`：front-matter title，缺省取文件名（去扩展名）；
/// - `tags`：front-matter 与行内标签合并去重（property 在前）；
/// - `links` 行号为**整篇内容**（含 front-matter）的 0-based 行号；
/// - `[[#标题]]` 的 target_name 补全为 `当前笔记标题#标题`（类型注释约定）。
pub fn parse_note(path: &str, content: &str, mtime: i64, size: u64) -> ParsedNote {
    let (raw, body) = split_front_matter(content);
    let meta = raw.map(parse_front_matter).unwrap_or_default();
    let title = display_title(meta.title.as_deref(), path);
    let line_offset = if raw.is_some() {
        content[..content.len() - body.len()].matches('\n').count() as u32
    } else {
        0
    };

    let mut links = parse_links(body);
    for link in &mut links {
        link.line = link.line.saturating_add(line_offset);
        if link.is_anchor {
            let heading = link.target_name.trim_start_matches('#');
            link.target_name = format!("{title}#{heading}");
        }
    }

    let inline_tags = extract_inline_tags(body);
    let property_tags = meta.tags.clone();
    let mut tags = property_tags.clone();
    for tag in &inline_tags {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }

    ParsedNote {
        path: path.to_string(),
        title,
        content: content.to_string(),
        front_matter: raw.map(str::to_string),
        tags,
        inline_tags,
        property_tags,
        links,
        created: meta.created,
        updated: meta.updated,
        hash: content_hash(content),
        mtime,
        size,
    }
}

// ===========================================================================
// 行扫描辅助：围栏代码块状态 / 行内代码遮罩
// ===========================================================================

/// 围栏代码块（``` 或 ~~~）状态机。
#[derive(Debug, Default, Clone, Copy)]
struct FenceState {
    marker: Option<(char, usize)>,
}

impl FenceState {
    fn inside(&self) -> bool {
        self.marker.is_some()
    }

    /// 若本行是围栏开始/结束行则更新状态并返回 true（该行本身不是内容行）。
    fn update(&mut self, line: &str) -> bool {
        let t = line.trim_start();
        let Some(c) = t.chars().next() else {
            return false;
        };
        if c != '`' && c != '~' {
            return false;
        }
        let run = t.chars().take_while(|&x| x == c).count();
        if run < 3 {
            return false;
        }
        match self.marker {
            None => {
                self.marker = Some((c, run));
                true
            }
            Some((marker, min_len)) if marker == c && run >= min_len => {
                self.marker = None;
                true
            }
            _ => false,
        }
    }
}

/// 把一行中的行内代码（反引号包裹）替换为空格，长度/位置不变。
fn mask_inline_code(line: &str) -> Vec<char> {
    let mut chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let mut run = 1;
        while i + run < n && chars[i + run] == '`' {
            run += 1;
        }
        let mut j = i + run;
        let mut close = None;
        while j < n {
            if chars[j] == '`' {
                let mut r2 = 1;
                while j + r2 < n && chars[j + r2] == '`' {
                    r2 += 1;
                }
                if r2 == run {
                    close = Some(j);
                    break;
                }
                j += r2;
            } else {
                j += 1;
            }
        }
        match close {
            Some(j) => {
                for c in chars.iter_mut().take(j + run).skip(i) {
                    *c = ' ';
                }
                i = j + run;
            }
            None => i += run,
        }
    }
    chars
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-02T10:00:00+08:00";

    // ---- front-matter 拆分/解析 -------------------------------------------

    #[test]
    fn split_front_matter_basic() {
        let (raw, body) = split_front_matter("---\ntitle: 标题\n---\n\n正文");
        assert_eq!(raw, Some("title: 标题\n"));
        assert_eq!(body, "\n正文");

        let (raw, body) = split_front_matter("正文 **粗体**");
        assert_eq!(raw, None);
        assert_eq!(body, "正文 **粗体**");

        // 未闭合的 --- 当作正文（避免把半截文件当元数据）
        let (raw, body) = split_front_matter("---\ntitle: x\n");
        assert_eq!(raw, None);
        assert_eq!(body, "---\ntitle: x\n");

        // 空 front-matter
        let (raw, body) = split_front_matter("---\n---\nbody");
        assert_eq!(raw, Some(""));
        assert_eq!(body, "body");

        // CRLF
        let (raw, body) = split_front_matter("---\r\ntitle: t\r\n---\r\nbody");
        assert_eq!(raw, Some("title: t\r\n"));
        assert_eq!(body, "body");
    }

    #[test]
    fn parse_front_matter_fields() {
        let raw = "title: 操作系统-第3章\ntags: [考研, 操作系统]\ncreated: 2026-09-18T10:30:00+08:00\nupdated: 2026-09-18T21:05:12+08:00\n";
        let fm = parse_front_matter(raw);
        assert_eq!(fm.title.as_deref(), Some("操作系统-第3章"));
        assert_eq!(fm.tags, vec!["考研".to_string(), "操作系统".to_string()]);
        assert_eq!(fm.created.as_deref(), Some("2026-09-18T10:30:00+08:00"));
        assert_eq!(fm.updated.as_deref(), Some("2026-09-18T21:05:12+08:00"));
        assert_eq!(fm.lines.len(), 4);
    }

    #[test]
    fn parse_front_matter_block_tags_and_quotes() {
        let fm = parse_front_matter("tags:\n  - 考研\n  - 操作系统\ntitle: \"含: 冒号 # 的标题\"");
        assert_eq!(fm.tags, vec!["考研".to_string(), "操作系统".to_string()]);
        assert_eq!(fm.title.as_deref(), Some("含: 冒号 # 的标题"));

        // 行内注释与单引号
        let fm = parse_front_matter("title: 标题 # 缺省时取文件名\nmood: 'happy'");
        assert_eq!(fm.title.as_deref(), Some("标题"));
        // 未知字段保留在 lines 中
        assert!(fm.lines.iter().any(|l| l == "mood: 'happy'"));
        // C# 不是注释
        let fm = parse_front_matter("title: C# 入门");
        assert_eq!(fm.title.as_deref(), Some("C# 入门"));
    }

    #[test]
    fn read_meta_without_front_matter() {
        let fm = read_meta("# 只有正文");
        assert_eq!(fm.title, None);
        assert!(fm.tags.is_empty());
        assert!(fm.lines.is_empty());
    }

    #[test]
    fn touch_front_matter_creates_when_absent() {
        let out = touch_front_matter("正文内容", "新笔记", NOW);
        assert!(out.starts_with("---\ntitle: 新笔记\ncreated: 2026-11-02T10:00:00+08:00\nupdated: 2026-11-02T10:00:00+08:00\n---\n\n"));
        assert!(out.ends_with("正文内容"));

        let fm = read_meta(&out);
        assert_eq!(fm.title.as_deref(), Some("新笔记"));
        assert_eq!(fm.created.as_deref(), Some(NOW));
        assert_eq!(fm.updated.as_deref(), Some(NOW));

        // 空正文：不产生多余空行，且再次 touch 稳定
        let empty = touch_front_matter("", "空笔记", NOW);
        let again = touch_front_matter(&empty, "空笔记", NOW);
        assert_eq!(empty, again);
    }

    #[test]
    fn touch_front_matter_preserves_user_fields() {
        let content = "---\ntitle: 旧标题\nmood: happy\ncreated: 2020-01-01T00:00:00+08:00\nupdated: 2020-01-02T00:00:00+08:00\n---\n\n正文 **加粗**\n";
        let out = touch_front_matter(content, "忽略此值", NOW);
        assert!(out.contains("title: 旧标题"));
        assert!(out.contains("mood: happy"));
        // created 保留用户值，updated 刷新
        assert!(out.contains("created: 2020-01-01T00:00:00+08:00"));
        assert!(out.contains(&format!("updated: {NOW}")));
        assert!(!out.contains("2020-01-02"));
        assert_eq!(out.matches("updated:").count(), 1);
        // 正文（含尾随换行）逐字保留
        let (_, body) = split_front_matter(&out);
        assert_eq!(body, "\n正文 **加粗**\n");
    }

    #[test]
    fn touch_front_matter_adds_created_when_missing() {
        let content = "---\ntitle: 标题\n---\nbody";
        let out = touch_front_matter(content, "标题", NOW);
        assert_eq!(read_meta(&out).created.as_deref(), Some(NOW));
        assert_eq!(read_meta(&out).title.as_deref(), Some("标题"));
        // 只改 front-matter，正文不变
        assert_eq!(split_front_matter(&out).1, "body");
    }

    // ---- 行内标签 ---------------------------------------------------------

    #[test]
    fn extract_inline_tags_skips_code_and_headings() {
        let body = "# 一级标题\n普通 #考研 笔记 #2024\n```\n#代码内标签\n```\n`#行内代码` 之后 #操作系统入门\n见 https://example.com/#anchor\n";
        let tags = extract_inline_tags(body);
        assert_eq!(tags, vec!["考研", "2024", "操作系统入门"]);
    }

    #[test]
    fn extract_inline_tags_dedup_and_edge_cases() {
        let tags = extract_inline_tags("#a #a #b-1 #c_d\nC#语言 与 a#b\n");
        assert_eq!(tags, vec!["a", "b-1", "c_d"]);
        // 空标签、只有连字符的标签不提取
        assert!(extract_inline_tags("# #-- #\n").is_empty());
        // 中文标点后的标签可提取
        assert_eq!(extract_inline_tags("（#标签）\n"), vec!["标签"]);
    }

    // ---- 双链 -------------------------------------------------------------

    #[test]
    fn parse_links_variants() {
        let body = "第一行 [[目标笔记]]\n第二行 [[目标|别名]] 与 [[#标题一]]\n```\n[[代码内]]\n```\n转义 \\[[不解析]] 但与 [[另一个.md|显示]]";
        let links = parse_links(body);
        assert_eq!(links.len(), 4);

        assert_eq!(links[0].target_name, "目标笔记");
        assert_eq!(links[0].alias, None);
        assert_eq!(links[0].line, 0);
        assert!(!links[0].is_anchor);

        assert_eq!(links[1].target_name, "目标");
        assert_eq!(links[1].alias.as_deref(), Some("别名"));
        assert_eq!(links[1].line, 1);

        assert_eq!(links[2].target_name, "#标题一");
        assert!(links[2].is_anchor);
        assert_eq!(links[2].line, 1);

        // .md 后缀被规范化；行号 5
        assert_eq!(links[3].target_name, "另一个");
        assert_eq!(links[3].alias.as_deref(), Some("显示"));
        assert_eq!(links[3].line, 5);
    }

    #[test]
    fn parse_links_ignores_inline_code_and_empty() {
        let links = parse_links("`[[行内代码]]` 与 [[]] 与 [[ |仅有别名]]\n有效 [[笔记]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_name, "笔记");
        assert_eq!(links[0].line, 1);
    }

    #[test]
    fn parse_links_multiple_per_line() {
        let links = parse_links("[[一]]和[[二|乙]]和[[#三]]");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].line, 0);
        assert_eq!(links[1].target_name, "二");
        assert!(links[2].is_anchor);
    }

    #[test]
    fn rewrite_links_keeps_alias_and_anchor() {
        let content = "[[目标]] [[目标|别名]] [[目标#锚点]] [[无关]]\n";
        let (out, n) = rewrite_links(content, "目标", "新目标");
        assert_eq!(n, 3);
        assert!(out.contains("[[新目标]]"));
        assert!(out.contains("[[新目标|别名]]"));
        assert!(out.contains("[[新目标#锚点]]"));
        assert!(out.contains("[[无关]]"));
        assert!(!out.contains("[[目标]]"));
    }

    #[test]
    fn rewrite_links_skips_code_and_normalizes_target() {
        let content = "`[[目标]]`\n```\n[[目标]]\n```\n[[目标.md]] 与 [[目标| 别名 ]]\n";
        let (out, n) = rewrite_links(content, "目标.md", "新名");
        assert_eq!(n, 2, "只改两处正文双链：{out}");
        // 行内代码与围栏代码块保持不变
        assert!(out.contains("`[[目标]]`"));
        assert!(out.contains("```\n[[目标]]\n```"));
        assert!(out.contains("[[新名]]"));
        assert!(out.contains("[[新名| 别名 ]]"));
    }

    #[test]
    fn rewrite_links_no_match_returns_original() {
        let content = "[[别的]] 普通目标 文字";
        let (out, n) = rewrite_links(content, "目标", "新目标");
        assert_eq!(n, 0);
        assert_eq!(out, content);
        // 空参数保护
        let (out, n) = rewrite_links(content, "", "x");
        assert_eq!(n, 0);
        assert_eq!(out, content);
    }

    #[test]
    fn rewrite_links_preserves_crlf() {
        let content = "a\r\n[[目标]]\r\nb";
        let (out, n) = rewrite_links(content, "目标", "新目标");
        assert_eq!(n, 1);
        assert_eq!(out, "a\r\n[[新目标]]\r\nb");
    }

    // ---- hash / 规范化 / 文件名 ------------------------------------------

    #[test]
    fn content_hash_is_stable() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("hello "));
        assert_ne!(content_hash("hello"), content_hash("Hello"));
        let h = content_hash("中文内容\n第二行");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(h.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn normalize_tag_rules() {
        assert_eq!(normalize_tag("#考研").as_deref(), Some("考研"));
        assert_eq!(normalize_tag("  操作系统-第3章  ").as_deref(), Some("操作系统-第3章"));
        assert_eq!(normalize_tag("tag_1").as_deref(), Some("tag_1"));
        assert_eq!(normalize_tag(""), None);
        assert_eq!(normalize_tag("#"), None);
        assert_eq!(normalize_tag("有 空格"), None);
        assert_eq!(normalize_tag("a/b"), None);
        assert_eq!(normalize_tag("---"), None);
    }

    #[test]
    fn normalize_target_rules() {
        assert_eq!(normalize_target("  目标.md "), "目标");
        assert_eq!(normalize_target("目标.markdown"), "目标");
        assert_eq!(normalize_target("a.MD"), "a");
        assert_eq!(normalize_target("目标"), "目标");
        assert_eq!(normalize_target("  "), "");
    }

    #[test]
    fn display_title_rules() {
        assert_eq!(display_title(Some("标题"), "dir/a.md"), "标题");
        assert_eq!(display_title(Some("   "), "dir/a.md"), "a");
        assert_eq!(display_title(None, "a/b/笔记.md"), "笔记");
        assert_eq!(display_title(None, "a/b/c.markdown"), "c");
        assert_eq!(display_title(None, "a\\b\\d.md"), "d");
        assert_eq!(display_title(None, ""), "未命名");
    }

    #[test]
    fn safe_file_name_rules() {
        assert_eq!(safe_file_name("a/b\\c:d*e?f\"g<h>i|j"), "abcdefghij");
        assert_eq!(safe_file_name("  名称.  "), "名称");
        assert_eq!(safe_file_name("CON"), "_CON");
        assert_eq!(safe_file_name("lpt9.md"), "_lpt9.md");
        assert_eq!(safe_file_name(""), "未命名");
        assert_eq!(safe_file_name(".."), "未命名");
        assert_eq!(safe_file_name("正常 名称"), "正常 名称");
    }

    // ---- 完整解析 ---------------------------------------------------------

    #[test]
    fn parse_note_merges_tags_and_offsets_lines() {
        let content = "---\ntitle: 我的笔记\ntags: [考研, 学习]\ncreated: 2026-01-01T00:00:00+08:00\nupdated: 2026-01-02T00:00:00+08:00\n---\n\n# 标题\n正文 #考研 #额外 引用 [[其他]] 与 [[#小节]]\n";
        let note = parse_note("docs/我的笔记.md", content, 123, 456);

        assert_eq!(note.title, "我的笔记");
        assert_eq!(note.property_tags, vec!["考研", "学习"]);
        assert_eq!(note.inline_tags, vec!["考研", "额外"]);
        assert_eq!(note.tags, vec!["考研", "学习", "额外"]);
        assert_eq!(note.created.as_deref(), Some("2026-01-01T00:00:00+08:00"));
        assert_eq!(note.updated.as_deref(), Some("2026-01-02T00:00:00+08:00"));
        assert_eq!(note.hash, content_hash(content));
        assert_eq!(note.mtime, 123);
        assert_eq!(note.size, 456);
        assert_eq!(note.path, "docs/我的笔记.md");
        assert_eq!(note.content, content);
        assert!(note.front_matter.as_deref().unwrap().contains("title: 我的笔记"));

        assert_eq!(note.links.len(), 2);
        assert_eq!(note.links[0].target_name, "其他");
        // 行号以整篇内容计（含 front-matter 的 6 行）
        assert_eq!(note.links[0].line, 8);
        assert!(note.links[1].is_anchor);
        assert_eq!(note.links[1].target_name, "我的笔记#小节");
    }

    #[test]
    fn parse_note_without_front_matter() {
        let content = "# 无元数据\n正文 [[链接]]\n";
        let note = parse_note("无元数据.md", content, 0, 0);
        assert_eq!(note.title, "无元数据");
        assert!(note.front_matter.is_none());
        assert!(note.property_tags.is_empty());
        assert_eq!(note.links.len(), 1);
        assert_eq!(note.links[0].line, 1);
    }
}
