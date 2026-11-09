import { checkCompat, install, requiresExit } from '@tauri-apps/plugin-delta-updater';

// 方案 A：保留官方 check() → downloadAndInstall() 的调用形状。
// checkCompat() 返回 null 表示无更新 / 灰度未命中 / 系统版本过低；
// 增量不可用时它会自动切到 installFull()（capability 需包含
// `delta-updater:full`）。
export async function runCompat(): Promise<void> {
  const update = await checkCompat();
  if (!update) {
    return;
  }

  console.log(`found ${update.version} (current ${update.currentVersion})`);
  await update.downloadAndInstall((event) => {
    switch (event.event) {
      case 'Started':
        console.log(`download ${event.data.contentLength ?? 0} bytes`);
        break;
      case 'Progress':
        console.log(`+${event.data.chunkLength} bytes`);
        break;
      case 'Finished':
        console.log('download finished');
        break;
    }
  });
}

// 方案 B：直接用本插件的阶段事件。插件在 `updated` / `repair_required`
// 时会自动退出进程；requiresExit 只用于你自己控制退出时机（例如先发 IPC）。
export async function runNative(): Promise<void> {
  const result = await install((event) => {
    switch (event.stage) {
      case 'downloading':
      case 'downloading_full':
        console.log(`download ${event.downloaded}/${event.total}`);
        break;
      case 'applying':
        console.log('applying');
        break;
      case 'ready':
        console.log(`ready ${event.version}`);
        break;
      case 'safe_mode':
        console.log('safe mode: show a manual download link');
        break;
      default:
        break;
    }
  });

  if (requiresExit(result)) {
    // 插件已经安排退出；这里只做最后的本地清理。
  }
}
