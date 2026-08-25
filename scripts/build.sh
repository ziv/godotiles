#!/usr/bin/env sh
# Build the godotiles GDExtension and copy it into addons/godotiles/bin/<platform>/.
#
#   scripts/build.sh [debug|release]      (default: release)
#
# macOS: builds a universal (arm64 + x86_64) dylib when both Rust targets are installed
# (`rustup target add aarch64-apple-darwin x86_64-apple-darwin`), otherwise the host arch only.
set -eu
cd "$(dirname "$0")/.."

PROFILE="${1:-release}"
case "$PROFILE" in
  debug)   CARGO_FLAGS="" ; DIR=debug ;;
  release) CARGO_FLAGS="--release" ; DIR=release ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

OS="$(uname -s)"
ARCH="$(uname -m)"
BIN=addons/godotiles/bin

case "$OS" in
  Darwin)
    OUT="$BIN/macos"; mkdir -p "$OUT"
    if rustup target list --installed 2>/dev/null | grep -q aarch64-apple-darwin && \
       rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then
      cargo build --manifest-path rust/Cargo.toml $CARGO_FLAGS --target aarch64-apple-darwin
      cargo build --manifest-path rust/Cargo.toml $CARGO_FLAGS --target x86_64-apple-darwin
      lipo -create -output "$OUT/libgodotiles.dylib" \
        "rust/target/aarch64-apple-darwin/$DIR/libgodotiles.dylib" \
        "rust/target/x86_64-apple-darwin/$DIR/libgodotiles.dylib"
    else
      cargo build --manifest-path rust/Cargo.toml $CARGO_FLAGS
      cp "rust/target/$DIR/libgodotiles.dylib" "$OUT/libgodotiles.dylib"
    fi
    ;;
  Linux)
    case "$ARCH" in
      x86_64) OUT="$BIN/linux-x86_64" ;;
      aarch64|arm64) OUT="$BIN/linux-arm64" ;;
      *) echo "unsupported Linux arch $ARCH" >&2; exit 1 ;;
    esac
    mkdir -p "$OUT"
    cargo build --manifest-path rust/Cargo.toml $CARGO_FLAGS
    cp "rust/target/$DIR/libgodotiles.so" "$OUT/libgodotiles.so"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    OUT="$BIN/windows-x86_64"; mkdir -p "$OUT"
    cargo build --manifest-path rust/Cargo.toml $CARGO_FLAGS
    cp "rust/target/$DIR/godotiles.dll" "$OUT/godotiles.dll"
    ;;
  *) echo "unsupported OS $OS" >&2; exit 1 ;;
esac

echo "built $PROFILE -> $OUT"
