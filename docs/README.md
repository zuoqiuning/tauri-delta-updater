# 文档索引

| 路径 | 用途 |
|---|---|
| [USAGE.md](USAGE.md) | 客户端接入、前端结果处理、可取消会话与库 API 分层 |
| [PUBLISHING.md](PUBLISHING.md) | `generate` / `cargo delta release` / `doctor` 参数与发布流程 |
| [TROUBLESHOOTING.md](TROUBLESHOOTING.md) | 接入对比、症状排障、错误码与版本迁移 |
| [RELEASING.md](RELEASING.md) | 发布门禁、授权、失败补救与 CI 边界 |
| [RELEASE_STANDARD.md](RELEASE_STANDARD.md) | 所有版本通用的三平台发布命令与发布说明规范 |
| [SOURCE_MAP.md](SOURCE_MAP.md) | 改什么 → 改哪个文件 → 哪个测试守护 |
| [DEVICE_TESTS.md](DEVICE_TESTS.md) | 发布前 macOS / Windows / Linux 真机验证清单 |
| [releases/](releases/) | 每个版本的发布说明归档 |
| [proposals/](proposals/) | 尚未实现功能的设计与威胁模型，不代表已排期 |

根目录的 [VERSIONING.md](../VERSIONING.md) 是版本语义与发布约束的权威；
操作步骤以 `RELEASING.md` 为准，两者冲突时版本判定以 `VERSIONING.md` 为准。
