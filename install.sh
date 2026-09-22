#!/bin/sh
# Build and install envpick, then say how to wire it into the shell.
#
# Plain POSIX sh, and deliberately small: the interesting logic lives in the
# Rust code, and a fat installer is just one more thing to go wrong on someone
# else's machine.

set -eu

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

cd "$(dirname "$0")"

command -v cargo >/dev/null 2>&1 \
  || die "找不到 cargo。先装 Rust: https://rustup.rs"

say "编译 envpick（release）..."
cargo build --release || die "编译失败"

BIN=target/release/envpick
[ -x "$BIN" ] || die "编译完成但找不到 $BIN"

# Prefer a directory that is already on PATH, so the shell integration finds the
# binary immediately. ~/.cargo/bin is where rustup puts things and is on PATH
# for anyone who installed Rust the normal way.
if [ -w "${CARGO_HOME:-$HOME/.cargo}/bin" ]; then
  DEST="${CARGO_HOME:-$HOME/.cargo}/bin"
elif [ -w /usr/local/bin ]; then
  DEST=/usr/local/bin
else
  DEST="$HOME/.local/bin"
fi

mkdir -p "$DEST"
install -m 0755 "$BIN" "$DEST/envpick" 2>/dev/null \
  || { cp "$BIN" "$DEST/envpick" && chmod 0755 "$DEST/envpick"; }

say "已安装到 $DEST/envpick"
"$DEST/envpick" --version || true

case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    say ""
    say "注意：$DEST 不在 PATH 里，先把这行加进 shell 配置："
    say "  export PATH=\"$DEST:\$PATH\""
    ;;
esac

say ""
say "下一步——把 shell 集成加进 rc 文件，否则 ep use 无法修改当前 shell 的环境："
say ""
say "  zsh:   echo 'eval \"\$(envpick init zsh)\"'  >> ~/.zshrc"
say "  bash:  echo 'eval \"\$(envpick init bash)\"' >> ~/.bashrc"
say ""
say "然后重开一个终端，或者直接 eval 一次上面那行，就能用 ep 了："
say ""
say "  ep new work"
say "  ep set work EDITOR=nvim"
say "  ep use work"
say "  ep ui"
say ""
say "跨机同步（端到端加密，服务端只看到密文）："
say ""
say "   ep sync genid                       # 生成同步 ID"
say "   ep sync init <ID> --key-stdin       # 输入密钥（不留在 shell 历史里）"
say "   ep sync push"
