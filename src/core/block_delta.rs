//! 大文件分块差分（TDXB1）。
//!
//! bsdiff 的内存模型是 `max(17n, 9n+m)`：单文件超过约 240 MB 就无法在
//! 4 GiB 预算内完成，发布端只能跳过该基线、让用户下载全量安装包。
//!
//! 分块差分用**内容定义分块**（content-defined chunking, CDC）+ 哈希索引
//! 绕开这个限制：
//!
//! - 旧文件按内容边界切块（平均 8 KiB），建立 `(长度, 哈希) → 偏移` 索引；
//! - 新文件按同一规则切块，命中即写 `COPY`；
//! - 缺口先尝试在对应旧内容上做小块 bsdiff（`PATCH`），否则写 `LITERAL`；
//! - 所有 `PATCH` / `LITERAL` 字节汇入一条 zstd 流，跨块共享压缩字典。
//!
//! 因为分块边界只由局部内容决定，插入/删除造成的整体位移不会让后续块失配
//! ——这正是 rsync 类算法的性质。未命中的缺口会在两侧可定位的旧内容上再做
//! 一次有界（≤4 MiB）的 bsdiff，把散点修改压到接近常规补丁的体积。
//! 内存开销与文件总大小基本无关（索引约 `size/8K × 24 B`，2 GB 文件约
//! 18 MB；单次缺口补丁约 68 MB），2 GB 以上文件也能生成补丁。
//!
//! # 兼容性
//!
//! TDXB1 是与 `BSDIFF40` / zstd 并列的第三种容器格式，只有 0.5.0 起的
//! 客户端能应用。发布端必须显式开启 `--large-file-delta`，且只能在确认
//! 所有客户端都已升级后使用；否则老客户端会下载到无法应用的补丁。
//!
//! # 容器布局
//!
//! ```text
//! magic "TDXB1" (5)
//! version u8 (1)
//! ops_len u32
//! old_len u64
//! new_len u64
//! ops[ops_len]
//! zstd(literal stream)
//! ```
//!
//! 指令：`0 = COPY { old_offset u64, len u32 }`、
//! `1 = LITERAL { len u32 }`（字节按顺序取自 zstd 流）、
//! `2 = PATCH { old_offset u64, old_len u32, out_len u32, patch_len u32 }`
//! （从 zstd 流取 `patch_len` 字节的 `BSDIFF40`，在旧文件该区间上还原，
//! 产出 `out_len` 字节）。

use super::diff::{map_file, mapped, run_blocking};
use crate::core::disk::check_disk_space;
use crate::{DeltaError, Result};
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// 容器魔数。
const MAGIC: &[u8; 5] = b"TDXB1";
/// 当前格式版本。
const FORMAT_VERSION: u8 = 1;
/// 固定头长度：magic + version + ops_len + old_len + new_len。
///
/// 头里**不允许**出现任何"仅作信息"的字段：容器每个字节都必须参与校验，
/// 否则"翻转任意字节必定失败"就不再成立。
const HEADER_LEN: usize = 5 + 1 + 4 + 8 + 8;
/// 分块下限：避免把连续小改动切得过碎。
const MIN_CHUNK: usize = 2 * 1024;
/// 分块上限：控制单次 COPY/LITERAL 的内存与指令粒度。
const MAX_CHUNK: usize = 64 * 1024;
/// 边界掩码：`1 << 13` 对应平均 8 KiB 一块。
const BOUNDARY_MASK: u64 = (1 << 13) - 1;
/// 指令区的合理上限（约 500 万条指令），防止恶意补丁撑爆内存。
const MAX_OPS_BYTES: u64 = 64 * 1024 * 1024;
/// 分块差分允许的最大文件大小（bsdiff 的 2 GB 限制不适用）。
const MAX_BLOCK_DELTA_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// 同一哈希最多记录的偏移数：全零文件会让所有块落到同一个键上。
const MAX_OFFSETS_PER_KEY: usize = 1024;
/// 缺口再做一次小块 bsdiff 的规模上限。
///
/// 缺口越大，bsdiff 的内存（约 17×）与耗时越高；4 MiB 对应约 68 MiB，
/// 在"低内存"路径里仍然可接受，超出则退回字面量。
const MAX_GAP_PATCH: usize = 4 * 1024 * 1024;
/// 缺口补丁字节的上限：生成端只接受小于缺口本身的补丁，再加固定余量。
const MAX_GAP_PATCH_BYTES: u32 = (MAX_GAP_PATCH + 64 * 1024) as u32;
/// 解压后产物的固定余量，与 zstd 补丁的 `decompress_to_temp` 保持一致。
const EXPAND_HEADROOM_BYTES: u64 = 128 * 1024 * 1024;

/// 第一遍扫描得到的区间：可整体复制的旧内容，或需要重建的缺口。
enum Segment {
    Copy { len: usize, old_off: u64 },
    Miss { new_start: usize, len: usize },
}

/// 一条重建指令。
enum Op {
    Copy {
        offset: u64,
        len: u32,
    },
    Literal {
        len: u32,
    },
    Patch {
        old_offset: u64,
        old_len: u32,
        patch_len: u32,
    },
}

/// 按 qbsdiff 的方式为分块任务估算工作集（索引 + 映射页）。
fn estimate_working_set(old_len: u64) -> u64 {
    old_len / 32 + 16 * 1024 * 1024
}

/// 判断补丁文件是否为 TDXB1 容器（只读前 5 字节）。
pub(crate) async fn is_block_delta_file(path: &Path) -> Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut head = [0u8; 5];
    let mut read = 0;
    while read < 5 {
        match file.read(&mut head[read..]).await? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read == 5 && &head == MAGIC)
}

/// 生成 TDXB1 分块补丁。
///
/// 与 [`crate::create_patch`] 不同，这里没有 `17n` 的工作集要求：
/// 内存只与分块索引相关，因此超大单文件也能生成增量补丁。
///
/// # 什么时候该用它
///
/// 发布流水线应**先尝试 bsdiff**，只有它因工作集超限被拒绝时才回退到这里
/// （`cargo delta generate --large-file-delta` 已内置该决策）。它不是
/// bsdiff 的替代品：大段连续内容移动、少量分散修改的场景下，bsdiff 的
/// 补丁通常小一个数量级；TDXB1 的收益是"bsdiff 根本跑不了"的大文件。
///
/// # 内存上界
///
/// - 分块索引：约 `old_size / 8 KiB × 40 B`（2 GB 文件约 10 MB）；
/// - 单次缺口补丁：bsdiff 输入 ≤ 4 MiB，约 68 MiB；
/// - 字面量流：zstd 编码器缓冲，与输入块大小同阶。
///
/// 与文件总大小基本无关。
///
/// # 完整性
///
/// 容器本身**不是**签名产物：它的完整性由外层清单里的 `patch_hash` 与
/// 产物 `target_hash` 保证，与 `BSDIFF40` 的威胁模型一致。解析器仍会对
/// 每条指令做越界与总量校验，见 `docs/BLOCK_DELTA_THREAT_MODEL.md`。
pub async fn create_patch_block_delta<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
    old_file: P1,
    new_file: P2,
    patch_output: P3,
    zstd_level: i32,
) -> Result<()> {
    if !(1..=22).contains(&zstd_level) {
        return Err(DeltaError::Config(format!(
            "zstd 压缩级别必须在 1–22 之间，实际 {zstd_level}"
        )));
    }
    let old_path = old_file.as_ref().to_path_buf();
    let new_path = new_file.as_ref().to_path_buf();
    let patch_path = patch_output.as_ref().to_path_buf();

    let old_len = std::fs::metadata(&old_path)
        .map(|m| m.len())
        .map_err(DeltaError::Io)?;
    let new_len = std::fs::metadata(&new_path)
        .map(|m| m.len())
        .map_err(DeltaError::Io)?;
    for size in [old_len, new_len] {
        if size > MAX_BLOCK_DELTA_BYTES {
            return Err(DeltaError::IncrementalNotApplicable { size });
        }
    }

    run_blocking(
        "生成分块增量补丁",
        estimate_working_set(old_len),
        move || {
            generate_sync(
                &old_path,
                &new_path,
                &patch_path,
                old_len,
                new_len,
                zstd_level,
            )
        },
    )
    .await
}

fn generate_sync(
    old_path: &Path,
    new_path: &Path,
    patch_path: &Path,
    old_len: u64,
    new_len: u64,
    zstd_level: i32,
) -> Result<()> {
    let old = map_file(old_path).map_err(DeltaError::Io)?;
    let new = map_file(new_path).map_err(DeltaError::Io)?;
    let old_bytes = mapped(&old);
    let new_bytes = mapped(&new);
    debug_assert_eq!(old_bytes.len() as u64, old_len);
    debug_assert_eq!(new_bytes.len() as u64, new_len);

    // 旧文件分块索引：(长度, 哈希) -> 偏移列表
    let mut index: HashMap<(u32, u64), Vec<u64>> = HashMap::new();
    for (start, end) in cdc_chunks(old_bytes) {
        let offsets = index.entry(chunk_key(&old_bytes[start..end])).or_default();
        if offsets.len() < MAX_OFFSETS_PER_KEY {
            offsets.push(start as u64);
        }
    }

    let literal_path = with_suffix(patch_path, ".lit");
    let mut ops: Vec<u8> = Vec::new();
    let written = (|| -> Result<()> {
        let literal_file = std::fs::File::create(&literal_path)
            .map_err(|e| DeltaError::PatchGenFailed(format!("创建分块差分临时文件失败: {e}")))?;
        let mut encoder = zstd::stream::Encoder::new(BufWriter::new(literal_file), zstd_level)
            .map_err(|e| DeltaError::PatchGenFailed(format!("初始化 zstd 编码器失败: {e}")))?;

        // 第一遍：按内容分块并合并相邻同类区间。
        // 未命中的区间不立即写字面量——先看它两侧的 COPY 能否定位出一段
        // 对应的旧内容，能定位就再做一次小块 bsdiff，把"整块字面量"压成
        // 接近 bsdiff 质量的补丁。这一步是超大文件仍有小补丁的关键。
        let mut segments: Vec<Segment> = Vec::new();
        for (start, end) in cdc_chunks(new_bytes) {
            let chunk = &new_bytes[start..end];
            let matched = index.get(&chunk_key(chunk)).and_then(|offsets| {
                offsets.iter().copied().find(|offset| {
                    let offset = *offset as usize;
                    offset + chunk.len() <= old_bytes.len()
                        && &old_bytes[offset..offset + chunk.len()] == chunk
                })
            });
            let len = end - start;
            match matched {
                Some(offset) => {
                    if let Some(Segment::Copy {
                        len: prev_len,
                        old_off: prev_off,
                        ..
                    }) = segments.last_mut()
                    {
                        if *prev_off + *prev_len as u64 == offset {
                            *prev_len += len;
                            continue;
                        }
                    }
                    segments.push(Segment::Copy {
                        len,
                        old_off: offset,
                    });
                }
                None => {
                    if let Some(Segment::Miss {
                        new_start: prev_start,
                        len: prev_len,
                    }) = segments.last_mut()
                    {
                        if *prev_start + *prev_len == start {
                            *prev_len += len;
                            continue;
                        }
                    }
                    segments.push(Segment::Miss {
                        new_start: start,
                        len,
                    });
                }
            }
        }

        // 第二遍：把区间写成指令，缺口尝试小块 bsdiff。
        let mut old_cursor: u64 = 0;
        for index in 0..segments.len() {
            match &segments[index] {
                Segment::Copy { len, old_off } => {
                    push_copy(&mut ops, *old_off, *len as u32);
                    old_cursor = *old_off + *len as u64;
                }
                Segment::Miss { new_start, len } => {
                    let next_old = segments[index + 1..]
                        .iter()
                        .find_map(|segment| match segment {
                            Segment::Copy { old_off, .. } => Some(*old_off),
                            Segment::Miss { .. } => None,
                        })
                        .unwrap_or(old_len);
                    let new_gap = &new_bytes[*new_start..*new_start + *len];
                    let old_gap: &[u8] = if next_old > old_cursor {
                        &old_bytes[old_cursor as usize..next_old as usize]
                    } else {
                        &[]
                    };

                    let mut patched = false;
                    if !old_gap.is_empty()
                        && old_gap.len() <= MAX_GAP_PATCH
                        && new_gap.len() <= MAX_GAP_PATCH
                    {
                        let mut patch = Vec::new();
                        if qbsdiff::Bsdiff::new(old_gap, new_gap)
                            .compare(&mut patch)
                            .is_ok()
                            && patch.len() + 24 < new_gap.len()
                        {
                            encoder.write_all(&patch).map_err(|e| {
                                DeltaError::PatchGenFailed(format!("写入缺口补丁失败: {e}"))
                            })?;
                            push_patch(
                                &mut ops,
                                old_cursor,
                                old_gap.len() as u32,
                                new_gap.len() as u32,
                                patch.len() as u32,
                            );
                            patched = true;
                        }
                    }
                    if !patched {
                        encoder.write_all(new_gap).map_err(|e| {
                            DeltaError::PatchGenFailed(format!("写入分块差分字面量失败: {e}"))
                        })?;
                        push_literal(&mut ops, new_gap.len() as u32);
                    }
                }
            }
            if ops.len() as u64 > MAX_OPS_BYTES {
                return Err(DeltaError::PatchGenFailed(
                    "分块差分指令过多，文件内容差异过大".into(),
                ));
            }
        }

        let mut literal_file = encoder
            .finish()
            .map_err(|e| DeltaError::PatchGenFailed(format!("结束 zstd 编码失败: {e}")))?;
        literal_file
            .flush()
            .map_err(|e| DeltaError::PatchGenFailed(format!("刷新字面量流失败: {e}")))?;
        // 显式关闭，避免后面重新打开时在 Windows 上遇到共享冲突。
        drop(literal_file);
        Ok(())
    })();

    if let Err(err) = written {
        let _ = std::fs::remove_file(&literal_path);
        return Err(err);
    }

    let assembled = (|| -> std::io::Result<()> {
        let mut out = BufWriter::new(std::fs::File::create(patch_path)?);
        out.write_all(MAGIC)?;
        out.write_all(&[FORMAT_VERSION])?;
        out.write_all(&(ops.len() as u32).to_le_bytes())?;
        out.write_all(&old_len.to_le_bytes())?;
        out.write_all(&new_len.to_le_bytes())?;
        out.write_all(&ops)?;
        let mut literals = std::fs::File::open(&literal_path)?;
        std::io::copy(&mut literals, &mut out)?;
        out.flush()?;
        out.get_ref().sync_all()
    })();

    let _ = std::fs::remove_file(&literal_path);

    assembled.map_err(|e| {
        let _ = std::fs::remove_file(patch_path);
        DeltaError::PatchGenFailed(format!("写入分块补丁失败: {e}"))
    })
}

/// 应用 TDXB1 分块补丁。
///
/// 必须在 [`run_blocking`] 的独立线程中调用；产物哈希由外层
/// [`crate::apply_patch`] 统一校验。
///
/// # 校验顺序
///
/// 1. 头部字段与文件长度自洽（版本、指令区、声明大小）；
/// 2. 产物大小不超过 `2 × 旧文件 + 128 MiB`（与 zstd 补丁同口径）；
/// 3. 旧文件实际大小必须与头部一致；
/// 4. 每条 `COPY` / `PATCH` 的源区间必须落在旧文件内，`LITERAL` 总量
///    必须精确等于声明的产物大小。
///
/// 任何一项不满足都返回错误，不会 panic，也不会写出越界内容。
pub(crate) fn apply_block_delta(old_path: &Path, patch_path: &Path, out_path: &Path) -> Result<()> {
    let mut reader = BufReader::new(
        std::fs::File::open(patch_path)
            .map_err(|e| DeltaError::PatchApplyFailed(format!("打开分块补丁失败: {e}")))?,
    );

    let mut header = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut header)
        .map_err(|e| DeltaError::PatchApplyFailed(format!("分块补丁头不完整: {e}")))?;
    if &header[..5] != MAGIC {
        return Err(DeltaError::PatchApplyFailed("不是分块补丁".into()));
    }
    if header[5] != FORMAT_VERSION {
        return Err(DeltaError::PatchApplyFailed(format!(
            "不支持的分块补丁版本 {}",
            header[5]
        )));
    }
    let ops_len = u64::from(u32::from_le_bytes(
        header[6..10].try_into().expect("固定头长度已校验"),
    ));
    let old_len = u64::from_le_bytes(header[10..18].try_into().expect("固定头长度已校验"));
    let new_len = u64::from_le_bytes(header[18..26].try_into().expect("固定头长度已校验"));

    if ops_len > MAX_OPS_BYTES {
        return Err(DeltaError::PatchApplyFailed(format!(
            "分块补丁指令区过大（{ops_len} 字节）"
        )));
    }
    let patch_file_len = std::fs::metadata(patch_path).map_err(DeltaError::Io)?.len();
    if ops_len.saturating_add(HEADER_LEN as u64) > patch_file_len {
        return Err(DeltaError::PatchApplyFailed(
            "分块补丁指令区超出文件长度".into(),
        ));
    }
    if new_len > MAX_BLOCK_DELTA_BYTES {
        return Err(DeltaError::PatchApplyFailed(format!(
            "分块补丁声明的产物过大（{new_len} 字节）"
        )));
    }
    // 与 zstd 补丁一致的上限：防止一个签名清单把磁盘写到溢出。
    let max_expanded = old_len
        .saturating_mul(2)
        .saturating_add(EXPAND_HEADROOM_BYTES);
    if new_len > max_expanded {
        return Err(DeltaError::PatchExpandedTooLarge {
            max_bytes: max_expanded,
        });
    }
    let actual_old = std::fs::metadata(old_path).map_err(DeltaError::Io)?.len();
    if actual_old != old_len {
        return Err(DeltaError::PatchApplyFailed(format!(
            "旧文件大小与补丁不匹配（期望 {old_len}，实际 {actual_old}）"
        )));
    }

    let mut ops_bytes = vec![0u8; ops_len as usize];
    reader
        .read_exact(&mut ops_bytes)
        .map_err(|e| DeltaError::PatchApplyFailed(format!("分块补丁指令区不完整: {e}")))?;
    let ops = parse_ops(&ops_bytes, old_len, new_len)?;

    check_disk_space(out_path, new_len)?;

    let old = map_file(old_path).map_err(DeltaError::Io)?;
    let old_bytes = mapped(&old);
    let mut decoder = zstd::stream::Decoder::new(reader)
        .map_err(|e| DeltaError::PatchApplyFailed(format!("补丁解压失败: {e}")))?;

    let mut out = BufWriter::new(
        std::fs::File::create(out_path)
            .map_err(|e| DeltaError::PatchApplyFailed(format!("创建产物失败: {e}")))?,
    );
    let mut buf = vec![0u8; 64 * 1024];
    for op in ops {
        match op {
            Op::Copy { offset, len } => {
                let start = offset as usize;
                out.write_all(&old_bytes[start..start + len as usize])
                    .map_err(|e| DeltaError::PatchApplyFailed(format!("写入产物失败: {e}")))?;
            }
            Op::Literal { len } => {
                let mut remaining = len as usize;
                while remaining > 0 {
                    let take = remaining.min(buf.len());
                    decoder.read_exact(&mut buf[..take]).map_err(|e| {
                        DeltaError::PatchApplyFailed(format!("分块补丁字面量流不完整: {e}"))
                    })?;
                    out.write_all(&buf[..take])
                        .map_err(|e| DeltaError::PatchApplyFailed(format!("写入产物失败: {e}")))?;
                    remaining -= take;
                }
            }
            Op::Patch {
                old_offset,
                old_len,
                patch_len,
            } => {
                let mut patch_bytes = vec![0u8; patch_len as usize];
                decoder.read_exact(&mut patch_bytes).map_err(|e| {
                    DeltaError::PatchApplyFailed(format!("分块补丁缺口流不完整: {e}"))
                })?;
                let start = old_offset as usize;
                let patcher = qbsdiff::Bspatch::new(&patch_bytes)
                    .map_err(|e| DeltaError::PatchApplyFailed(format!("缺口补丁无法解析: {e}")))?;
                patcher
                    .apply(&old_bytes[start..start + old_len as usize], &mut out)
                    .map_err(|e| DeltaError::PatchApplyFailed(format!("缺口补丁应用失败: {e}")))?;
            }
        }
    }
    out.flush()
        .map_err(|e| DeltaError::PatchApplyFailed(format!("写入产物失败: {e}")))?;
    out.get_ref()
        .sync_all()
        .map_err(|e| DeltaError::PatchApplyFailed(format!("刷新产物失败: {e}")))?;
    Ok(())
}

/// 解析并校验指令区：偏移必须落在旧文件内，字面量总量必须等于产物大小。
fn parse_ops(bytes: &[u8], old_file_len: u64, new_len: u64) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    let mut cursor = 0;
    let mut total: u64 = 0;
    while cursor < bytes.len() {
        let kind = bytes[cursor];
        cursor += 1;
        match kind {
            0 => {
                let fields = bytes.get(cursor..cursor + 12).ok_or_else(|| {
                    DeltaError::PatchApplyFailed("分块补丁指令区截断（COPY）".into())
                })?;
                let offset = u64::from_le_bytes(fields[..8].try_into().expect("长度已校验"));
                let len = u32::from_le_bytes(fields[8..].try_into().expect("长度已校验"));
                if offset.saturating_add(u64::from(len)) > old_file_len {
                    return Err(DeltaError::PatchApplyFailed("分块补丁 COPY 越界".into()));
                }
                total = total.saturating_add(u64::from(len));
                ops.push(Op::Copy { offset, len });
                cursor += 12;
            }
            1 => {
                let field = bytes.get(cursor..cursor + 4).ok_or_else(|| {
                    DeltaError::PatchApplyFailed("分块补丁指令区截断（LITERAL）".into())
                })?;
                let len = u32::from_le_bytes(field.try_into().expect("长度已校验"));
                total = total.saturating_add(u64::from(len));
                ops.push(Op::Literal { len });
                cursor += 4;
            }
            2 => {
                let fields = bytes.get(cursor..cursor + 20).ok_or_else(|| {
                    DeltaError::PatchApplyFailed("分块补丁指令区截断（PATCH）".into())
                })?;
                let old_offset = u64::from_le_bytes(fields[..8].try_into().expect("长度已校验"));
                let old_len = u32::from_le_bytes(fields[8..12].try_into().expect("长度已校验"));
                let out_len = u32::from_le_bytes(fields[12..16].try_into().expect("长度已校验"));
                let patch_len = u32::from_le_bytes(fields[16..20].try_into().expect("长度已校验"));
                if old_offset.saturating_add(u64::from(old_len)) > old_file_len {
                    return Err(DeltaError::PatchApplyFailed("分块补丁 PATCH 越界".into()));
                }
                if patch_len > MAX_GAP_PATCH_BYTES {
                    return Err(DeltaError::PatchApplyFailed(format!(
                        "分块补丁缺口补丁字节数异常（{patch_len}）"
                    )));
                }
                total = total.saturating_add(u64::from(out_len));
                ops.push(Op::Patch {
                    old_offset,
                    old_len,
                    patch_len,
                });
                cursor += 20;
            }
            other => {
                return Err(DeltaError::PatchApplyFailed(format!(
                    "分块补丁包含未知指令 {other}"
                )));
            }
        }
        if total > new_len {
            return Err(DeltaError::PatchApplyFailed(
                "分块补丁声明的产物大小与实际指令不符".into(),
            ));
        }
    }
    if total != new_len {
        return Err(DeltaError::PatchApplyFailed(format!(
            "分块补丁重建大小不匹配（期望 {new_len}，指令合计 {total}）"
        )));
    }
    Ok(ops)
}

fn push_copy(ops: &mut Vec<u8>, offset: u64, len: u32) {
    ops.push(0);
    ops.extend_from_slice(&offset.to_le_bytes());
    ops.extend_from_slice(&len.to_le_bytes());
}

fn push_literal(ops: &mut Vec<u8>, len: u32) {
    ops.push(1);
    ops.extend_from_slice(&len.to_le_bytes());
}

fn push_patch(ops: &mut Vec<u8>, old_offset: u64, old_len: u32, out_len: u32, patch_len: u32) {
    ops.push(2);
    ops.extend_from_slice(&old_offset.to_le_bytes());
    ops.extend_from_slice(&old_len.to_le_bytes());
    ops.extend_from_slice(&out_len.to_le_bytes());
    ops.extend_from_slice(&patch_len.to_le_bytes());
}

/// 块内容哈希：长度 + SipHash。命中后仍会逐字节比较，哈希只用于索引。
fn chunk_key(data: &[u8]) -> (u32, u64) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    (data.len() as u32, hasher.finish())
}

/// gear 哈希表：确定性生成，保证同一进程内边界稳定。
fn gear_table() -> &'static [u64; 256] {
    static TABLE: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u64; 256];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for slot in table.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *slot = state;
        }
        table
    })
}

/// 内容定义分块的迭代器：边界只由局部字节决定，位移不影响后续切分。
struct Cdc<'a> {
    data: &'a [u8],
    table: &'static [u64; 256],
    start: usize,
}

impl Iterator for Cdc<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.data.len() {
            return None;
        }
        let start = self.start;
        let limit = (start + MAX_CHUNK).min(self.data.len());
        let mut hash: u64 = 0;
        let mut end = start;
        while end < limit {
            hash = (hash << 1).wrapping_add(self.table[self.data[end] as usize]);
            end += 1;
            if end - start >= MIN_CHUNK && (hash & BOUNDARY_MASK) == 0 {
                break;
            }
        }
        self.start = end;
        Some((start, end))
    }
}

fn cdc_chunks(data: &[u8]) -> Cdc<'_> {
    Cdc {
        data,
        table: gear_table(),
        start: 0,
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::{apply_block_delta, create_patch_block_delta, is_block_delta_file};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tdx-block-delta-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    async fn roundtrip(old: &[u8], new: &[u8]) -> (Vec<u8>, u64) {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        let new_path = dir.path("new.bin");
        let patch_path = dir.path("patch.tdxb");
        std::fs::write(&old_path, old).unwrap();
        std::fs::write(&new_path, new).unwrap();

        create_patch_block_delta(&old_path, &new_path, &patch_path, 19)
            .await
            .unwrap();
        assert!(is_block_delta_file(&patch_path).await.unwrap());

        let out_path = dir.path("out.bin");
        apply_block_delta(&old_path, &patch_path, &out_path).unwrap();
        let restored = std::fs::read(&out_path).unwrap();
        let patch_size = std::fs::metadata(&patch_path).unwrap().len();
        (restored, patch_size)
    }

    #[tokio::test]
    async fn roundtrip_with_local_edits() {
        let old = pseudo_random(512 * 1024, 7);
        let mut new = old.clone();
        new[1000..1400].copy_from_slice(&pseudo_random(400, 11));
        let (restored, patch_size) = roundtrip(&old, &new).await;
        assert_eq!(restored, new);
        assert!(
            patch_size < new.len() as u64 / 4,
            "局部改动应产生小补丁，实际 {patch_size}"
        );
    }

    #[tokio::test]
    async fn roundtrip_with_insertion_shift() {
        let old = pseudo_random(256 * 1024, 3);
        // 中间插入 4 KiB：整段内容后移，CDC 仍应命中大部分块
        let mut new = Vec::with_capacity(old.len() + 4096);
        new.extend_from_slice(&old[..64 * 1024]);
        new.extend_from_slice(&pseudo_random(4096, 5));
        new.extend_from_slice(&old[64 * 1024..]);
        let (restored, patch_size) = roundtrip(&old, &new).await;
        assert_eq!(restored, new);
        assert!(
            patch_size < new.len() as u64 / 4,
            "位移不应让全部内容失配，实际补丁 {patch_size}"
        );
    }

    #[tokio::test]
    async fn roundtrip_identical_and_empty() {
        let data = pseudo_random(64 * 1024, 9);
        let (restored, patch_size) = roundtrip(&data, &data).await;
        assert_eq!(restored, data);
        assert!(patch_size < 4096, "相同文件应只有指令区，实际 {patch_size}");

        let (restored, _) = roundtrip(b"", b"").await;
        assert!(restored.is_empty());

        let (restored, _) = roundtrip(b"", b"hello").await;
        assert_eq!(restored, b"hello");
    }

    #[tokio::test]
    async fn rejects_corrupted_patch() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        std::fs::write(&old_path, pseudo_random(8192, 1)).unwrap();

        // 魔数正确但指令区截断
        let patch_path = dir.path("broken.tdxb");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TDXB1");
        bytes.push(1);
        bytes.extend_from_slice(&12u32.to_le_bytes());
        bytes.extend_from_slice(&8192u64.to_le_bytes());
        bytes.extend_from_slice(&8192u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(0); // COPY，但字段缺失
        std::fs::write(&patch_path, &bytes).unwrap();
        let out = dir.path("out.bin");
        let err = apply_block_delta(&old_path, &patch_path, &out);
        assert!(err.is_err());

        // 旧文件大小不匹配
        std::fs::write(&old_path, b"short").unwrap();
        let err = apply_block_delta(&old_path, &patch_path, &out);
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn apply_patch_dispatches_to_block_delta() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        let new_path = dir.path("new.bin");
        let patch_path = dir.path("patch.tdxb");
        let out_path = dir.path("out.bin");
        let old = pseudo_random(128 * 1024, 21);
        let mut new = old.clone();
        new[4096..4608].copy_from_slice(&pseudo_random(512, 23));
        std::fs::write(&old_path, &old).unwrap();
        std::fs::write(&new_path, &new).unwrap();

        create_patch_block_delta(&old_path, &new_path, &patch_path, 19)
            .await
            .unwrap();
        let patch_hash = crate::calculate_hash(&patch_path).await.unwrap();
        let target_hash = crate::calculate_hash(&new_path).await.unwrap();

        // 外层 apply_patch 必须识别 TDXB1 并走分块还原（而不是当成 bsdiff）
        let artifact =
            crate::apply_patch(&old_path, &patch_path, &out_path, &patch_hash, &target_hash)
                .await
                .unwrap();
        assert_eq!(artifact.sha256(), target_hash);
        assert_eq!(std::fs::read(&out_path).unwrap(), new);

        // 补丁哈希不符时必须在解析前拒绝
        let err = crate::apply_patch(
            &old_path,
            &patch_path,
            &dir.path("out2.bin"),
            "0000000000000000000000000000000000000000000000000000000000000000",
            &target_hash,
        )
        .await;
        assert!(matches!(err, Err(crate::DeltaError::HashMismatch { .. })));
    }

    #[tokio::test]
    async fn roundtrip_with_scattered_edits_uses_gap_patches() {
        let old = pseudo_random(256 * 1024, 31);
        let mut new = old.clone();
        // 每 8 KiB 改一个字节：整块都失配，但两侧缺口能定位到对应旧内容
        let mut offset = 0;
        while offset < new.len() {
            new[offset] ^= 0xFF;
            offset += 8 * 1024;
        }
        let insert_at = new.len() / 2;
        new.splice(insert_at..insert_at, pseudo_random(4096, 37));

        let (restored, patch_size) = roundtrip(&old, &new).await;
        assert_eq!(restored, new);
        assert!(
            patch_size < new.len() as u64 / 10,
            "散点修改应由缺口补丁压缩到很小，实际 {patch_size}"
        );
    }

    fn raw_patch(ops: &[u8], old_len: u64, new_len: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TDXB1");
        bytes.push(1);
        bytes.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&old_len.to_le_bytes());
        bytes.extend_from_slice(&new_len.to_le_bytes());
        bytes.extend_from_slice(ops);
        bytes
    }

    /// `COPY` 的源区间越界必须在解析阶段拒绝，不能依赖索引可信。
    #[test]
    fn rejects_out_of_bounds_copy() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        std::fs::write(&old_path, pseudo_random(8192, 1)).unwrap();

        let mut ops = Vec::new();
        super::push_copy(&mut ops, 8193, 10);
        let patch_path = dir.path("patch.tdxb");
        std::fs::write(&patch_path, raw_patch(&ops, 8192, 10)).unwrap();
        let err = apply_block_delta(&old_path, &patch_path, &dir.path("out.bin")).unwrap_err();
        assert!(
            err.to_string().contains("越界"),
            "必须报告越界，实际: {err}"
        );

        // 长度字段为 u32::MAX 的 COPY 同样被拦截
        let mut ops = Vec::new();
        super::push_copy(&mut ops, 0, u32::MAX);
        std::fs::write(&patch_path, raw_patch(&ops, 8192, u32::MAX as u64)).unwrap();
        assert!(apply_block_delta(&old_path, &patch_path, &dir.path("out2.bin")).is_err());
    }

    /// 声明一个 4 GB 的 LITERAL 不能在分配或写盘前放行。
    #[test]
    fn rejects_huge_literal_before_allocating() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        std::fs::write(&old_path, pseudo_random(8192, 1)).unwrap();

        let mut ops = Vec::new();
        super::push_literal(&mut ops, u32::MAX);
        let patch_path = dir.path("patch.tdxb");
        std::fs::write(&patch_path, raw_patch(&ops, 8192, u32::MAX as u64)).unwrap();
        let err = apply_block_delta(&old_path, &patch_path, &dir.path("out.bin")).unwrap_err();
        assert!(
            matches!(err, crate::DeltaError::PatchExpandedTooLarge { .. }),
            "必须命中展开大小上限，实际: {err}"
        );
    }

    /// 指令区声明的长度超过文件本身时立即失败（不预分配 64 MB）。
    #[test]
    fn rejects_ops_len_beyond_file() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        std::fs::write(&old_path, pseudo_random(1024, 1)).unwrap();

        let mut bytes = raw_patch(&[], 1024, 0);
        // 把 ops_len 改成 1 MiB，但文件只包含头部
        bytes[6..10].copy_from_slice(&(1024u32 * 1024).to_le_bytes());
        let patch_path = dir.path("patch.tdxb");
        std::fs::write(&patch_path, &bytes).unwrap();
        assert!(apply_block_delta(&old_path, &patch_path, &dir.path("out.bin")).is_err());
    }

    /// `PATCH` 指令的旧内容区间同样必须落在旧文件内。
    #[test]
    fn rejects_out_of_bounds_gap_patch() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        std::fs::write(&old_path, pseudo_random(8192, 1)).unwrap();

        let mut ops = vec![2u8];
        ops.extend_from_slice(&8192u64.to_le_bytes()); // old_offset = EOF
        ops.extend_from_slice(&100u32.to_le_bytes()); // old_len
        ops.extend_from_slice(&100u32.to_le_bytes()); // out_len
        ops.extend_from_slice(&10u32.to_le_bytes()); // patch_len
        let patch_path = dir.path("patch.tdxb");
        std::fs::write(&patch_path, raw_patch(&ops, 8192, 100)).unwrap();
        let err = apply_block_delta(&old_path, &patch_path, &dir.path("out.bin")).unwrap_err();
        assert!(
            err.to_string().contains("越界"),
            "必须报告越界，实际: {err}"
        );
    }

    /// 任意字节翻转都必须导致"报错或产物不同"，不能静默重建出旧内容。
    #[tokio::test]
    async fn any_byte_flip_is_detected() {
        let dir = TempDir::new();
        let old_path = dir.path("old.bin");
        let new_path = dir.path("new.bin");
        let patch_path = dir.path("patch.tdxb");
        let old = pseudo_random(64 * 1024, 41);
        let mut new = old.clone();
        new[1000..1200].copy_from_slice(&pseudo_random(200, 43));
        std::fs::write(&old_path, &old).unwrap();
        std::fs::write(&new_path, &new).unwrap();
        create_patch_block_delta(&old_path, &new_path, &patch_path, 19)
            .await
            .unwrap();

        let original = std::fs::read(&patch_path).unwrap();
        let mut checked = 0;
        for index in (0..original.len()).step_by(37) {
            let mut tampered = original.clone();
            tampered[index] ^= 0xFF;
            let tampered_path = dir.path("tampered.tdxb");
            std::fs::write(&tampered_path, &tampered).unwrap();

            let out_path = dir.path("flip-out.bin");
            let _ = std::fs::remove_file(&out_path);
            if apply_block_delta(&old_path, &tampered_path, &out_path).is_ok() {
                let produced = std::fs::read(&out_path).unwrap();
                assert_ne!(produced, new, "翻转第 {index} 字节后仍静默重建出正确内容");
            }
            checked += 1;
        }
        assert!(checked > 10, "样本太少，测试没有意义");
    }

    /// 属性测试：随机旧/新内容 + 随机改动都必须往返一致。
    #[tokio::test]
    async fn property_roundtrips_random_edits() {
        for seed in 0..10u64 {
            let size = 4096 + (seed as usize * 4099) % (96 * 1024);
            let old = pseudo_random(size, seed + 1);
            let mut new = old.clone();
            let edits = 1 + (seed as usize % 5);
            for edit in 0..edits {
                let pos = (seed as usize * 7919 + edit * 104729) % new.len();
                let len = 1 + ((seed as usize + edit) % 97).min(new.len() - pos - 1);
                new[pos..pos + len].copy_from_slice(&pseudo_random(len, seed * 31 + edit as u64));
            }
            let (restored, _) = roundtrip(&old, &new).await;
            assert_eq!(restored, new, "seed={seed} 往返失败");
        }
    }

    #[test]
    fn chunker_respects_bounds() {
        let data = pseudo_random(1024 * 1024, 42);
        let mut total = 0;
        let mut count = 0;
        for (start, end) in super::cdc_chunks(&data) {
            assert!(end > start);
            assert!(end - start <= super::MAX_CHUNK);
            assert!(end - start >= super::MIN_CHUNK || end == data.len());
            total += end - start;
            count += 1;
        }
        assert_eq!(total, data.len());
        assert!(count > 1, "1 MiB 数据应切成多块");
    }
}
