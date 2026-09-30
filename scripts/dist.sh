#!/usr/bin/env bash
# Builds relcut for macOS and static Linux with cargo-zigbuild, into dist/
# with a SHA256SUMS file. Needs zig and cargo-zigbuild.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
targets=(aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl)
cargo zigbuild --release "${targets[@]/#/--target=}"

rm -rf dist
mkdir dist
for target in "${targets[@]}"; do
  cp "target/$target/release/relcut" "dist/relcut-$target"
done
(cd dist && shasum -a 256 relcut-* > SHA256SUMS)
ls -l dist
