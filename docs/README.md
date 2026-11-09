# 文档索引

| 路径 | 用途 |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | 模块边界、主流程、不可破坏的不变量与失败恢复矩阵 |
| [BLOCK_DELTA_THREAT_MODEL.md](BLOCK_DELTA_THREAT_MODEL.md) | TDXB1 容器的信任边界、越界/谎报/翻转攻击的处置与守护测试 |
| [USAGE.md](USAGE.md) | 按场景划分的接入索引、客户端接入、前端结果处理、可取消会话与库 API 分层 |
| [CONFIG_CHEATSHEET.md](CONFIG_CHEATSHEET.md) | 配置决策树、插件字段速查、权限集与发布参数 |
| [PUBLISHING.md](PUBLISHING.md) | `generate` / `cargo delta release` / `doctor` 参数与发布流程 |
| [TROUBLESHOOTING.md](TROUBLESHOOTING.md) | 接入对比、症状排障、错误码与版本迁移 |
| [RELEASING.md](RELEASING.md) | 发布前置条件、门禁、三平台顺序、发布说明规范与失败补救 |
| [SOURCE_MAP.md](SOURCE_MAP.md) | 改什么 → 改哪个文件 → 哪个测试守护 |
| [DEVICE_TESTS.md](DEVICE_TESTS.md) | 发布前 macOS / Windows / Linux 真机验证清单 |
| [releases/](releases/) | 每个版本的发布说明归档 |

根目录的 [VERSIONING.md](../VERSIONING.md) 是版本语义与发布约束的权威；
操作步骤以 `RELEASING.md` 为准，两者冲突时版本判定以 `VERSIONING.md` 为准。
