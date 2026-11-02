//! 性能冒烟测试（对应设计文档 13.3/13.4 与 AC-026 的小规模预演）。
//!
//! 在临时库生成 N 篇笔记（默认 1,000 篇，含中文正文/标签/双链），测量：
//! - 全量索引建立耗时
//! - 标题检索 P95
//! - 全文检索 P95
//!
//! 说明：正式验收要求 10,000 篇 / 2GB 基准数据集（AC-027），本测试仅作开发期冒烟，
//! 阈值取宽松上界以避免 CI 抖动；实测数据写入 `docs/测试报告.md`。

use std::time::{Duration, Instant};

use monote_lib::state::AppState;

const N: usize = 1000;
const P95_TITLE_MS: u128 = 200;
const P95_FULL_MS: u128 = 800;

fn p95(samples: &[Duration]) -> Duration {
    let mut v: Vec<Duration> = samples.to_vec();
    v.sort();
    let idx = ((v.len() as f64) * 0.95).ceil() as usize;
    v[idx.saturating_sub(1).min(v.len() - 1)]
}

#[test]
fn perf_smoke_1000_notes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vault_dir = tmp.path().join("vault");
    let mut state = AppState::new_with_config(tmp.path().join("config.json"));
    state.open_vault(&vault_dir).expect("open vault");

    // 1) 生成 N 篇笔记
    let t0 = Instant::now();
    for i in 0..N {
        let tag = if i % 3 == 0 { "考研" } else { "随笔" };
        let link = if i > 0 { format!("\n\n参见 [[笔记{}]]。", i - 1) } else { String::new() };
        let content = format!(
            "---\ntitle: 笔记{i}\ntags: [{tag}]\n---\n\n# 笔记{i}\n\n这是第 {i} 篇性能测试笔记，主题包含虚拟内存、文件系统与操作系统调度。{link}\n\n#性能 #测试\n"
        );
        state
            .write_note(&format!("笔记{i}.md"), &content, Some(0))
            .expect("write note");
    }
    let write_elapsed = t0.elapsed();

    // 2) 全量重建索引并计时
    let t1 = Instant::now();
    state.rebuild_index().expect("rebuild");
    for _ in 0..600 {
        if !state.index_status().building {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!state.index_status().building, "index rebuild did not finish in 30s");
    let index_elapsed = t1.elapsed();

    // 3) 标题检索 50 次
    let mut title_samples = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        let r = state
            .search("title", "笔记99", Default::default(), "relevance", 0, 20)
            .expect("title search");
        title_samples.push(t.elapsed());
        assert!(!r.items.is_empty());
    }

    // 4) 全文检索 50 次
    let mut full_samples = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        let r = state
            .search("full", "虚拟内存", Default::default(), "relevance", 0, 20)
            .expect("full search");
        full_samples.push(t.elapsed());
        assert!(!r.items.is_empty());
    }

    let title_p95 = p95(&title_samples);
    let full_p95 = p95(&full_samples);
    println!(
        "[perf] N={N} write={write_elapsed:?} index={index_elapsed:?} title_p95={title_p95:?} full_p95={full_p95:?}"
    );

    assert!(
        title_p95.as_millis() <= P95_TITLE_MS,
        "标题检索 P95 超阈值: {title_p95:?} > {P95_TITLE_MS}ms"
    );
    assert!(
        full_p95.as_millis() <= P95_FULL_MS,
        "全文检索 P95 超阈值: {full_p95:?} > {P95_FULL_MS}ms"
    );
}
