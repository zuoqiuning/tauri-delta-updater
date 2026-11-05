//! 底层能力：差分、下载、校验、锁与安全策略。
//!
//! 这一层不依赖任何上层编排，可以单独使用与测试。

/// 更新流程在磁盘上的产物文件名。
pub mod artifacts;

/// TLS 加密后端安装。
pub mod crypto;

/// 二进制差分与合并。
pub mod diff;

/// 磁盘空间检查与临时文件清理。
pub mod disk;

/// 补丁下载（支持断点续传与重试）。
pub mod downloader;

/// 可选结构化事件（内部使用）。
pub(crate) mod events;

/// SHA-256 计算。
pub mod hasher;

/// 跨进程更新锁。
pub mod lock;

/// 运行时路径推导与可写性探测。
pub mod paths;

/// 安全模式：连续失败计数。
pub mod safety;

/// 清单签名校验。
pub mod verify;
