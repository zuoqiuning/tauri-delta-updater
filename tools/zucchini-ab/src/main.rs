//! Zucchini A/B：同一对旧/新产物上比较 bsdiff+zstd 与 Zucchini。
//!
//! 用法：
//! `cargo run --release --manifest-path tools/zucchini-ab/Cargo.toml -- old.exe new.exe`
//!
//! 该工具单独依赖 `zucchini`，不进入主 crate 的 Cargo.toml；构建它需要 clang
//!（`zucchini-sys` 从 Chromium 源码构建）。这是有意的：引擎本身保持零额外系统依赖。

use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let old = PathBuf::from(args.next().ok_or("用法: zucchini-ab <old> <new>")?);
    let new = PathBuf::from(args.next().ok_or("用法: zucchini-ab <old> <new>")?);

    let old_bytes = std::fs::read(&old)?;
    let new_bytes = std::fs::read(&new)?;

    // 主引擎当前的默认后端（qbsdiff + zstd）。
    let patch_path = std::env::temp_dir().join(format!("zucchini-ab-{}.patch", std::process::id()));
    tauri_delta_updater::create_patch(&old, &new, &patch_path).await?;
    let bsdiff_patch = std::fs::read(&patch_path)?;
    let _ = std::fs::remove_file(&patch_path);

    // Zucchini 输出不压缩；同时报告原始体积与叠加 zstd 后的体积。
    let zucchini_patch = zucchini::generate(&old_bytes, &new_bytes)?;
    let zucchini_zstd = zstd::bulk::compress(&zucchini_patch, 19)?;
    let restored = zucchini::apply(&old_bytes, &zucchini_patch)?;
    assert_eq!(restored, new_bytes, "Zucchini apply 结果必须逐字节一致");

    let full = new_bytes.len() as f64;
    println!("new file:          {:>10} bytes", new_bytes.len());
    println!(
        "bsdiff + zstd:     {:>10} bytes ({:.2}%)",
        bsdiff_patch.len(),
        bsdiff_patch.len() as f64 / full * 100.0
    );
    println!(
        "zucchini raw:      {:>10} bytes ({:.2}%)",
        zucchini_patch.len(),
        zucchini_patch.len() as f64 / full * 100.0
    );
    println!(
        "zucchini + zstd:   {:>10} bytes ({:.2}%)",
        zucchini_zstd.len(),
        zucchini_zstd.len() as f64 / full * 100.0
    );
    Ok(())
}
