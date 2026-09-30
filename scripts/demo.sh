#!/usr/bin/env bash
# Runs check, prepare and a dry-run publish against a throwaway repository and
# a local bare origin, to see what relcut logs. Nothing leaves this machine
# except npm's dry-run lookup of the package on the registry.
#   scripts/demo.sh [--log-style plain|github-actions]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cargo build -q --manifest-path "$root/Cargo.toml"
bin=$root/target/debug/relcut

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
git init -q --bare -b main "$dir/origin.git"
git init -q -b main "$dir/work"
cd "$dir/work"
git config user.email demo@example.com
git config user.name demo
git remote add origin "$dir/origin.git"

printf '{"name":"@origin/demo","version":"0.0.0-placeholder","repository":"origin","scripts":{"prepack":"echo building"}}\n' > package.json
printf '{"name":"@origin/demo","lockfileVersion":3,"requires":true,"packages":{"":{"name":"@origin/demo"}}}\n' > package-lock.json
git add .
git commit -qm "feat: first"
git tag v1.0.0
git commit -q --allow-empty -m "fix(api): repair the thing"
git commit -q --allow-empty -m "feat: add more"
git commit -q --allow-empty -m "chore: forgot something"
git commit -q --allow-empty -m "wip stuff"
git push -q --tags origin main

export GITHUB_SERVER_URL=file://$dir GITHUB_REPOSITORY=origin GITHUB_REF_NAME=main RUNNER_TEMP=$dir
export RELCUT_BRANCHES=main RELCUT_PUBLISH=npm,github RELCUT_GITHUB_TOKEN=demo
export npm_config_fetch_retries=0

for command in check prepare "publish --dry-run --yes"; do
  printf '\n===== relcut %s\n' "$command"
  # shellcheck disable=SC2086
  "$bin" $command "${@:---log-style=plain}" || echo "[exit $?]"
done
