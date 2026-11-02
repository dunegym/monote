//! 构建脚本。
//!
//! 除 Tauri 标准构建外，为 **测试目标**（集成/性能测试）注入 Windows
//! Common-Controls 6.0 manifest（依赖 comctl32!TaskDialogIndirect）。
//!
//! 背景：`rfd` 原生对话框最终使用 comctl32 v6 的 TaskDialogIndirect；应用主程序
//! `monote.exe` 由 tauri-build 自动嵌入 Common-Controls 6.0 manifest，而 cargo 生成的
//! 测试可执行文件默认没有资源区，会加载 comctl32 v5 并在启动期报
//! `0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND`。此处用 Cargo 的
//! `cargo:rustc-link-arg-tests` 为测试目标嵌入 manifest（不影响主程序/bin 目标）。

fn main() {
    #[cfg(windows)]
    embed_common_controls_manifest_for_tests();
    tauri_build::build()
}

/// 为测试目标注入 Common-Controls 6.0 manifest（仅 Windows）。
#[cfg(windows)]
fn embed_common_controls_manifest_for_tests() {
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => return,
    };
    let manifest_path = out_dir.join("common-controls6.manifest");
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
    if std::fs::write(&manifest_path, manifest).is_err() {
        return;
    }
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
        manifest_path.display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
