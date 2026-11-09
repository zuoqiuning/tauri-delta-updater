# TDXB1 威胁模型

> 适用范围：`src/core/block_delta.rs` 的 TDXB1 容器（生成端、解析端）。
> 相关：补丁完整性由清单签名与双端 SHA-256 保证，见
> [`SECURITY.md`](../SECURITY.md)；补丁容器种类见
> [`ARCHITECTURE.md`](ARCHITECTURE.md) §7.1。

## 1. 信任边界

TDXB1 是**确定性字节重排器**：它把旧文件的内容按下标重排、拼接字面量，
产出一个新文件。它不做压缩算法协商，也不携带任何"可信元数据"。

- 补丁字节在进入解析器之前，必须已经通过外层 `patch_hash` 校验，
  而该哈希写在**已签名**的清单里；
- 产物写出后，外层 `apply_patch` 会再校验 `target_hash`；
- 因此攻击者要构造"能通过校验的恶意补丁"，等价于拥有签名私钥，
  这不在本模型的防护范围内。

本模型防护的是：**容器的畸形输入不能让进程越界读写、无限分配、静默产出
错误内容或 panic**。即使补丁来自签名清单，发布端也可能生成错误字节。

## 2. 攻击者能力

假设攻击者可以控制补丁文件的任意字节（例如 CDN 被入侵但私钥未泄露时，
攻击者无法通过 `patch_hash`；这里仍然按"解析器必须自证安全"设计）：

- 任意修改头部、指令、字面量流；
- 截断文件或追加垃圾；
- 构造自洽但语义恶意的大小字段。

不在范围内：

- 拥有签名私钥的攻击者；
- 已经能改写旧文件或工作目录的本地攻击者（见 `SECURITY.md` 的 out of scope）。

## 3. 威胁与处置

| 威胁 | 处置 | 守护测试 |
|---|---|---|
| `COPY` 的 `offset + len` 越界，读到旧文件之外 | `parse_ops` 在**写盘前**逐条校验 `offset + len <= old_len`，不依赖生成端索引可信 | `rejects_out_of_bounds_copy` |
| `len` 为 `u32::MAX` 造成巨大切片 | 同上；越界立即 `PatchApplyFailed` | `rejects_out_of_bounds_copy` |
| `PATCH` 的旧区间越界 | `parse_ops` 校验 `old_offset + old_len <= old_len` | `rejects_out_of_bounds_gap_patch` |
| `PATCH` 的 `patch_len` 谎报，触发 GB 级 `Vec` 预分配 | `patch_len <= 4 MiB + 64 KiB`，超出直接拒绝 | `rejects_out_of_bounds_gap_patch`（字段级） |
| 嵌套 bsdiff 解析恶意补丁 | `qbsdiff::Bspatch::new` 先做结构校验；失败返回 `PatchApplyFailed`，不 panic | `rejects_corrupted_patch` |
| `LITERAL` 长度谎报，写出 4 GiB 产物 | 指令累计输出必须**精确等于**头部 `new_len`；`new_len <= 2 × old_len + 128 MiB`（与 zstd 补丁同口径） | `rejects_huge_literal_before_allocating` |
| `ops_len` 谎报，预分配 64 MiB | `ops_len <= 64 MiB` 且 `ops_len + 头长 <= 补丁文件长度`；不足时 `read_exact` 失败 | `rejects_ops_len_beyond_file` |
| 指令流截断 | 每个字段用 `bytes.get(..)` 检查，缺失即报错 | `rejects_corrupted_patch` |
| 未知指令 / 未知版本 / 错误魔数 | 直接拒绝；魔数不符时不进入 TDXB1 分支 | `rejects_corrupted_patch`、`apply_patch_dispatches_to_block_delta` |
| 旧文件与补丁不匹配（同版本号但内容不同） | 头部 `old_len` 必须等于旧文件实际大小；最终由 `target_hash` 兜底 | `rejects_corrupted_patch` |
| 任意字节翻转后静默重建出错误内容 | 容器**没有仅供信息、不参与校验的字段**（头部只含魔数/版本/三个长度，指令字段全部被消费）；翻转必然导致解析失败或产物不同 | `any_byte_flip_is_detected` |
| 生成端哈希碰撞导致 `COPY` 指向错误内容 | 命中哈希后仍逐字节 `memcmp`，不一致按缺口处理 | `property_roundtrips_random_edits` |
| 磁盘被写满 | 写盘前 `check_disk_space(new_len)` | 由 `core::disk` 单测覆盖 |

## 4. 资源上界

| 项 | 上界 |
|---|---|
| 分块索引（生成端） | `old_size / 8 KiB × 40 B`，2 GB 约 10 MB |
| 单次缺口 bsdiff | 输入 ≤ 4 MiB，约 68 MiB 工作集 |
| 指令区 | ≤ 64 MiB；`Vec<Op>` ≤ 约 500 万条 |
| 解析期额外缓冲 | 单个 `patch_bytes` ≤ 4 MiB + 64 KiB；字面量缓冲 64 KiB |
| 产物 | `new_len ≤ 2 × old_len + 128 MiB`，且受磁盘空间检查约束 |
| 生成/应用的并发 | 由 `core::diff` 的内存预算加权限流，见 `configure_patch_concurrency` |

## 5. 兼容性与发布约束

- TDXB1 只有 **0.5.0 起**的客户端能解析；
- 发布端默认不产出它，必须显式开启 `--large-file-delta`；
- 它与 `--legacy-patch` **互斥**：后者面向只认 `BSDIFF40` 的旧客户端，
  混用会让所有旧客户端下载到无法应用的补丁，CLI 与库都会直接报错；
- 清单的 `patches[].algorithm` 会写入 `tdxb1`。客户端遇到不认识的算法值
  会**跳过该补丁**（回退全量），而不是下载后失败；
- 因此 TDXB1 的启用前提是：所有客户端版本都 ≥ 0.5.0，或确认这些基线
  不会被旧客户端命中。

## 6. 复核清单

修改 `block_delta.rs` 时至少重跑：

```bash
cargo test --lib --all-features block_delta
cargo test --test generate_cli --all-features
```

并确认：

- [ ] 新增指令字段都在 `parse_ops` 中有越界与总量校验；
- [ ] 头部没有引入"仅作信息"的字段；
- [ ] 任一新增分支失败时返回 `DeltaError`，不使用 `unwrap` / 索引切片；
- [ ] 生成端对哈希命中仍做逐字节比较；
- [ ] `--legacy-patch` 与 `--large-file-delta` 的互斥没有被放宽。
