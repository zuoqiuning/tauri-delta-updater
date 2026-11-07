//! 更新取消令牌。
//!
//! 取消是**协作式**的：令牌只设置一个原子标志，下载循环与各阶段边界主动检查。
//! 这样不需要引入 `AbortHandle` / `select!` 之类的异步取消机制，
//! 也不会在取消时留下半截文件。
//!
//! 语义边界：
//!
//! - 下载阶段的取消会立即中止流式写入并删除半截补丁；
//! - 合并阶段的取消只能在合并完成后生效（差分本身不可中断）；
//! - 启动替换脚本之后无法撤销——脚本已在等待旧进程退出。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 可在任意线程触发的取消令牌。
///
/// 克隆出来的句柄共享同一标志：宿主把克隆体交给 UI 线程，更新流程持有另一个，
/// 用户点"取消"时只会设置标志，不会阻塞更新任务。
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// 创建一个未取消的令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。幂等，可重复调用。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// 是否已被取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
