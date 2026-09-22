#!/usr/bin/env bash
#
# envpick 安装脚本（两种用法，同一个文件）
#
#   1) 仓库里执行：        ./install.sh
#      编译并安装到 ~/.local/bin（开发安装）
#
#   2) 一行命令装最新版：  curl -fsSL <raw 地址>/install.sh | bash
#      脚本旁边没有源码时自动切到下载模式：从 GitHub release 拉预编译二进制。
#      也支持钉住版本：    curl -fsSL ... | bash -s -- v1.2.3
#
#   PREFIX=/usr/local/bin ./install.sh   安装到指定目录（两种模式都支持）
#
# 环境变量（fork / 测试用）：ENVPICK_GH_BASE、ENVPICK_REPO
#
# 注意：脚本只装二进制，不改你的 rc 文件。shell 集成需要手动加一行 ——
# 安装结束后会把那一行原样打出来。
#
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local/bin}"
GH_BASE="${ENVPICK_GH_BASE:-https://github.com}"
REPO="${ENVPICK_REPO:-prnake/envpick}"
PIN="${1:-${VERSION:-}}"     # 位置参数或 VERSION 环境变量：钉住某个 tag

info() { printf '· %s\n' "$*"; }
ok()   { printf '✓ %s\n' "$*"; }
bad()  { printf '✗ %s\n' "$*" >&2; }

# 下载模式需要 curl；本地模式不需要，所以只在真的要用时才要求。
need_curl() {
  command -v curl >/dev/null 2>&1 || { bad "缺少 curl（下载模式必需）"; exit 1; }
}

# 当前平台对应的 release 资产名，必须和 .github/workflows/release.yml 一致。
asset_name() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}-${arch}" in
    Darwin-arm64)  printf 'envpick-aarch64-apple-darwin\n' ;;
    Darwin-x86_64) printf 'envpick-x86_64-apple-darwin\n' ;;
    Linux-x86_64)  printf 'envpick-x86_64-unknown-linux-gnu\n' ;;
    Linux-aarch64) printf 'envpick-aarch64-unknown-linux-gnu\n' ;;
    *)
      bad "没有为 ${os}/${arch} 发布二进制"
      info "请从源码安装：git clone https://${GH_BASE#https://}/${REPO} && cd envpick && ./install.sh"
      info "（需要 Rust 1.85+：https://rustup.rs）"
      exit 1
      ;;
  esac
}

# 校验 SHA256。校验和是必须的，不是可选的 —— 装进去的东西每次开 shell 都会跑，
# 而它是从网络上下载来的。缺 SHA256SUMS 时宁可失败，也不装一个没校验过的二进制。
verify() {
  # $1 = 目录, $2 = 资产名
  local dir="$1" asset="$2"
  [ -s "${dir}/SHA256SUMS" ] || { bad "release 没有带 SHA256SUMS，拒绝安装未校验的二进制"; exit 1; }
  if command -v shasum >/dev/null 2>&1; then
    (cd "$dir" && grep -E "  ${asset}\$" SHA256SUMS | shasum -a 256 -c -) \
      || { bad "SHA256 校验失败"; exit 1; }
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$dir" && grep -E "  ${asset}\$" SHA256SUMS | sha256sum -c -) \
      || { bad "SHA256 校验失败"; exit 1; }
  else
    bad "系统里没有 shasum / sha256sum，无法校验下载内容"
    exit 1
  fi
  ok "SHA256 校验通过"
}

# curl | bash 时脚本在 /dev/fd/63 这类位置，cd 会失败或找不到兄弟文件 ——
# 这正是判断「本地模式 / 下载模式」的依据。
SRC="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"

BIN=""
if [ -n "$SRC" ] && [ -f "$SRC/Cargo.toml" ] && [ -f "$SRC/src/main.rs" ]; then
  # ---------------- 本地模式：编译 ----------------
  info "本地安装：从 ${SRC} 编译"
  command -v cargo >/dev/null 2>&1 \
    || { bad "找不到 cargo。先装 Rust: https://rustup.rs"; exit 1; }
  (cd "$SRC" && cargo build --release) || { bad "编译失败"; exit 1; }
  BIN="$SRC/target/release/envpick"
  [ -x "$BIN" ] || { bad "编译完成但找不到 $BIN"; exit 1; }
else
  # ---------------- 下载模式（curl | bash）----------------
  need_curl
  ASSET="$(asset_name)"
  info "在线安装：从 ${GH_BASE}/${REPO} 拉取 ${ASSET}"

  if [ -n "$PIN" ]; then
    tag="$PIN"
  else
    # 不用 GitHub API（免 token / 免限流）：releases/latest 会 302 到 /tag/vX.Y.Z
    tag="$(curl -fsSL --connect-timeout 5 --max-time 15 -o /dev/null -w '%{url_effective}' \
      "${GH_BASE}/${REPO}/releases/latest" | sed -n 's|.*/tag/\(.*\)$|\1|p')"
    if [ -z "$tag" ]; then
      bad "查不到最新版本（网络不通？或 ${REPO} 还没有发布过 release）"
      exit 1
    fi
  fi
  ok "目标版本：${tag}"

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/envpick-install.XXXXXX")"
  # 下载失败也要把临时目录清掉，别往 /tmp 里漏垃圾
  trap 'rm -rf "$tmp"' EXIT
  for f in "$ASSET" SHA256SUMS; do
    info "下载 ${f}…"
    curl -fsSL --connect-timeout 5 --max-time 120 -o "${tmp}/${f}" \
      "${GH_BASE}/${REPO}/releases/download/${tag}/${f}" \
      || { bad "下载 ${f} 失败（确认 release ${tag} 里有这个资产）"; exit 1; }
  done
  verify "$tmp" "$ASSET"
  BIN="${tmp}/${ASSET}"
  # 二进制是从网络来的，装上之前先确认它真的能跑 —— 校验和证明它是发布的那一份，
  # 跑一下才证明它是能用的那一份（架构不匹配会在这里暴露，而不是在你的 rc 文件里）。
  chmod 0755 "$BIN"
  "$BIN" --version >/dev/null 2>&1 \
    || { bad "下载的二进制无法运行（架构不匹配？）"; exit 1; }
fi

# ---------------- 两种模式汇合：装文件 ----------------
mkdir -p "$PREFIX"
install -m 0755 "$BIN" "$PREFIX/envpick" 2>/dev/null \
  || { cp "$BIN" "$PREFIX/envpick" && chmod 0755 "$PREFIX/envpick"; }
ok "已安装 $PREFIX/envpick"
"$PREFIX/envpick" --version || true

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *)
    info ""
    info "注意：$PREFIX 不在 PATH 里，先把这行加进 shell 配置："
    printf '\n    export PATH="%s:$PATH"\n\n' "$PREFIX"
    ;;
esac

# 刻意不自动改 rc 文件：改别人的 dotfile 是不可逆的，而这一行本来就该由用户
# 自己决定放哪儿（很多人把这类东西收在单独的文件里 source）。
info ""
info "下一步——把 shell 集成加进 rc 文件，否则 ep use 无法修改当前 shell 的环境："
printf '\n'
printf '    zsh:   echo '"'"'eval "$(envpick init zsh)"'"'"'  >> ~/.zshrc\n'
printf '    bash:  echo '"'"'eval "$(envpick init bash)"'"'"' >> ~/.bashrc\n'
printf '\n'
info "然后重开终端，或者直接 eval 一次上面那行，就能用 ep 了："
printf '\n'
printf '    ep new work\n'
printf '    ep set work EDITOR=nvim\n'
printf '    ep use work\n'
printf '    ep ui\n'
printf '\n'
info "跨机同步（端到端加密，服务端只看到密文）："
printf '\n'
printf '    ep sync genid                   生成同步 ID\n'
printf '    ep sync init <ID> --key-stdin   输入密钥（不留在 shell 历史里）\n'
printf '    ep sync push\n'
printf '\n'
info "以后升级用：envpick update"
