#!/bin/bash
# macOS 真机验证：签名、Gatekeeper 与公证状态一次性检查。
#
# 用法：./examples/macos-verify.sh /Applications/MyApp.app
# 退出码：0 = 全部通过；1 = 存在失败项（需人工确认）。
set -u

APP="${1:-}"
if [ -z "$APP" ] || [ ! -d "$APP" ]; then
  echo "用法: $0 /Applications/MyApp.app" >&2
  exit 2
fi

fail=0
check() {
  name="$1"
  shift
  if "$@"; then
    echo "[PASS] $name"
  else
    echo "[FAIL] $name"
    fail=1
  fi
}

check "代码签名完整" codesign --verify --deep --strict --verbose=2 "$APP"
check "Gatekeeper 接受" spctl --assess --verbose=4 "$APP"

# 公证票据：未公证的开发者签名仍然能通过 codesign，但 Gatekeeper 分发会拦截。
if xcrun stapler validate "$APP" >/dev/null 2>&1; then
  echo "[PASS] 公证票据有效"
else
  echo "[WARN] 未公证（仅开发者签名）；首次下载可能被 Gatekeeper 拦截"
fi

# quarantine 属性：从浏览器下载的包会带它；更新流程产物不应残留。
if xattr -l "$APP" 2>/dev/null | grep -q 'com.apple.quarantine'; then
  echo "[WARN] 存在 com.apple.quarantine：请确认首次启动体验"
else
  echo "[PASS] 无 quarantine 属性"
fi

echo
echo "还需人工完成 docs/DEVICE_TESTS.md 的更新与回滚项。"
exit "$fail"
