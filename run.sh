#!/usr/bin/env bash
# tovuer — run and build from anywhere:
#   ./run.sh client [args] | install | build | test | smoke | friend <room> [password]
# Releases (what /update installs) are made by GitHub Actions from a tag:
#   git tag v2.1.0 && git push origin v2.1.0
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.local/share/mise/shims:$HOME/.cargo/bin:$HOME/.local/zig:$PATH"

WIN_TARGET="x86_64-pc-windows-gnu"

# Windows build: cross-compiled through zig, no sudo or mingw needed.
build_windows() {  # $1 — build folder (target-dir)
  if ! command -v cargo-zigbuild >/dev/null || ! command -v zig >/dev/null; then
    echo "skipping Windows: zig and cargo-zigbuild are needed (cargo install cargo-zigbuild)" >&2
    return 1
  fi
  rustup target add "$WIN_TARGET" >/dev/null 2>&1 || true
  cargo zigbuild --release -p tovuer --target "$WIN_TARGET" --target-dir "$1"
}

case "${1:-client}" in
  client)
    shift || true
    cargo run --release -p tovuer -- "$@"
    ;;
  install)
    # The tovuer terminal command: build the client and put it into ~/.local/bin.
    cargo build --release -p tovuer
    target/release/tovuer --install
    ;;
  build)
    cargo build --release -p tovuer
    echo "done: target/release/tovuer"
    if build_windows target; then
      echo "done: target/$WIN_TARGET/release/tovuer.exe"
    fi
    ;;
  test)
    shift || true
    cargo test --workspace "$@"
    ;;
  smoke)
    # Two peers on the real internet find each other by room name and chat.
    cargo run --release -p tovuer-node --example smoke
    ;;
  friend)
    # A build for friends: the room (and its password) is baked in, so a
    # friend only runs the file and types a nick.
    ROOM="${2:-}"
    if [ -z "$ROOM" ]; then echo "usage: ./run.sh friend <room> [password]"; exit 1; fi
    export TOVUER_ROOM="$ROOM"
    export TOVUER_ROOM_PASSWORD="${3:-}"
    export TOVUER_FRIEND=1
    export TOVUER_VERSION="${TOVUER_VERSION:-$(date +%Y%m%d-%H%M)}"
    echo "baking in room “$ROOM”${3:+ with a password} (version $TOVUER_VERSION)"
    # a separate build folder: the normal and the friend client do not rebuild each other
    cargo build --release -p tovuer --target-dir target/friend
    cp -f target/friend/release/tovuer tovuer-friend
    if build_windows target/friend; then
      cp -f "target/friend/$WIN_TARGET/release/tovuer.exe" tovuer-friend.exe
      echo "done: ./tovuer-friend and ./tovuer-friend.exe — friends just run it and type a nick"
    else
      echo "done: ./tovuer-friend (no .exe)"
    fi
    ;;
  *)
    echo "usage: ./run.sh [client [args]|install|build|test|smoke|friend <room> [password]]"
    exit 1
    ;;
esac
