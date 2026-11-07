//! 渐进发布（灰度）分桶：稳定安装 ID 与比例命中判断。
//!
//! 服务端只在清单里声明比例（0–100），客户端用
//! `sha256(install_id + version)` 得到稳定桶位。同一安装在同一版本上的
//! 结果不会抖动；换版本重新分桶，避免同一批用户永远第一个或最后一个收到更新。

use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 安装 ID 文件名（放在工作目录）。
pub const INSTALL_ID_FILE: &str = "tdx_install_id";

/// 计算稳定的桶位（0–99）。
///
/// 只依赖安装 ID 与版本号，不读取环境、不联网，便于在宿主侧复现与测试。
pub fn bucket(install_id: &str, version: &str) -> u8 {
    let mut hasher = Sha256::new();
    hasher.update(install_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(version.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) % 100) as u8
}

/// 当前安装是否落在给定放量比例内。
///
/// `percentage == 0` 表示暂停发布，`>= 100` 表示全量放行。
pub fn selected(install_id: &str, version: &str, percentage: u8) -> bool {
    if percentage == 0 {
        false
    } else if percentage >= 100 {
        true
    } else {
        bucket(install_id, version) < percentage
    }
}

/// 读取工作目录中的安装 ID；不存在或内容非法时生成并持久化一个新 ID。
///
/// ID 只用于灰度分桶，不携带用户信息，也不参与任何安全校验。
/// 安装 ID 由工作目录、可执行文件路径、机器/用户环境与当前时间派生，
/// 因此不同安装之间不会共用同一个分桶结果。
pub fn load_or_create_install_id(work_dir: &Path) -> std::io::Result<String> {
    let path = work_dir.join(INSTALL_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if is_valid_install_id(id) {
            return Ok(id.to_string());
        }
    }

    let id = generate_install_id(work_dir);
    std::fs::write(&path, &id)?;
    Ok(id)
}

/// 安装 ID 是 8–64 位十六进制字符串；损坏时重新生成而不是报错。
fn is_valid_install_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn generate_install_id(work_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(work_dir.as_os_str().as_encoded_bytes());
    if let Ok(exe) = std::env::current_exe() {
        hasher.update(exe.as_os_str().as_encoded_bytes());
    }
    for key in ["COMPUTERNAME", "HOSTNAME", "USERNAME", "USER"] {
        if let Some(value) = std::env::var_os(key) {
            hasher.update(value.as_encoded_bytes());
        }
    }
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        hasher.update(now.as_nanos().to_le_bytes());
    }
    hasher.update(std::process::id().to_le_bytes());

    let digest = hasher.finalize();
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{bucket, load_or_create_install_id, selected};

    /// 桶位必须是确定性的且落在 0–99。
    #[test]
    fn bucket_is_stable_and_in_range() {
        let id = "0123456789abcdef";
        let first = bucket(id, "1.2.3");
        assert_eq!(first, bucket(id, "1.2.3"));
        assert!(first < 100, "桶位必须落在 0–99，实际 {first}");

        // 版本参与哈希：换版本重新分桶，避免同一批用户永远排在最后。
        let versions: Vec<u8> = ["1.2.3", "1.2.4", "1.3.0"]
            .iter()
            .map(|version| bucket(id, version))
            .collect();
        assert!(
            versions.iter().any(|value| *value != first),
            "不同版本应至少产生一个不同桶位"
        );
    }

    /// 0 暂停、100 全量；中间比例必须与桶位严格一致（`bucket < percentage`）。
    #[test]
    fn selected_respects_percentage_boundaries() {
        let id = "fedcba9876543210";
        let version = "2.0.0";
        let bucket = bucket(id, version);

        assert!(!selected(id, version, 0), "0% 必须一个都不放行");
        assert!(selected(id, version, 100), "100% 必须全部放行");
        assert!(!selected(id, version, bucket), "桶位等于阈值时不应命中");
        assert!(
            selected(id, version, bucket.saturating_add(1)),
            "阈值高于桶位时应命中"
        );
    }

    /// 1000 个安装、20% 放量：命中数量应接近 200，且同一安装多次检查结果稳定。
    #[test]
    fn percentage_distribution_is_stable_across_many_installs() {
        let mut hits = 0;
        for i in 0..1000 {
            let id = format!("install-{i:04}");
            let first = selected(&id, "1.2.3", 20);
            assert_eq!(
                first,
                selected(&id, "1.2.3", 20),
                "同一安装同一版本的分桶结果必须稳定"
            );
            if first {
                hits += 1;
            }
        }
        assert!(
            (150..=250).contains(&hits),
            "20% 放量在 1000 个安装中应接近 200，实际 {hits}"
        );
    }

    /// 安装 ID 首次创建后必须复用，而不是每次检查都重新分桶。
    #[test]
    fn install_id_is_created_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_install_id(dir.path()).unwrap();
        let second = load_or_create_install_id(dir.path()).unwrap();

        assert_eq!(first, second);
        assert!(super::is_valid_install_id(&first));
        assert!(dir.path().join(super::INSTALL_ID_FILE).is_file());

        // 损坏内容不能沿用，否则会一直得到同一个空字符串。
        std::fs::write(dir.path().join(super::INSTALL_ID_FILE), "not hex").unwrap();
        let repaired = load_or_create_install_id(dir.path()).unwrap();
        assert!(super::is_valid_install_id(&repaired));
    }
}
