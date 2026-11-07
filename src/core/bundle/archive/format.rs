//! 归档格式的编解码原语：路径校验、流式哈希与头字段读写。

use crate::error::{DeltaError, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

/// 归档魔数。版本号写进最后一个字节，格式变更时旧客户端会直接拒绝。
///
/// `TDXBNDL2` 起每条记录额外携带类型与 Unix 权限位：整包重建 macOS
/// `.app` 时必须恢复符号链接与可执行位，否则签名与启动都会失效。
pub(super) const MAGIC: &[u8; 8] = b"TDXBNDL2";

/// 分块读取大小（构建与提取都不把整包读进内存）。
pub(super) const CHUNK: usize = 64 * 1024;

/// 把相对路径转成 `/` 分隔，并拒绝包含反斜杠的输入。
pub(super) fn to_slash(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => {
                return Err(DeltaError::BundleInvalid(format!(
                    "非法相对路径: {}",
                    path.display()
                )))
            }
        }
    }
    Ok(parts.join("/"))
}

/// 校验归档内路径：只允许 `a/b/c` 形式的普通相对路径。
pub(super) fn validate_rel_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || path.contains('\0')
        || path.starts_with('/')
    {
        return Err(DeltaError::BundleInvalid(format!(
            "非法路径（绝对路径/盘符/反斜杠）: {path}"
        )));
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(DeltaError::BundleInvalid(format!(
            "非法路径（空段或上级目录）: {path}"
        )));
    }
    Ok(())
}

/// 计算文件的大小与 SHA-256（流式，不整体读入内存）。
pub(crate) fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut reader = BufReader::new(File::open(path).map_err(DeltaError::Io)?);
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = reader.read(&mut buf).map_err(DeltaError::Io)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buf[..read]);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

/// 写文件路径（长度 + UTF-8 字节）。
pub(super) fn write_path<W: Write>(writer: &mut W, path: &str) -> Result<()> {
    let bytes = path.as_bytes();
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .map_err(DeltaError::Io)?;
    writer.write_all(bytes).map_err(DeltaError::Io)?;
    Ok(())
}

/// 读文件路径并校验格式。
pub(super) fn read_path<R: Read>(reader: &mut R) -> Result<String> {
    let len = read_u32(reader)? as usize;
    if len == 0 || len > 4096 {
        return Err(DeltaError::BundleInvalid(format!("路径长度非法: {len}")));
    }
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| DeltaError::BundleInvalid("归档头被截断".into()))?;
    String::from_utf8(bytes).map_err(|_| DeltaError::BundleInvalid("路径不是 UTF-8".into()))
}

/// 读并校验魔数，返回条目数。
pub(super) fn read_header<R: Read>(reader: &mut R) -> Result<u32> {
    let mut magic = [0u8; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|_| DeltaError::BundleInvalid("不是整包归档（头被截断）".into()))?;
    if &magic != MAGIC {
        return Err(DeltaError::BundleInvalid("不是整包归档（魔数不符）".into()));
    }
    let count = read_u32(reader)?;
    if count > 100_000 {
        return Err(DeltaError::BundleInvalid(format!("条目数异常: {count}")));
    }
    Ok(count)
}

pub(super) fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| DeltaError::BundleInvalid("归档头被截断".into()))?;
    Ok(u32::from_le_bytes(bytes))
}

pub(super) fn read_u64<R: Read>(reader: &mut R) -> Result<u64> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| DeltaError::BundleInvalid("归档头被截断".into()))?;
    Ok(u64::from_le_bytes(bytes))
}

pub(super) fn read_u8<R: Read>(reader: &mut R) -> Result<u8> {
    let mut bytes = [0u8; 1];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| DeltaError::BundleInvalid("归档头被截断".into()))?;
    Ok(bytes[0])
}

/// 读 32 字节原始 SHA-256，返回小写十六进制。
pub(super) fn read_sha<R: Read>(reader: &mut R) -> Result<String> {
    let mut bytes = [0u8; 32];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| DeltaError::BundleInvalid("归档头被截断".into()))?;
    Ok(bytes_to_hex(&bytes))
}

/// 把十六进制 SHA-256 转成 32 字节。
pub(super) fn sha_bytes(hex: &str) -> Result<[u8; 32]> {
    let text = hex.trim();
    if text.len() != 64 {
        return Err(DeltaError::BundleInvalid(format!(
            "SHA-256 长度非法: {hex}"
        )));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
        let value = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or(""), 16)
            .map_err(|_| DeltaError::BundleInvalid(format!("SHA-256 不是十六进制: {hex}")))?;
        out[index] = value;
    }
    Ok(out)
}

pub(super) fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 计算任意字节的 SHA-256（小写十六进制）。
#[cfg(unix)]
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
