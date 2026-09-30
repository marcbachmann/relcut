#!/usr/bin/env bash
# Installs the relcut binary for this runner from the GitHub release that
# matches the action's own version, checked against the release's SHA256SUMS,
# into the runner's tool cache. Needs bash, curl and git, nothing else.
set -euo pipefail

fail() {
  echo "::error::relcut setup: $*"
  exit 1
}

# The action's repository and ref: from the context, else from where the
# runner unpacked it, _actions/<owner>/<repo>/<ref>.
repository=${RELCUT_REPOSITORY:-}
ref=${RELCUT_REF:-}
if [[ -z $repository || -z $ref ]] && [[ ${GITHUB_ACTION_PATH:-} =~ /_actions/([^/]+)/([^/]+)/([^/]+) ]]; then
  repository=${repository:-${BASH_REMATCH[1]}/${BASH_REMATCH[2]}}
  ref=${ref:-${BASH_REMATCH[3]}}
fi
[[ -n $repository ]] || fail "cannot tell which repository the action came from, set version"
server=${GITHUB_SERVER_URL:-https://github.com}
git_url=${RELCUT_GIT_URL:-$server/$repository.git}
releases=${RELCUT_RELEASES_URL:-$server/$repository/releases/download}

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  *) fail "no relcut build for $(uname -s) $(uname -m); there are Linux and macOS builds for x64 and arm64" ;;
esac

# The version: asked for, the tag the action is pinned to, or the release tag
# on the commit it is pinned to.
version=${RELCUT_VERSION_WANTED:-}
version=${version#v}
semver='^v?[0-9]+\.[0-9]+\.[0-9]+$'
if [[ -z $version && $ref =~ $semver ]]; then
  version=${ref#v}
fi
if [[ -z $version ]]; then
  [[ -n $ref ]] || fail "the action's ref is unknown, set version"
  remote=$(git ls-remote "$git_url") || fail "git ls-remote $git_url failed"
  sha=$ref
  if [[ ! $ref =~ ^[0-9a-f]{40}$ ]]; then
    sha=$(awk -v ref="$ref" '$2 == "refs/heads/" ref || $2 == "refs/tags/" ref || $2 == "refs/tags/" ref "^{}" {print $1}' <<<"$remote" | tail -1)
    [[ -n $sha ]] || fail "$ref is no branch or tag of $repository"
  fi
  version=$(awk -v sha="$sha" '$1 == sha && $2 ~ /^refs\/tags\/v[0-9]+\.[0-9]+\.[0-9]+(\^\{\})?$/ {sub(/^refs\/tags\/v/, "", $2); sub(/\^\{\}$/, "", $2); print $2}' <<<"$remote" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)
  [[ -n $version ]] || fail "no release tag on $ref of $repository: pin the action to a release, or set version"
fi

dir=${RUNNER_TOOL_CACHE:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}}/relcut/$version/$target
bin=$dir/relcut
url=$releases/v$version
sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# SHA256SUMS comes from the release on every run, and a binary already in
# the tool cache is checked against it: anything that ran earlier in the job,
# a package script of prepare among them, could have written both.
mkdir -p "$dir"
rm -f "$dir/SHA256SUMS" "$dir/download"
curl -fsSL --retry 3 -o "$dir/SHA256SUMS" "$url/SHA256SUMS" || fail "downloading $url/SHA256SUMS failed"
expected=$(awk -v file="relcut-$target" '$2 == file || $2 == "*" file {print $1}' "$dir/SHA256SUMS")
[[ -n $expected ]] || fail "the SHA256SUMS of v$version have no relcut-$target"
if [[ ! -f $bin || -L $bin || $(sha256 "$bin") != "$expected" ]]; then
  curl -fsSL --retry 3 -o "$dir/download" "$url/relcut-$target" || fail "downloading $url/relcut-$target failed"
  [[ $(sha256 "$dir/download") == "$expected" ]] || fail "relcut-$target of v$version does not match its SHA256SUMS"
  chmod +x "$dir/download"
  mv -f "$dir/download" "$bin"
fi

echo "relcut $version ($target) at $bin"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
  echo "path=$bin" >>"$GITHUB_OUTPUT"
  echo "version=$version" >>"$GITHUB_OUTPUT"
fi
if [[ ${RELCUT_ADD_TO_PATH:-} == true && -n ${GITHUB_PATH:-} ]]; then
  echo "$dir" >>"$GITHUB_PATH"
fi
