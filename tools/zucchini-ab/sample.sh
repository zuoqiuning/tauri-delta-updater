#!/usr/bin/env bash
# 生成三份真实 ELF 构建：仅版本字符串变化、一个模块重编译。
# CI 与本地都可运行；产物只用于 A/B，不进入发布包。
set -euo pipefail

cd "$(dirname "$0")"
mkdir -p samples

cat > samples/base.rs <<'RS'
const VERSION: &str = "0.1.0";
fn core(x: u64) -> u64 { x.wrapping_mul(37) }
fn main() { println!("{VERSION} {}", core(42)); }
RS

cat > samples/version.rs <<'RS'
const VERSION: &str = "0.1.1";
fn core(x: u64) -> u64 { x.wrapping_mul(37) }
fn main() { println!("{VERSION} {}", core(42)); }
RS

cat > samples/module.rs <<'RS'
const VERSION: &str = "0.1.0";
fn core(x: u64) -> u64 { x.wrapping_mul(41).wrapping_add(7) }
fn main() { println!("{VERSION} {}", core(42)); }
RS

for variant in base version module; do
  rustc -O "samples/${variant}.rs" -o "samples/${variant}"
done

echo "samples: $(pwd)/samples"
