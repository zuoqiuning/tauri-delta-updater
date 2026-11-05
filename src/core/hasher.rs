use crate::error::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncReadExt;

/// 计算文件的 SHA-256 十六进制哈希。
pub async fn calculate_hash<P: AsRef<Path>>(path: P) -> Result<String> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 同步计算文件的 SHA-256 十六进制哈希。
///
/// 供替换脚本生成前的最后校验与同步上下文使用。
pub fn hash_file_sync<P: AsRef<Path>>(path: P) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 计算字节序列的 SHA-256 十六进制哈希。
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
