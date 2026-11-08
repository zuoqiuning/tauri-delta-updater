//! 确定性归档的构建、列举与安全提取。

use crate::error::{DeltaError, Result};

mod format;
pub(crate) use self::format::hash_file;
use self::format::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// 整包条目的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BundleFileKind {
    /// 普通文件。
    #[default]
    File,
    /// 符号链接。`size` 是链接目标字符串的长度，`sha256` 是目标字符串的哈希。
    Symlink,
}

/// 整包里的一个文件条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BundleFile {
    /// 相对路径，统一用 `/` 分隔（例如 `resources/app.asar`）。
    pub path: String,
    /// 文件大小（字节）；符号链接时为目标字符串的长度。
    pub size: u64,
    /// 文件内容的 SHA-256（小写十六进制）；符号链接时为链接目标的哈希。
    pub sha256: String,
    /// Unix 权限位（`0o755` 等）。Windows 上没有意义，为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// 条目类型。
    #[serde(default, skip_serializing_if = "BundleFileKind::is_file")]
    pub kind: BundleFileKind,
}

impl BundleFileKind {
    fn is_file(&self) -> bool {
        matches!(self, BundleFileKind::File)
    }

    fn to_byte(self) -> u8 {
        match self {
            BundleFileKind::File => 0,
            BundleFileKind::Symlink => 1,
        }
    }

    fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(BundleFileKind::File),
            1 => Ok(BundleFileKind::Symlink),
            other => Err(DeltaError::BundleInvalid(format!(
                "未知的条目类型: {other}"
            ))),
        }
    }
}

/// 扫描目录，生成按路径排序的文件清单。
///
/// 普通文件记录大小与哈希；Unix 上同时记录权限位与符号链接
/// （macOS `.app` 的框架目录常含相对链接，丢失后签名与启动都会失效）。
/// 非 Unix 平台跳过符号链接，避免把需要额外权限才能重建的条目卷进归档。
pub fn describe_dir(root: impl AsRef<Path>) -> Result<Vec<BundleFile>> {
    let root = root.as_ref();
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<BundleFile>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(DeltaError::Io)? {
        let entry = entry.map_err(DeltaError::Io)?;
        let file_type = entry.file_type().map_err(DeltaError::Io)?;
        let path = entry.path();
        if file_type.is_symlink() {
            #[cfg(unix)]
            out.push(symlink_entry(root, &path)?);
            #[cfg(not(unix))]
            let _ = path;
            continue;
        }
        if file_type.is_dir() {
            walk(root, &path, out)?;
        } else if file_type.is_file() {
            out.push(file_entry(root, &path)?);
        }
    }
    Ok(())
}

/// 生成普通文件条目（含 Unix 权限位）。
fn file_entry(root: &Path, path: &Path) -> Result<BundleFile> {
    let relative = relative_path(root, path)?;
    let (size, sha256) = hash_file(path)?;
    Ok(BundleFile {
        path: relative,
        size,
        sha256,
        mode: file_mode(path),
        kind: BundleFileKind::File,
    })
}

/// 生成符号链接条目：大小与哈希都作用于链接目标字符串。
#[cfg(unix)]
fn symlink_entry(root: &Path, path: &Path) -> Result<BundleFile> {
    let relative = relative_path(root, path)?;
    let target = std::fs::read_link(path).map_err(DeltaError::Io)?;
    let target = target.to_str().ok_or_else(|| {
        DeltaError::BundleInvalid(format!("符号链接目标不是 UTF-8: {}", path.display()))
    })?;
    validate_link_target(target)?;
    let bytes = target.as_bytes();
    Ok(BundleFile {
        path: relative,
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        mode: None,
        kind: BundleFileKind::Symlink,
    })
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| DeltaError::BundleInvalid(format!("{} 不在根目录下", path.display())))?;
    to_slash(relative)
}

/// 读取 Unix 权限位（低 9 位）。非 Unix 平台返回 `None`。
#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// 校验符号链接目标：只允许包内相对路径，拒绝绝对路径与 `..`。
#[cfg(unix)]
fn validate_link_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.contains('\0')
        || target.starts_with('/')
        || target
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(DeltaError::BundleInvalid(format!(
            "符号链接目标非法（绝对路径或含上级目录）: {target}"
        )));
    }
    Ok(())
}

/// 校验安装目录里的文件与清单完全一致。
///
/// 任何缺失、尺寸变化或哈希不符都会返回错误——调用方应据此回退全量下载，
/// 而不是用不可信的字节去应用补丁（那只会得到哈希不符的产物）。
pub fn verify_layout(root: impl AsRef<Path>, files: &[BundleFile]) -> Result<()> {
    let root = root.as_ref();
    for file in files {
        validate_rel_path(&file.path)?;
        let path = root.join(Path::new(&file.path));

        if file.kind == BundleFileKind::Symlink {
            #[cfg(unix)]
            {
                let meta = std::fs::symlink_metadata(&path).map_err(|_| {
                    DeltaError::BundleInvalid(format!("缺少符号链接: {}", file.path))
                })?;
                if !meta.file_type().is_symlink() {
                    return Err(DeltaError::BundleInvalid(format!(
                        "应为符号链接但实际不是: {}",
                        file.path
                    )));
                }
                let target = std::fs::read_link(&path).map_err(DeltaError::Io)?;
                let target = target.to_str().ok_or_else(|| {
                    DeltaError::BundleInvalid(format!("符号链接目标不是 UTF-8: {}", file.path))
                })?;
                verify_content(&file.path, target.as_bytes(), file)?;
                continue;
            }
            #[cfg(not(unix))]
            return Err(DeltaError::BundleInvalid(format!(
                "当前平台不支持符号链接条目: {}",
                file.path
            )));
        }

        if !path.is_file() {
            return Err(DeltaError::BundleInvalid(format!(
                "缺少文件: {}",
                file.path
            )));
        }
        let (size, sha256) = hash_file(&path)?;
        if size != file.size || sha256 != file.sha256.to_lowercase() {
            return Err(DeltaError::HashMismatch {
                expected: format!("{}:{}", file.path, file.sha256),
                actual: format!("{}:{sha256}", file.path),
            });
        }
    }
    Ok(())
}

/// 校验符号链接目标的长度与哈希。
#[cfg(unix)]
fn verify_content(path: &str, bytes: &[u8], file: &BundleFile) -> Result<()> {
    let sha256 = sha256_hex(bytes);
    if bytes.len() as u64 != file.size || sha256 != file.sha256.to_lowercase() {
        return Err(DeltaError::HashMismatch {
            expected: format!("{}:{}", path, file.sha256),
            actual: format!("{path}:{sha256}"),
        });
    }
    Ok(())
}

/// 按确定性格式构建归档。
///
/// 条目按路径排序，内容逐块写入；构建时再次校验文件尺寸与哈希，
/// 避免把"清单写错"变成用户侧的更新失败。
pub fn build_archive(
    root: impl AsRef<Path>,
    files: &[BundleFile],
    out: impl AsRef<Path>,
) -> Result<()> {
    let root = root.as_ref();
    let mut sorted: Vec<&BundleFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    let mut writer = BufWriter::new(File::create(out.as_ref()).map_err(DeltaError::Io)?);
    writer.write_all(MAGIC).map_err(DeltaError::Io)?;
    writer
        .write_all(&(sorted.len() as u32).to_le_bytes())
        .map_err(DeltaError::Io)?;

    for (index, file) in sorted.iter().enumerate() {
        validate_rel_path(&file.path)?;
        if index > 0 && sorted[index - 1].path == file.path {
            return Err(DeltaError::BundleInvalid(format!(
                "重复的文件路径: {}",
                file.path
            )));
        }

        let source = root.join(Path::new(&file.path));

        write_path(&mut writer, &file.path)?;
        writer
            .write_all(&file.size.to_le_bytes())
            .map_err(DeltaError::Io)?;
        writer
            .write_all(&sha_bytes(&file.sha256)?)
            .map_err(DeltaError::Io)?;
        writer
            .write_all(&[file.kind.to_byte()])
            .map_err(DeltaError::Io)?;
        writer
            .write_all(&file.mode.unwrap_or(0).to_le_bytes())
            .map_err(DeltaError::Io)?;

        match file.kind {
            BundleFileKind::File => {
                let mut reader = BufReader::new(File::open(&source).map_err(DeltaError::Io)?);
                let mut hasher = Sha256::new();
                let mut remaining = file.size;
                let mut buf = vec![0u8; CHUNK];
                while remaining > 0 {
                    let want = usize::try_from(remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
                    let read = reader.read(&mut buf[..want]).map_err(DeltaError::Io)?;
                    if read == 0 {
                        return Err(DeltaError::BundleInvalid(format!(
                            "文件比清单声明的小: {}",
                            file.path
                        )));
                    }
                    hasher.update(&buf[..read]);
                    writer.write_all(&buf[..read]).map_err(DeltaError::Io)?;
                    remaining -= read as u64;
                }
                if reader.read(&mut buf[..1]).map_err(DeltaError::Io)? != 0 {
                    return Err(DeltaError::BundleInvalid(format!(
                        "文件比清单声明的大: {}",
                        file.path
                    )));
                }

                let actual = format!("{:x}", hasher.finalize());
                if actual != file.sha256.to_lowercase() {
                    return Err(DeltaError::HashMismatch {
                        expected: format!("{}:{}", file.path, file.sha256),
                        actual: format!("{}:{actual}", file.path),
                    });
                }
            }
            BundleFileKind::Symlink => {
                #[cfg(unix)]
                {
                    let target = std::fs::read_link(&source).map_err(DeltaError::Io)?;
                    let target = target.to_str().ok_or_else(|| {
                        DeltaError::BundleInvalid(format!("符号链接目标不是 UTF-8: {}", file.path))
                    })?;
                    validate_link_target(target)?;
                    verify_content(&file.path, target.as_bytes(), file)?;
                    writer
                        .write_all(target.as_bytes())
                        .map_err(DeltaError::Io)?;
                }
                #[cfg(not(unix))]
                return Err(DeltaError::BundleInvalid(format!(
                    "当前平台无法归档符号链接: {}",
                    file.path
                )));
            }
        }
    }

    writer.flush().map_err(DeltaError::Io)?;
    Ok(())
}

/// 读取归档的文件清单（只读头部，不校验内容哈希）。
pub fn list_archive(archive: impl AsRef<Path>) -> Result<Vec<BundleFile>> {
    let mut reader = BufReader::new(File::open(archive.as_ref()).map_err(DeltaError::Io)?);
    let count = read_header(&mut reader)?;

    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let path = read_path(&mut reader)?;
        let size = read_u64(&mut reader)?;
        let sha256 = read_sha(&mut reader)?;
        let kind = BundleFileKind::from_byte(read_u8(&mut reader)?)?;
        let mode = read_u32(&mut reader)?;
        files.push(BundleFile {
            path,
            size,
            sha256,
            mode: (mode != 0).then_some(mode),
            kind,
        });
        reader
            .seek(SeekFrom::Current(size as i64))
            .map_err(DeltaError::Io)?;
    }
    Ok(files)
}

/// 把归档提取到指定目录，并逐文件校验哈希与清单。
///
/// `expected` 来自已验签的清单；归档里出现额外条目、缺条目或哈希不符都会中止。
/// 提取目录必须是新建的空目录——本函数不做清理，避免误删宿主已有文件。
pub fn extract_archive(
    archive: impl AsRef<Path>,
    expected: &[BundleFile],
    out_dir: impl AsRef<Path>,
) -> Result<()> {
    let mut expected_by_path: Vec<&BundleFile> = expected.iter().collect();
    expected_by_path.sort_by(|a, b| a.path.cmp(&b.path));

    let out_dir = out_dir.as_ref();
    std::fs::create_dir_all(out_dir).map_err(DeltaError::Io)?;

    let mut reader = BufReader::new(File::open(archive.as_ref()).map_err(DeltaError::Io)?);
    let count = read_header(&mut reader)?;
    if count as usize != expected_by_path.len() {
        return Err(DeltaError::BundleInvalid(format!(
            "归档条目数 {count} 与清单 {} 不一致",
            expected_by_path.len()
        )));
    }

    let mut seen = 0usize;
    for _ in 0..count {
        let path = read_path(&mut reader)?;
        validate_rel_path(&path)?;
        let size = read_u64(&mut reader)?;
        let archive_sha = read_sha(&mut reader)?;
        let kind = BundleFileKind::from_byte(read_u8(&mut reader)?)?;
        let mode = read_u32(&mut reader)?;

        let Some(entry) = expected_by_path.get(seen) else {
            return Err(DeltaError::BundleInvalid("归档条目多于清单".into()));
        };
        if entry.path != path
            || entry.size != size
            || entry.sha256.to_lowercase() != archive_sha
            || entry.kind != kind
            || entry.mode.unwrap_or(0) != mode
        {
            return Err(DeltaError::BundleInvalid(format!(
                "归档条目与清单不一致: {path}"
            )));
        }

        let dest = out_dir.join(Path::new(&path));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(DeltaError::Io)?;
        }

        if kind == BundleFileKind::Symlink {
            #[cfg(unix)]
            {
                const MAX_LINK_TARGET: u64 = 4096;
                if size > MAX_LINK_TARGET {
                    return Err(DeltaError::BundleInvalid(format!(
                        "符号链接目标过长: {path}"
                    )));
                }
                let mut bytes = vec![0u8; size as usize];
                reader
                    .read_exact(&mut bytes)
                    .map_err(|_| DeltaError::BundleInvalid(format!("归档条目被截断: {path}")))?;
                let target = String::from_utf8(bytes).map_err(|_| {
                    DeltaError::BundleInvalid(format!("符号链接目标不是 UTF-8: {path}"))
                })?;
                validate_link_target(&target)?;
                if sha256_hex(target.as_bytes()) != entry.sha256.to_lowercase() {
                    return Err(DeltaError::HashMismatch {
                        expected: format!("{path}:{}", entry.sha256),
                        actual: format!("{path}:{}", sha256_hex(target.as_bytes())),
                    });
                }
                std::os::unix::fs::symlink(&target, &dest).map_err(|e| {
                    DeltaError::Io(std::io::Error::new(
                        e.kind(),
                        format!("创建符号链接 {path} 失败: {e}"),
                    ))
                })?;
            }
            #[cfg(not(unix))]
            return Err(DeltaError::BundleInvalid(format!(
                "当前平台不支持符号链接条目: {path}"
            )));
        } else {
            let mut file = BufWriter::new(File::create(&dest).map_err(DeltaError::Io)?);
            let mut hasher = Sha256::new();
            let mut remaining = size;
            let mut buf = vec![0u8; CHUNK];
            while remaining > 0 {
                let want = usize::try_from(remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
                reader
                    .read_exact(&mut buf[..want])
                    .map_err(|_| DeltaError::BundleInvalid(format!("归档条目被截断: {path}")))?;
                hasher.update(&buf[..want]);
                file.write_all(&buf[..want]).map_err(DeltaError::Io)?;
                remaining -= want as u64;
            }
            file.flush().map_err(DeltaError::Io)?;

            let actual = format!("{:x}", hasher.finalize());
            if actual != entry.sha256.to_lowercase() {
                return Err(DeltaError::HashMismatch {
                    expected: format!("{path}:{}", entry.sha256),
                    actual: format!("{path}:{actual}"),
                });
            }

            #[cfg(unix)]
            if mode != 0 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode))
                    .map_err(DeltaError::Io)?;
            }
        }
        seen += 1;
    }

    if seen != expected_by_path.len() {
        return Err(DeltaError::BundleInvalid("归档条目少于清单".into()));
    }
    Ok(())
}

/// 便捷构造：相对路径 → 清单条目（发布端扫描构树时使用）。
pub fn entry_for(root: &Path, relative: impl AsRef<Path>) -> Result<BundleFile> {
    let relative = relative.as_ref();
    validate_rel_path(&to_slash(relative)?)?;
    let path = root.join(relative);
    let meta = std::fs::symlink_metadata(&path).map_err(DeltaError::Io)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        return symlink_entry(root, &path);
        #[cfg(not(unix))]
        return Err(DeltaError::BundleInvalid(format!(
            "当前平台不支持符号链接条目: {}",
            relative.display()
        )));
    }
    file_entry(root, &path)
}
