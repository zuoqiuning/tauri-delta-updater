import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

// 官方 updater 的典型写法：check() 返回 Update，downloadAndInstall 接收回调。
export async function run(): Promise<void> {
  const update = await check();
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

  await relaunch();
}
