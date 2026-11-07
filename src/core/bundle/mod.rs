//! 整包（多文件）更新的确定性容器。
//!
//! 单文件补丁只能覆盖主程序；使用 `externalBin`（sidecar）或 `resources` 的
//! 应用需要把整个安装产物当作一个整体来差分。这里定义两端的共同约定：
//!
//! 1. **文件清单**：[`BundleFile`] 列出相对路径、大小与 SHA-256；
//! 2. **确定性归档**：按路径排序、固定头格式，同一份文件树永远产出同一份字节；
//! 3. **安全提取**：拒绝绝对路径、`..`、盘符与反斜杠，写盘后再校验哈希。
//!
//! 归档本身**不是**压缩包格式，而是"头 + 原始字节"：差分算法直接作用在它上面，
//! 压缩由补丁文件外侧的 zstd 负责。两端都必须用本模块构建/读取，任何外部归档
//! 工具产生的字节顺序都不可信。
//!
//! ```no_run
//! # fn example() -> tauri_delta_updater::Result<()> {
//! let files = tauri_delta_updater::core::bundle::describe_dir("app")?;
//! tauri_delta_updater::core::bundle::build_archive("app", &files, "app.bundle")?;
//!
//! let listed = tauri_delta_updater::core::bundle::list_archive("app.bundle")?;
//! tauri_delta_updater::core::bundle::extract_archive("app.bundle", &listed, "staging")?;
//! # Ok(())
//! # }
//! ```

mod archive;
mod patch;

pub use archive::{
    build_archive, describe_dir, entry_for, extract_archive, list_archive, verify_layout,
    BundleFile, BundleFileKind,
};
pub use patch::{apply_bundle_patch, create_bundle_patch, BundleManifest, BundlePatch};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DeltaError;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn write(root: &Path, relative: &str, content: &[u8]) {
        let path: PathBuf = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn round_trip_is_deterministic() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root, "app.exe", b"main binary");
        write(&root, "resources/icon.png", b"icon bytes");
        write(&root, "sidecars/helper.exe", b"helper");

        let files = describe_dir(&root).unwrap();
        assert_eq!(files.len(), 3);

        let first = dir.path().join("a.bundle");
        let second = dir.path().join("b.bundle");
        build_archive(&root, &files, &first).unwrap();
        build_archive(&root, &files, &second).unwrap();
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap(),
            "同一文件树必须产出完全相同的归档字节"
        );

        let listed = list_archive(&first).unwrap();
        assert_eq!(listed, files);

        let out = dir.path().join("staging");
        extract_archive(&first, &files, &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("resources/icon.png")).unwrap(),
            b"icon bytes"
        );
        assert_eq!(
            std::fs::read(out.join("sidecars/helper.exe")).unwrap(),
            b"helper"
        );
    }

    #[test]
    fn tampered_archive_is_rejected() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root, "a.bin", b"0123456789");

        let files = describe_dir(&root).unwrap();
        let archive = dir.path().join("a.bundle");
        build_archive(&root, &files, &archive).unwrap();

        // 翻转内容最后一个字节：头部的哈希没变，内容哈希必须暴露篡改。
        let mut bytes = std::fs::read(&archive).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&archive, bytes).unwrap();

        let out = dir.path().join("staging");
        assert!(extract_archive(&archive, &files, &out).is_err());
    }

    #[test]
    fn verify_layout_detects_changes() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root, "a.bin", b"original");
        let files = describe_dir(&root).unwrap();

        verify_layout(&root, &files).unwrap();

        write(&root, "a.bin", b"modified");
        match verify_layout(&root, &files) {
            Err(DeltaError::HashMismatch { .. }) => {}
            other => panic!("改动文件必须被拒绝，实际: {other:?}"),
        }

        std::fs::remove_file(root.join("a.bin")).unwrap();
        assert!(matches!(
            verify_layout(&root, &files),
            Err(DeltaError::BundleInvalid(_))
        ));
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempdir().unwrap();
        for bad in ["../evil", "/abs", "a/../../evil", "C:/evil", "a\\b"] {
            let files = vec![BundleFile {
                path: bad.into(),
                size: 0,
                sha256: "0".repeat(64),
                mode: None,
                kind: BundleFileKind::File,
            }];
            let out = dir.path().join("x.bundle");
            assert!(
                build_archive(dir.path(), &files, &out).is_err(),
                "{bad} 必须被拒绝"
            );
        }
    }

    #[test]
    fn rejects_truncated_archive() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root, "a.bin", b"payload");
        let files = describe_dir(&root).unwrap();
        let archive = dir.path().join("a.bundle");
        build_archive(&root, &files, &archive).unwrap();

        let mut bytes = std::fs::read(&archive).unwrap();
        bytes.truncate(bytes.len() - 4);
        std::fs::write(&archive, bytes).unwrap();

        let out = dir.path().join("staging");
        assert!(extract_archive(&archive, &files, &out).is_err());
    }

    fn copy_tree(from: &Path, to: &Path) {
        for file in describe_dir(from).unwrap() {
            let bytes = std::fs::read(from.join(&file.path)).unwrap();
            write(to, &file.path, &bytes);
        }
    }

    /// 整包生成 + 应用 + 提取的闭环。
    #[tokio::test]
    async fn bundle_patch_round_trip() {
        let dir = tempdir().unwrap();
        let old_root = dir.path().join("old");
        let new_root = dir.path().join("new");
        write(&old_root, "app.exe", b"old main binary");
        write(&old_root, "resources/app.js", b"old resource");
        write(&old_root, "sidecars/helper.exe", b"same helper");

        write(&new_root, "app.exe", b"new main binary, v2");
        write(&new_root, "resources/app.js", b"new resource, v2");
        write(&new_root, "resources/extra.txt", b"added file");
        write(&new_root, "sidecars/helper.exe", b"same helper");

        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let patch_path = dir.path().join("patch.bin");

        let bundle = create_bundle_patch(
            &old_root,
            &new_root,
            "0.1.0",
            "0.1.1",
            "https://cdn.example.com/update.patch",
            &patch_path,
            &work,
        )
        .await
        .unwrap();

        assert_eq!(bundle.base_files.len(), 3);
        assert_eq!(bundle.target_files.len(), 4, "新增资源必须出现在新清单");

        // 模拟客户端的安装目录（旧版本）
        let install = dir.path().join("install");
        copy_tree(&old_root, &install);

        let staging = dir.path().join("staging");
        apply_bundle_patch(&install, &patch_path, &bundle, &staging, &work)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(staging.join("app.exe")).unwrap(),
            b"new main binary, v2"
        );
        assert_eq!(
            std::fs::read(staging.join("resources/app.js")).unwrap(),
            b"new resource, v2"
        );
        assert_eq!(
            std::fs::read(staging.join("resources/extra.txt")).unwrap(),
            b"added file"
        );
        assert_eq!(
            std::fs::read(staging.join("sidecars/helper.exe")).unwrap(),
            b"same helper"
        );
    }

    /// Unix 上符号链接与可执行位必须原样往返（macOS `.app` 重建依赖这两点）。
    #[cfg(unix)]
    #[test]
    fn preserves_symlinks_and_modes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root, "Contents/MacOS/MyApp", b"binary");
        std::fs::set_permissions(
            root.join("Contents/MacOS/MyApp"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        // 典型的框架链接：Versions/Current -> A
        std::fs::create_dir_all(root.join("Contents/Frameworks/Foo.framework/Versions/A")).unwrap();
        std::os::unix::fs::symlink(
            "A",
            root.join("Contents/Frameworks/Foo.framework/Versions/Current"),
        )
        .unwrap();

        let files = describe_dir(&root).unwrap();
        let binary = files
            .iter()
            .find(|f| f.path == "Contents/MacOS/MyApp")
            .unwrap();
        assert_eq!(binary.mode, Some(0o755), "可执行位必须被记录");

        let link = files
            .iter()
            .find(|f| f.path.ends_with("Versions/Current"))
            .unwrap();
        assert_eq!(link.kind, BundleFileKind::Symlink);
        assert_eq!(link.size, 1, "链接目标是 'A'");

        let archive = dir.path().join("app.bundle");
        build_archive(&root, &files, &archive).unwrap();
        assert_eq!(list_archive(&archive).unwrap(), files);

        let out = dir.path().join("staging");
        extract_archive(&archive, &files, &out).unwrap();
        assert!(
            std::fs::symlink_metadata(
                out.join("Contents/Frameworks/Foo.framework/Versions/Current")
            )
            .unwrap()
            .file_type()
            .is_symlink(),
            "符号链接必须被重建"
        );
        assert_eq!(
            std::fs::metadata(out.join("Contents/MacOS/MyApp"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "权限位必须被恢复"
        );
    }

    /// 指向包外或绝对路径的符号链接必须被拒绝。
    #[cfg(unix)]
    #[test]
    fn rejects_escaping_symlink_targets() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        std::fs::create_dir_all(&root).unwrap();
        for target in ["../outside", "/etc/passwd"] {
            let link = root.join("bad");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(target, &link).unwrap();
            let entry = entry_for(&root, "bad").unwrap_err();
            assert!(
                matches!(entry, DeltaError::BundleInvalid(_)),
                "{target} 必须被拒绝，实际: {entry:?}"
            );
        }
    }

    /// 整包清单只返回与清单版本、平台都一致的补丁。
    #[test]
    fn manifest_matches_version_and_platform() {
        let patch = BundlePatch {
            from_version: "0.1.0".into(),
            to_version: "0.2.0".into(),
            patch_url: "https://cdn.example.com/update.patch".into(),
            patch_hash: "a".repeat(64),
            target_hash: "b".repeat(64),
            patch_size: None,
            base_files: vec![],
            target_files: vec![],
        };
        let manifest = BundleManifest {
            version: "0.2.0".into(),
            platform: "windows-x86_64".into(),
            bundles: vec![patch.clone()],
        };

        assert!(manifest.find_patch("0.1.0", "windows-x86_64").is_some());
        assert!(
            manifest.find_patch("0.1.0", "darwin-aarch64").is_none(),
            "不能把其它平台的补丁套到当前平台"
        );
        assert!(manifest.find_patch("0.0.9", "windows-x86_64").is_none());

        // 发布端把补丁的 to_version 写错时不得命中
        let broken = BundleManifest {
            bundles: vec![BundlePatch {
                to_version: "0.1.9".into(),
                ..patch
            }],
            ..manifest
        };
        assert!(broken.find_patch("0.1.0", "windows-x86_64").is_none());
    }

    /// 本地安装目录被改动后必须拒绝应用（否则补丁基底不可信）。
    #[tokio::test]
    async fn bundle_patch_rejects_modified_install() {
        let dir = tempdir().unwrap();
        let old_root = dir.path().join("old");
        let new_root = dir.path().join("new");
        write(&old_root, "app.exe", b"old main");
        write(&old_root, "resources/a.txt", b"old resource");
        write(&new_root, "app.exe", b"new main, longer content");
        write(&new_root, "resources/a.txt", b"new resource, longer");

        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let patch_path = dir.path().join("patch.bin");
        let bundle = create_bundle_patch(
            &old_root,
            &new_root,
            "0.1.0",
            "0.1.1",
            "https://cdn.example.com/update.patch",
            &patch_path,
            &work,
        )
        .await
        .unwrap();

        let install = dir.path().join("install");
        copy_tree(&old_root, &install);
        // 用户改坏了资源文件
        write(&install, "resources/a.txt", b"user modified");

        let staging = dir.path().join("staging");
        let result = apply_bundle_patch(&install, &patch_path, &bundle, &staging, &work).await;
        assert!(result.is_err(), "安装目录与清单不一致时必须拒绝应用");
    }
}
