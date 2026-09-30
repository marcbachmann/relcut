#!/usr/bin/env bash
# Installs the release job's build tools from their prebuilt Linux x86_64
# downloads, each checked against a pinned SHA-256, into $1 (or
# $RUNNER_TEMP/tools) and puts them on GITHUB_PATH when there is one.
set -euo pipefail

dir=${1:-${RUNNER_TEMP:?}/tools}
mkdir -p "$dir/bin"

fetch() {
  local name=$1 url=$2 sha256=$3 file
  file="$dir/$(basename "$url")"
  curl -fsSL --retry 3 -o "$file" "$url"
  echo "$sha256  $file" | sha256sum -c --quiet
  mkdir -p "$dir/$name"
  tar -xJf "$file" -C "$dir/$name"
  rm "$file"
}

fetch zig \
  https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz \
  70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
fetch cargo-zigbuild \
  https://github.com/rust-cross/cargo-zigbuild/releases/download/v0.23.4/cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz \
  9e3cf73485edbd45905c8aadbc0fdf869c7ddc3848f0c898229f2680db52e44b
fetch cargo-cyclonedx \
  https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/download/cargo-cyclonedx-0.5.9/cargo-cyclonedx-x86_64-unknown-linux-musl.tar.xz \
  9bd3e599314f50810c9d98b8b68a617ff9d3cc20873968d90b29d121f6b226ff

# zig needs its lib/ beside it, so it stays where it was unpacked.
ln -sf "$(find "$dir/zig" -maxdepth 2 -type f -name zig)" "$dir/bin/zig"
for tool in cargo-zigbuild cargo-cyclonedx; do
  ln -sf "$(find "$dir/$tool" -type f -name "$tool")" "$dir/bin/$tool"
done

if [[ -n ${GITHUB_PATH:-} ]]; then
  echo "$dir/bin" >> "$GITHUB_PATH"
fi
"$dir/bin/zig" version
"$dir/bin/cargo-zigbuild" --help >/dev/null
"$dir/bin/cargo-cyclonedx" cyclonedx --version
