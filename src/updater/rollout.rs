//! 灰度发布：把清单里的 `rollout` 应用到当前安装。

use super::DeltaUpdater;
use crate::error::Result;
use crate::models::UpdateManifest;

impl DeltaUpdater {
    /// 当前安装的稳定分桶 ID。
    ///
    /// 优先使用宿主显式配置的 [`super::UpdateConfig::install_id`]；
    /// 否则读取或创建工作目录里的 `tdx_install_id`。
    /// 极端情况下（工作目录突然不可写）退回可执行文件路径，
    /// 保证不会被静默当成"全量放行"。
    pub(super) fn rollout_install_id(&self) -> String {
        if let Some(id) = self.config.install_id.as_deref().map(str::trim) {
            if !id.is_empty() {
                return id.to_string();
            }
        }
        crate::core::rollout::load_or_create_install_id(&self.work_dir)
            .unwrap_or_else(|_| self.executable_path.display().to_string())
    }

    /// 清单给当前平台声明的放量比例；未声明时为 `None`（全量）。
    pub(super) fn rollout_percentage(&self, manifest: &UpdateManifest) -> Option<u8> {
        manifest
            .platforms
            .get(&self.config.platform)
            .and_then(|platform| platform.rollout)
            .map(|rollout| rollout.percentage.min(100))
    }

    /// 当前安装是否被灰度挡在本次版本之外。
    ///
    /// `min_version` 是灰度之上的强制门槛：低于它的客户端无视比例，
    /// 必须更新（用于把用户从已知坏版本上拉走）。
    pub(super) fn deferred_by_rollout(&self, manifest: &UpdateManifest) -> Result<bool> {
        let Some(percentage) = self.rollout_percentage(manifest) else {
            return Ok(false);
        };
        if manifest.is_below_min_version(&self.config.current_version)? {
            return Ok(false);
        }
        Ok(!crate::core::rollout::selected(
            &self.rollout_install_id(),
            &manifest.version,
            percentage,
        ))
    }
}
