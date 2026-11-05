//! 5 分钟本地验证：用两个可执行文件确认增量真的生效。
//!
//! 用法：
//!
//! ```bash
//! cargo run --example simulate_update -- old.exe new.exe target/delta-sim
//! ```
//!
//! 不需要网络、不需要签名密钥，也不需要完整的 Tauri 应用。
//! 它跑的是与真实客户端相同的路径：生成 → zstd 压缩 → 应用 → 双端哈希校验。

use tauri_delta_updater::simulate_update;

#[tokio::main]
async fn main() -> tauri_delta_updater::Result<()> {
    let mut args = std::env::args().skip(1);
    let old = args.next().unwrap_or_else(|| "old.exe".to_string());
    let new = args.next().unwrap_or_else(|| "new.exe".to_string());
    let work = args
        .next()
        .unwrap_or_else(|| "target/delta-sim".to_string());

    let report = simulate_update(&old, &new, &work).await?;

    println!("补丁体积: {} 字节", report.patch_size);
    println!("全量体积: {} 字节", report.target_size);
    println!("补丁占比: {:.2}%", report.ratio() * 100.0);
    println!("补丁 SHA-256: {}", report.patch_hash);
    println!("产物 SHA-256: {}", report.target_hash);
    println!(
        "结论: {}",
        if report.ratio() < 0.5 {
            "增量收益明显，适合接入。"
        } else {
            "补丁接近全量，这两个构建差异过大，建议走全量下载。"
        }
    );
    Ok(())
}
