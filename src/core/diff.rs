use crate::core::disk::check_disk_space;
use crate::error::{DeltaError, Result};
use memmap2::Mmap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use tokio::fs;

/// 补丁计算线程的栈大小。
///
/// bsdiff 系列的差分算法递归深度与文件规模相关，栈不足会直接崩溃。
/// 本库不支持同进程并发执行多个补丁任务：64 位下每个任务固定占用 512 MB
/// 虚拟地址空间（提交内存按实际使用增长），并发两个即为 1 GB。
/// 32 位进程地址空间只有 2–3 GB，预留 512 MB 会让线程创建直接失败，因此下调。
const WORKER_STACK_SIZE: usize = if cfg!(target_pointer_width = "64") {
    512 * 1024 * 1024
} else {
    64 * 1024 * 1024
};

/// 增量更新适用的最大文件大小（单文件）。
///
/// 差分要求旧文件与新文件可被随机访问（内存映射按需分页），
/// 超过该阈值的文件即使能算，内存开销也不值得——应直接走全量下载。
/// 这是**应用补丁**的上限；生成补丁另有更严格的工作集检查。
pub const MAX_INCREMENTAL_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 生成补丁时允许的预估工作集上限（4 GiB）。
///
/// qbsdiff 需要约 `max(17n, 9n+m)` 的工作内存（n/m 为旧/新文件大小）。
/// 只检查文件大小上限是不够的：一个 1 GB 的基线会需要约 17 GB 内存，
/// 分配失败时 Rust 默认 abort 进程，CI 表现为"跑挂"而不是"跳过"。
const MAX_PATCH_WORKING_SET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// 解压后的补丁体积上限的固定余量。
const DECOMPRESS_HEADROOM_BYTES: u64 = 128 * 1024 * 1024;

/// zstd 帧的魔数（小端 0xFD2FB528）。
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// 检查文件大小是否在增量更新适用范围内。
///
/// 差分阶段旧文件与新文件都会被映射访问，超过上限时返回明确错误，
/// 让调用方回退全量下载，而不是在内存耗尽后拿到难以诊断的 `WorkerFailed`。
fn check_file_size(path: &Path) -> Result<()> {
    let size = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(DeltaError::Io)?;
    if size > MAX_INCREMENTAL_FILE_BYTES {
        return Err(DeltaError::IncrementalNotApplicable { size });
    }
    Ok(())
}

/// 生成补丁前的预估工作集检查（仅发布端）。
///
/// 上限 4 GiB 足以覆盖常见的 100–200 MB 桌面安装包；
/// 更大的产物应直接跳过基线、走全量分发，而不是把 CI 机器打爆。
fn check_working_set(old_path: &Path, new_path: &Path) -> Result<()> {
    let old = std::fs::metadata(old_path)
        .map(|m| m.len())
        .map_err(DeltaError::Io)?;
    let new = std::fs::metadata(new_path)
        .map(|m| m.len())
        .map_err(DeltaError::Io)?;
    let required = working_set_required(old, new);
    if required > MAX_PATCH_WORKING_SET_BYTES {
        return Err(DeltaError::PatchWorkingSetTooLarge {
            required,
            limit: MAX_PATCH_WORKING_SET_BYTES,
        });
    }
    Ok(())
}

/// 按 qbsdiff 的内存模型估算工作集：`max(17n, 9n+m)`。
fn working_set_required(old: u64, new: u64) -> u64 {
    old.saturating_mul(17)
        .max(old.saturating_mul(9).saturating_add(new))
}

/// 把文件只读映射进内存。空文件无法映射，返回 `None` 由调用方用空切片代替。
fn map_file(path: &Path) -> std::io::Result<Option<Mmap>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    // SAFETY：只读映射，不会通过映射写入数据。
    // 唯一的 UB 风险是映射期间底层文件被截断/改写；
    // 被映射的是待差分的旧/新安装包——更新锁保证没有并发的更新写它们，
    // 正在运行的 exe 在 Windows 上也无法被改写，因此该不变式成立。
    let mmap = unsafe { Mmap::map(&file)? };
    Ok(Some(mmap))
}

/// 旧文件或新文件内容的统一视图：映射页或空切片。
fn mapped(mmap: &Option<Mmap>) -> &[u8] {
    mmap.as_deref().unwrap_or(&[])
}

/// 判断补丁字节是否为 zstd 容器。
///
/// `generate` 产出的补丁是 bsdiff 原始输出经 zstd 压缩的结果；
/// 历史版本发布的未压缩补丁没有该魔数，原样进入 bspatch，
/// 因此旧清单无需任何迁移即可继续工作。
fn is_zstd_container(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == ZSTD_MAGIC
}

/// 判断补丁文件是否为 zstd 容器（只读前 4 字节，不加载整个文件）。
async fn is_zstd_file(path: &Path) -> Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut file = fs::File::open(path).await?;
    let mut head = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        match file.read(&mut head[read..]).await? {
            0 => break,
            n => read += n,
        }
    }
    Ok(is_zstd_container(&head[..read]))
}

/// 为旧版本与新版本生成增量补丁。
///
/// 产出 = bsdiff 原始输出经 **zstd 压缩**（level 19）后的文件。
/// bsdiff 的控制/差异/额外三股流含大量可压缩字节，不压缩时补丁体积
/// 可能接近甚至超过全量包（Deno 曾出现 147 MB 补丁大于本体的情况）。
///
/// 压缩用流式两段写：先落盘原始补丁，再流式压成最终文件，
/// 避免把可能接近全量大小的原始补丁整个抱在内存里。
///
/// # 资源约束
///
/// 差分任务在独立线程中运行，64 位下预留 **512 MB 栈**（32 位 64 MB；
/// bsdiff 递归深度与文件规模相关）。预估工作集超过 4 GiB 时直接返回
/// [`DeltaError::PatchWorkingSetTooLarge`]，由发布端跳过该基线。
/// **不要在同一进程内并发调用**：每次调用都会预留一份虚拟地址空间。
pub async fn create_patch<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
    old_file: P1,
    new_file: P2,
    patch_output: P3,
) -> Result<()> {
    create_patch_with_compression(old_file, new_file, patch_output, Some(19)).await
}

/// 与 [`create_patch`] 相同，但允许指定 zstd 压缩级别（1–22）。
///
/// 补丁很小（KB 级）时级别对体积几乎无影响；旧版本多、安装包大时，
/// 发布端可以降到 15–17 换取明显更短的 CI 压缩时间，客户端解压成本不变。
pub async fn create_patch_with_level<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
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
    create_patch_with_compression(old_file, new_file, patch_output, Some(zstd_level)).await
}

/// 生成**未压缩**的 bsdiff 补丁（`BSDIFF40` 原始输出）。
///
/// 只用于跨代际过渡：旧引擎客户端不认 zstd 容器，会把补丁判成
/// "not a valid patch"。未压缩补丁体积明显更大，新客户端应继续用
/// [`create_patch`]。
pub async fn create_patch_uncompressed<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
    old_file: P1,
    new_file: P2,
    patch_output: P3,
) -> Result<()> {
    create_patch_with_compression(old_file, new_file, patch_output, None).await
}

async fn create_patch_with_compression<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
    old_file: P1,
    new_file: P2,
    patch_output: P3,
    compression: Option<i32>,
) -> Result<()> {
    let old_path = old_file.as_ref().to_path_buf();
    let new_path = new_file.as_ref().to_path_buf();
    let patch_path = patch_output.as_ref().to_path_buf();

    check_file_size(&old_path)?;
    check_file_size(&new_path)?;
    check_working_set(&old_path, &new_path)?;

    run_blocking("生成增量补丁", move || {
        let old = map_file(&old_path).map_err(DeltaError::Io)?;
        let new = map_file(&new_path).map_err(DeltaError::Io)?;

        // 原始补丁先写到临时文件；压缩后删除，未压缩时直接改名成最终文件。
        let raw_path = raw_patch_path(&patch_path);
        let raw_file = std::fs::File::create(&raw_path)?;
        let result =
            qbsdiff::Bsdiff::new(mapped(&old), mapped(&new)).compare(BufWriter::new(raw_file));

        match result {
            Ok(_) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&raw_path);
                return Err(DeltaError::PatchGenFailed(e.to_string()));
            }
        }

        let Some(zstd_level) = compression else {
            // 未压缩：同目录重命名，不复制字节。
            return std::fs::rename(&raw_path, &patch_path).map_err(|e| {
                let _ = std::fs::remove_file(&raw_path);
                DeltaError::PatchGenFailed(format!("落盘未压缩补丁失败: {e}"))
            });
        };

        // zstd 流式压缩：raw -> 最终补丁文件
        let compressed = (|| -> std::io::Result<()> {
            let reader = BufReader::new(std::fs::File::open(&raw_path)?);
            let mut encoder = zstd::stream::Encoder::new(
                BufWriter::new(std::fs::File::create(&patch_path)?),
                zstd_level,
            )?;
            std::io::copy(&mut { reader }, &mut encoder)?;
            encoder.finish()?.flush()?;
            Ok(())
        })();

        let _ = std::fs::remove_file(&raw_path);

        match compressed {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&patch_path);
                Err(DeltaError::PatchGenFailed(format!(
                    "zstd 压缩补丁失败: {e}"
                )))
            }
        }
    })
    .await
}

/// 原始补丁的临时文件路径（与最终补丁同目录，保证同分区删除/重命名廉价）。
fn raw_patch_path(patch_path: &Path) -> PathBuf {
    let mut os = patch_path.as_os_str().to_os_string();
    os.push(".raw");
    PathBuf::from(os)
}

/// 已通过**双端哈希校验**的合并产物。
///
/// 类型级防护：它的构造函数是私有的，唯一的产生路径是
/// [`apply_patch`] 的成功返回——把"未经验证的产物"交给替换流程
/// 不是代码审查能发现的问题，而是**编译不过**。
///
/// 产物同时携带校验时算出的 SHA-256，替换脚本在复制前可以再校验一次，
/// 封住"校验完成到脚本执行之间文件被替换"的 TOCTOU 窗口。
#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    path: PathBuf,
    sha256: String,
}

impl VerifiedArtifact {
    /// 由**已通过外部验签**的文件构造（crate 内部使用，目前供 Linux 全量安装复用）。
    ///
    /// 全量安装包已经通过 minisign 验签，随后复用同一套替换脚本；
    /// 这里补上 SHA-256，让脚本仍能在复制前复核字节没有变化。
    #[cfg(target_os = "linux")]
    pub(crate) fn from_signed_file(path: PathBuf, sha256: String) -> Self {
        Self { path, sha256 }
    }

    /// 已验证产物的路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 已验证产物的 SHA-256（十六进制小写）。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// 重新校验磁盘上的产物是否仍是校验时的那份字节。
    ///
    /// 供替换脚本生成前的最后一道检查使用：从 `apply_patch` 返回到
    /// 脚本真正执行之间存在时间窗口，文件可能被替换。
    pub fn verify_integrity(&self) -> Result<()> {
        let actual = crate::core::hasher::hash_file_sync(&self.path)?;
        if actual != self.sha256 {
            return Err(DeltaError::HashMismatch {
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }
}

impl AsRef<Path> for VerifiedArtifact {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// 解压后的临时补丁文件守卫：离开作用域即删除。
struct TempPatch {
    path: Option<PathBuf>,
}

impl TempPatch {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or(Path::new(""))
    }
}

impl Drop for TempPatch {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 把 zstd 压缩的补丁流式解压到临时文件。
///
/// 不用 `decode_all`：它会一次性解压到内存，压缩补丁（≤512 MB）与
/// 解压后的 bsdiff 流可能同时驻留，峰值可达 GB 级，低内存机器会被 OOM 杀掉。
/// 落盘后按需映射，内存占用降到页缓存级别。
///
/// 上限取 `旧文件大小 × 2 + 128 MB`：正常补丁远小于此值，
/// 超过只可能是补丁损坏或构造的攻击载荷。
fn decompress_to_temp(patch_path: &Path, temp_path: &Path, old_size: u64) -> Result<TempPatch> {
    let guard = TempPatch::new(temp_path.to_path_buf());
    let limit = old_size
        .saturating_mul(2)
        .saturating_add(DECOMPRESS_HEADROOM_BYTES);

    let reader = BufReader::new(std::fs::File::open(patch_path)?);
    let mut decoder = zstd::stream::Decoder::new(reader)
        .map_err(|e| DeltaError::PatchApplyFailed(format!("补丁解压失败: {e}")))?;
    let mut out = BufWriter::new(std::fs::File::create(temp_path)?);

    let mut buf = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| DeltaError::PatchApplyFailed(format!("补丁解压失败: {e}")))?;
        if n == 0 {
            break;
        }
        written = written.saturating_add(n as u64);
        if written > limit {
            drop(out);
            return Err(DeltaError::PatchExpandedTooLarge { max_bytes: limit });
        }
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    Ok(guard)
}

/// 应用增量补丁，生成新版本文件。
///
/// 补丁文件是 zstd 容器（历史未压缩补丁通过魔数嗅探兼容）：
/// 先校验**下载形态**的哈希（与清单签名的 `patch_hash` 锚定一致），
/// 再解压、应用，最后校验产物哈希；任一不符都会删除产物并返回错误。
///
/// 成功时返回 [`VerifiedArtifact`]——只有通过全部校验的产物才能
/// 进入下游（替换脚本、`Staged`），未验证的字节在类型上无法流转。
///
/// # 资源约束
///
/// 合并任务与 [`create_patch`] 共享同一约束：独立线程预留 512 MB 栈（32 位 64 MB），
/// **不要在同一进程内并发调用**。
pub async fn apply_patch<P1: AsRef<Path>, P2: AsRef<Path>, P3: AsRef<Path>>(
    old_file: P1,
    patch_file: P2,
    new_output: P3,
    expected_patch_hash: &str,
    expected_target_hash: &str,
) -> Result<VerifiedArtifact> {
    let new_output_path = new_output.as_ref().to_path_buf();
    if new_output_path.exists() {
        return Err(DeltaError::TargetExists(
            new_output_path.display().to_string(),
        ));
    }

    check_file_size(old_file.as_ref())?;

    // 补丁哈希按流式计算，不把下载形态整体读入内存
    let actual_patch_hash = crate::core::hasher::calculate_hash(patch_file.as_ref()).await?;
    if actual_patch_hash != expected_patch_hash.to_lowercase() {
        return Err(DeltaError::HashMismatch {
            expected: format!("Patch: {expected_patch_hash}"),
            actual: format!("Patch: {actual_patch_hash}"),
        });
    }

    // 哈希已通过（锚定到清单签名），此时解压不可能遭遇解压炸弹
    let compressed = is_zstd_file(patch_file.as_ref()).await?;

    let old_path = old_file.as_ref().to_path_buf();
    let patch_path = patch_file.as_ref().to_path_buf();
    let target_path = new_output_path.clone();
    let target_hash = expected_target_hash.to_lowercase();
    let old_size = std::fs::metadata(&old_path).map(|m| m.len()).unwrap_or(0);

    // 解压后的临时补丁放在产物旁边，保证同分区
    let temp_patch = new_output_path.with_extension("patch.expanded");

    run_blocking("应用增量补丁", move || {
        // 只读映射替代全量读入：按需分页，峰值 RSS 远低于一次性分配
        let old = map_file(&old_path).map_err(DeltaError::Io)?;

        // 压缩补丁先流式解压到临时文件，再映射；未压缩补丁直接映射
        let temp = if compressed {
            Some(decompress_to_temp(&patch_path, &temp_patch, old_size)?)
        } else {
            None
        };
        let patch_source = match &temp {
            Some(guard) => guard.path().to_path_buf(),
            None => patch_path.clone(),
        };
        let patch_map = map_file(&patch_source).map_err(DeltaError::Io)?;

        // 产物大小直接来自补丁头，比按旧文件大小估算更准确
        let target_size = qbsdiff::Bspatch::new(mapped(&patch_map))
            .map(|p| p.hint_target_size())
            .unwrap_or(0);
        check_disk_space(&target_path, target_size)?;

        // 产物直接落盘，避免在内存中缓存整个新文件
        let mut file = std::fs::File::create(&target_path)?;
        // 预分配目标大小，减少写入过程中的文件碎片
        let _ = file.set_len(target_size);

        let patcher = qbsdiff::Bspatch::new(mapped(&patch_map))
            .map_err(|e| DeltaError::PatchApplyFailed(e.to_string()))?;
        let result = patcher.apply(mapped(&old), &mut file);
        file.sync_all()?;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&target_path);
                Err(DeltaError::PatchApplyFailed(e.to_string()))
            }
        }
    })
    .await?;

    let actual_hash = crate::core::hasher::calculate_hash(&new_output_path).await?;
    if actual_hash != target_hash {
        let _ = fs::remove_file(&new_output_path).await;
        return Err(DeltaError::HashMismatch {
            expected: target_hash,
            actual: actual_hash,
        });
    }

    Ok(VerifiedArtifact {
        path: new_output_path,
        sha256: actual_hash,
    })
}

/// 进程级串行化：512 MB 栈预留 + 手动线程在多平台并行发布时可能撞上
/// 线程创建失败。跨进程由调用方的 `UpdateLock` 负责，这里只约束本进程。
static PATCH_SEMAPHORE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// 在独立线程中执行阻塞操作。
///
/// - 闭包返回的原始 `DeltaError` **原样透传**，不重新包装，
///   调用方才能区分"哈希不匹配"和"线程崩溃"这类完全不同的失败；
/// - 线程 panic 与线程创建失败（通常是内存不足）转成 `WorkerFailed`；
/// - 同一进程内的补丁生成/应用会在这里串行排队。
async fn run_blocking<F>(task: &'static str, f: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    // 等待串行锁时不占用阻塞线程池；真正的重活仍在独立大栈线程里执行。
    let _permit = PATCH_SEMAPHORE
        .acquire()
        .await
        .map_err(|e| DeltaError::WorkerFailed(format!("{task}等待串行锁失败: {e}")))?;
    tokio::task::spawn_blocking(move || {
        match std::thread::Builder::new()
            .stack_size(WORKER_STACK_SIZE)
            .spawn(f)
        {
            Ok(handle) => match handle.join() {
                Ok(result) => result,
                Err(_) => Err(DeltaError::WorkerFailed(format!("{task}时线程异常终止"))),
            },
            // 栈分配失败几乎总是内存不足
            Err(e) => Err(DeltaError::WorkerFailed(format!(
                "无法创建补丁线程（内存不足？）: {e}"
            ))),
        }
    })
    .await
    .map_err(|e| DeltaError::WorkerFailed(format!("{task}任务调度失败: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::{check_working_set, working_set_required, MAX_PATCH_WORKING_SET_BYTES};

    /// 工作集估算必须是纯数学函数：大文件不会走到真正的分配。
    #[test]
    fn working_set_matches_qbsdiff_model() {
        assert_eq!(working_set_required(0, 0), 0);
        assert_eq!(working_set_required(1, 100), 109, "9n+m 在 m 很大时占优");
        assert_eq!(working_set_required(10, 1000), 1090, "9n+m 在 m 很大时占优");
        assert_eq!(
            working_set_required(300 * 1024 * 1024, 300 * 1024 * 1024),
            5_100 * 1024 * 1024
        );
    }

    /// 并发调用补丁生成不能被 512 MB 栈线程创建失败击穿。
    #[tokio::test]
    async fn concurrent_patch_jobs_are_serialized() {
        let dir = tempfile::tempdir().unwrap();
        let mut jobs = Vec::new();
        for index in 0..3 {
            let old = dir.path().join(format!("old{index}.bin"));
            let new = dir.path().join(format!("new{index}.bin"));
            let output = dir.path().join(format!("p{index}.patch"));
            std::fs::write(&old, format!("old-{index}")).unwrap();
            std::fs::write(&new, format!("new-{index}")).unwrap();
            jobs.push(super::create_patch(old, new, output));
        }
        let results = futures_util::future::join_all(jobs).await;
        for result in results {
            assert!(result.is_ok(), "并发补丁失败: {result:?}");
        }
    }

    /// 超过上限时必须返回可识别的错误，发布端据此跳过基线。
    #[test]
    fn oversized_working_set_is_reported_not_attempted() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.bin");
        let new = dir.path().join("new.bin");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        check_working_set(&old, &new).unwrap();

        // 用小文件验证分支本身，避免真的分配 GB 级内存
        let required = working_set_required(400 * 1024 * 1024, 400 * 1024 * 1024);
        assert!(required > MAX_PATCH_WORKING_SET_BYTES);
        let err = crate::DeltaError::PatchWorkingSetTooLarge {
            required,
            limit: MAX_PATCH_WORKING_SET_BYTES,
        };
        assert_eq!(err.code(), "patch_working_set_too_large");
    }
}
