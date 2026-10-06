<div align="center">

# relcut

**Checked in review. Released on merge. Tokens out of reach.**
Releases from conventional commits · one static binary · no config files, no plugins · built for GitHub Actions

[Website](https://marcbachmann.github.io/relcut/) · [Quick start](#quick-start) · [Scenarios](#scenarios) · [Safeguards](#what-relcut-guards-against) · [Reference](#reference)

</div>

---

relcut reads the conventional commits since your last version tag and works out the next
version. Every pull request shows what merging it will release, and fails on a commit that
is not conventional or a version outside the target branch's rules. On merge relcut
releases it: a git tag, an npm package on npmjs and GitHub Packages, and a GitHub release
with notes, assets and comments on the pull requests it contains. No dependency, script or
git hook is handed a token, or finds one in relcut's environment or git config.

```text
── Environment ─────────────────────────────────
   relcut  v1.0.0 · /opt/hostedtoolcache/relcut/1.0.0/x86_64-unknown-linux-musl/relcut
   git     v2.51.0 · /usr/bin/git
   node    v24.11.1 · /opt/hostedtoolcache/node/24.11.1/x64/bin/node
   npm     v11.21.0 · /opt/hostedtoolcache/node/24.11.1/x64/lib/node_modules/npm
   user    runner · uid 501

── Commits ─────────────────────────────────────
   2 conventional commits
   main is a release branch
▸ 2 commits since v1.0.0
   v1.1.0 · 2 commits since v1.0.0 · 0.0s

── Prepare ─────────────────────────────────────
   No stored git credentials · 0.0s
   No install scripts to run · 0.0s
   Installed dependencies · from package-lock.json · 4.2s
   Packed @acme/widgets@1.1.0 · relcut/acme-widgets-1.1.0.tgz · 18.2 KB · 1.8s
▸ Install dependencies
▸ Pack the tarball
   Prepared v1.1.0 · 4 steps · 6.4s

── Publish ─────────────────────────────────────
   Ready to tag v1.1.0 · 6 checks passed · 0.1s
   Tagged v1.1.0 · 53de8e0 · 0.4s
   Published to registry.npmjs.org · @acme/widgets@1.1.0 on latest · trusted publishing · 2.1s
   GitHub release · https://github.com/acme/widgets/releases/tag/v1.1.0 · 0.6s
▸ Publish to registry.npmjs.org

────────────────────────────────────────────────────────────────

   Released v1.1.0 · npm, github · 12.6s

   Release     https://github.com/acme/widgets/releases/tag/v1.1.0
   npm         https://www.npmjs.com/package/@acme/widgets/v/1.1.0
   Registry    registry.npmjs.org
   Dist-tags   latest

────────────────────────────────────────────────────────────────
```

## Is relcut for you?

relcut is not an all-round tool. It does one way of releasing, and does it well. Either
it fits you or it does not.

**It fits when**

- your history uses [conventional commits](https://www.conventionalcommits.org): `feat:`,
  `fix:`, `BREAKING CHANGE:`
- versions live in git tags (`v1.4.2`), not in a committed `package.json` or `Cargo.toml`
- you release from GitHub Actions to npm, GitHub Packages and GitHub releases
- you want release branches that take backports for older versions
- you would rather have a release step that cannot run anybody's code with your tokens

**It does not fit when**

- you need monorepos with many packages versioned apart
- you need prereleases (`1.0.0-beta.1`)
- your release runs somewhere other than GitHub

There are no plugins: what relcut does not do is a step of your workflow. Another registry
takes the tarball between `prepare` and `publish`; a `CHANGELOG.md` commit or an
announcement comes after `publish`, with the outputs `version`, `tag` and `notes`, or in a
workflow on the tag's push or on `release: published`.

## Quick start

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    branches: [main]

jobs:
  release:
    runs-on: ubuntu-latest
    permissions:
      contents: write       # the tag and the GitHub release
      pull-requests: write  # comments on the released pull requests
      id-token: write       # npm trusted publishing
    steps:
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0             # relcut reads every tag and commit
          filter: tree:0             # as commits only; files for HEAD alone
          persist-credentials: false # relcut refuses tokens left in .git/config
      - uses: actions/setup-node@v5
        with:
          node-version: 24
      - uses: marcbachmann/relcut@v1.0.0
        with:
          branches: main
          publish: npm
```

`filter: tree:0` keeps that cheap: every commit and tag, but files only for the commit
checked out. The history of 30,000 commits is about 7 MB, its full clone 86 MB. A shallow checkout,
`actions/checkout`'s default, gets a warning, and relcut fetches the same history into it
with `github-token` before any script runs. A `publish` after `prepare` never fetches.

That is the whole setup: no `.releaserc`, no plugins, nothing to install. A push to `main`
with a `fix:` releases a patch, a `feat:` a minor version, a `BREAKING CHANGE:` a major one.
Any other commit (`chore:`, `docs:`, `test:` …) releases nothing.

> Pin the action to a release tag or a commit SHA. The action installs exactly the relcut
> release it is pinned to.

## How it works

relcut is three steps, and `relcut release` runs all of them:

| Step | What it does | Credentials |
| --- | --- | --- |
| `check` | Reads the commits since the last version tag, works out the next version and its release notes. Changes nothing. | none, at most `github-token` to fetch the history into a shallow checkout |
| `prepare` | `npm ci --ignore-scripts`, `npm rebuild` for dependencies with install scripts, sets the version and a `ci` block (repository, directory, date, commit, buildUrl, branch, tag) in `package.json`, `npm pack`. | none, at most a read token for `npm ci` |
| `publish` | Tags and pushes first, then publishes the packed tarball, creates the GitHub release, uploads the assets, comments on the pull requests. | the ones it needs, and only there |

The tag is the point of no return, so `publish` checks what it can before it: the assets,
the tarball, the npm dist-tags. When a release still fails after its tag, run it again on
the same commit: relcut takes the tag as its own, keeps what is already published and does
the rest.

Run `release` when nothing needs to happen in between. Run the steps apart when you build
something between `prepare` and `publish`, or want `check` on every pull request.

```yaml
- uses: marcbachmann/relcut@v1.0.0
  id: check
  with:
    command: check
    branches: main
- run: npm run build:docs # needs the version, has no tokens
  if: steps.check.outputs.release == 'true'
- uses: marcbachmann/relcut@v1.0.0
  if: steps.check.outputs.release == 'true'
  with:
    command: publish
    branches: main
    github-assets: dist/docs.zip
```

## Scenarios

### 1 · An npm package with trusted publishing

No npm token in your secrets: npm exchanges the job's OIDC token for a short-lived one.
Configure the workflow as the package's trusted publisher on npmjs.com once.

```yaml
permissions:
  contents: write
  pull-requests: write
  id-token: write
steps:
  - uses: actions/checkout@v5
    with: { fetch-depth: 0, filter: tree:0, persist-credentials: false }
  - uses: actions/setup-node@v5
    with: { node-version: 24 }
  - uses: marcbachmann/relcut@v1.0.0
    with:
      branches: main
      publish: npm
```

Trusted publishing needs npm 11.5.1 or newer, and `repository.url` in `package.json`
naming the repository: npm's provenance checks it, so relcut checks it before the tag. More than one dist-tag through trusted
publishing needs npm 11.21.0 or 12.2.0; with older versions pass `npm-token`.

With `npm-stage: true` relcut runs `npm stage publish`: the version waits in the package's
stage queue on npmjs and is installable once a maintainer approves it with 2FA, on npmjs.com
or with `npm stage approve <id>`. A trusted publisher can then be limited to staging, so
the workflow alone publishes nothing. It needs npm 11.15.0 or newer. The approval sets the one
dist-tag the version was staged with, `latest` by default, and no dist-tag moves before
it; a second `npm-tag` is refused before the tag, because `npm dist-tag` cannot name a
version that is not on the registry yet. The git tag and the GitHub release are made right
away. Rejecting a staged version frees its number on npm, but the git tag stays: run the
release again on the same commit to stage it once more.

`npm` publishes where `package.json` says, as npm reads it: the scope's registry in
`publishConfig`, else `publishConfig.registry`, else npmjs. For GitHub Packages set it to
`https://npm.pkg.github.com`; relcut publishes there with `github-token` and needs the
package under the owner's scope, `@<owner>/<name>`.

```json
{
  "name": "@acme/widgets",
  "publishConfig": { "registry": "https://npm.pkg.github.com" }
}
```

### 2 · Release branches for backports

This is where relcut shines. `main` moves on, and your users on `1.4` still get fixes,
from a branch that can only release below what `main` released already. No setting
says so: the tags do.

```sh
git switch -c release-1.4 v1.4.2   # cut the branch from the release it fixes
git push -u origin release-1.4
```

```yaml
on:
  push:
    branches: [main, release-*]
jobs:
  release:
    steps:
      - uses: marcbachmann/relcut@v1.0.0
        with:
          branches: main release-*
          publish: npm
```

A branch releases below the lowest version that exists outside its history. With `main`
at `v1.5.0`:

- A `fix:` on `release-1.4` releases `v1.4.3`.
- A `feat:` there would be `v1.5.0` and fails with `v1.5.0 is not below v1.5.0, which
  exists already`, before anything is packed or tagged.
- A release below an existing version is a backport: npm gets the dist-tag `release-1.4`,
  so it never moves `latest` back. Users install it with
  `npm install @acme/widgets@release-1.4`. npm takes no dist-tag that reads as a version
  range: name the branch `release-1.4`, not `v1.4`, or set `npm-tag`.
- Its GitHub release never becomes the repository's latest release.
- A branch without a release of its own fails once other branches released, instead of
  releasing `1.0.0`.
- The repository's default branch is the trunk the others backport to. When
  `release-1.4` releases `v1.4.3` before `main` released past `v1.4.2`, that line is
  the branch's: the next release of `main` is `v1.5.0` at least, never below `v1.4.3`.

With `main` at `v2.0.0` instead, a `feat:` on `release-1.4` releases `v1.5.0`: nothing
exists between. To take fixes only, set `constraint: v1.4` in that branch's workflow.

Or let a commit cut the branch. A `Release-Cut:` footer, or `RELEASE CUT:`, on the first
line of the commit's body names it, and relcut creates it at the parent of that commit, in
the same atomic push as the tag of `main`:

```text
chore(renovate): maintain release-2026-09

Release-Cut: release-2026-09
```

Merging it into `main` at `v1.4.2`:

- `main` releases `v1.5.0`. A cut is a minor version at least, as `BREAKING CHANGE:` is a
  major one, so the fixes of `main` go on as `v1.5.x`.
- `release-2026-09` starts at the commit before, on `v1.4.2`, and releases below `v1.5.0`:
  its first fix is `v1.4.3`, under the dist-tag `release-2026-09`. When it carries a
  feature `main` has not released, that is its own `v1.5.0`, and `main` goes to `v1.6.0`.
- The pull request shows it: `v1.5.0 · minor on main · cuts release-2026-09 at 53de8e0`.
  A footer relcut cannot follow fails the pull request: one without a branch, with one
  outside `branches`, the branch that releases, one that exists already without the
  parent of the commit, or on a backport. On the branch it is a warning and the release
  goes on without the cut. A cut branch only the remote has, which the checkout does not
  show, stops the release before the tag.
- A footer further down the body, below text or among a squash merge's trailers like
  `Co-authored-by:`, is a warning and never followed.
- A cut needs its commit on `main` itself, squashed or rebased: a merge commit brings it
  in with the merged branch's parent, so relcut warns and releases without the cut. Turn
  off merge commits in the repository's settings.
- Give that commit a change of its own: a rebase merge drops an empty commit, footer and
  all, so the pull request shows the cut and the merge makes none.
- The `cut` output names the branch, for a step after relcut, such as one that commits a
  `fix:` to it for its first release.

A cut by hand is a commit as well: a `workflow_dispatch` job commits the footer to `main`
through the API with a GitHub App token, and the push starts the release workflow.

### 3 · Pull requests: what merging will release

`check` on every pull request shows the version the merge will release, right in the
check's title (`v1.4.3 · patch on release-1.4`), and fails the pull request before anyone merges:

- on a commit of the pull request that is not conventional (`wip: stuff`, `Update file`).
  Commits already on the target branch are listed, never reported.
  `conventional-commits: warn` only warns.
- on a version outside the target branch's rules: with `main` at `v1.5.0`, a `feat:` into
  `release-1.4` fails with `v1.5.0 is not below v1.5.0, which exists already`.
- on a `Release-Cut:` or `release:` the pull request asks for and relcut cannot follow.

```yaml
on:
  pull_request:
jobs:
  check:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      checks: write # the check's title shows the version
    steps:
      - uses: actions/checkout@v5
        with: { fetch-depth: 0, filter: tree:0, persist-credentials: false }
      - uses: marcbachmann/relcut@v1.0.0
        with:
          command: check
          branches: main release-*
          check-run-id: ${{ job.check_run_id }}
```

Without `check-run-id` the result still lands in the job summary and the outputs.

### 4 · Binaries on GitHub releases

relcut releases itself this way: four binaries and a checksum file, attached to the
GitHub release, with a downloads table in the notes.

```yaml
- uses: marcbachmann/relcut@v1.0.0
  with:
    branches: main
    github-assets: |
      dist/app-x86_64-linux
      dist/app-aarch64-darwin
      dist/SHA256SUMS
```

Assets take metadata as JSON when the file name is not what people should see:

```yaml
    github-assets: >
      [{"file": "dist/app-x86_64-linux", "name": "app-linux-x64",
        "label": "Linux x64 (static)", "content_type": "application/octet-stream"}]
```

`github-artifacts: docs-site` attaches artifacts an earlier job of the same run uploaded
(needs `actions: read`). The log lists every asset with its size and content type. The
release notes list none unless `release-notes-template` has `{assets}`, which puts in a
table with every asset, its size and its SHA-256. A release with assets is a draft until
they are uploaded, so nobody sees it without them.

### 5 · Release on request

Release when you ask for it, not on every merge. With `releases: explicit` the commits
wait until one asks for a release: a `release:` commit, or a `Release:` footer on the first
line of the commit's body.

```yaml
- uses: marcbachmann/relcut@v1.0.0
  with:
    branches: main
    releases: explicit
    publish: npm
```

- Until then `check` shows what waits: `v1.5.0 · minor on next release`.
- `release: ship the update` releases what the commits since the last release make, a
  patch at least. A `Release-Cut:` footer asks for its release too.
- A release by hand is a commit as well: a `workflow_dispatch` job commits `release: …` to
  `main` through the API with a GitHub App token.
- With `releases: auto`, the default, every push releases, and a release request fails the
  pull request; on the branch it is a warning.

### 6 · Docker images and everything else on the tag

relcut tags first, so a workflow on the tag or the release can build what belongs to it:

```yaml
on:
  release:
    types: [published]
```

GitHub starts no workflow for events made with `GITHUB_TOKEN`. Pass a GitHub App or
personal token as `github-token` when another workflow has to follow the release.

### 7 · From your machine

```sh
relcut release --branches main --publish github
```

Outside of Actions relcut takes the repository and branch from git, the token from
`gh auth token`, and asks before it tags: `Publish v1.4.3 of acme/widgets from main (github)? [y/N]`.
`--yes` skips the question; without a terminal and without `--yes` it refuses.

## What relcut guards against

| Risk | What relcut does |
| --- | --- |
| A dependency's install script reads your publish token | npm runs without any variable whose name holds `TOKEN`, `SECRET`, `PASSWORD` or `_AUTH`. `npm ci` runs with `--ignore-scripts`, `npm rebuild` and `npm pack` without credentials. `--pass-env` names exceptions. |
| A script looks for the token in relcut itself | relcut takes those variables out of its own environment at startup, where `ps` and `/proc` would show them. On Linux no process of the same user can read its memory, and npm runs with `no_new_privs`, so `sudo` is no way around that. The action leaves no shell behind that still holds them. |
| A script rewrites npm or node, which sit in the runner's writable tool cache | `prepare` fingerprints node, the npm package and npm's global config before any package code runs. npm gets a token only while every byte still matches, and it runs by its full path, so a planted `npm` or `node` on the `PATH` is never used. Run as separate steps, `publish` holds npm to what `prepare` recorded. |
| `actions/checkout` left the job token in `.git/config` | `prepare` refuses to run npm when the git config holds an `extraheader`, a URL with credentials or a credential helper. It names the setting, never its value. |
| A git hook or a rewritten git config runs next to the push token | git runs with `core.hooksPath=/dev/null`. Only `ls-remote` and `push` get the token, as a header through `GIT_CONFIG_*`, and they run from an empty repository of relcut's own: no git config of the checkout, the user or the system is read there. A proxy or CA for the push comes from the environment. The one exception is the fetch into a shallow checkout, which reads the checkout's config: it runs when relcut starts, before any script, and never in a `publish` after `prepare`. |
| A script plants a `git`, or changes git's helpers | relcut resolves git once when it starts, fingerprints the binary and its helpers, runs it by that path only, and checks the fingerprint again before every command that carries the token. Run as separate steps, `publish` holds git to what `prepare` recorded. |
| A script leaves a process running that reads a later npm's or git's environment | Each npm that runs scripts leads a process group of its own, killed once it returns; on Linux relcut adopts every orphan of its scripts and ends every descendant, round after round. By default `prepare` then fails: a release step is no place for a script that leaves something running (`side-effects: warn` goes on). On macOS a process that left its group survives. |
| A script writes to the runner's env, path, output, state or summary file, which the runner applies to every later step | The scripts' npm gets none of those variables, and the files are put back as they were once it returns. By default `prepare` then fails; with `side-effects: warn` what the scripts wrote to the env and path files comes out as the outputs `scripts-env` and `scripts-path`, for a workflow to apply on purpose. |
| A script steers `npm publish` through files it leaves behind | The npm that holds a token runs in a directory of relcut's own, with `--prefix` there, an environment of a fixed list of variables, and only the user config relcut wrote. The registry comes from `package.json` as committed, read object by object with every hash checked against the commit the remote points at. The tarball's `name`, `version` and `publishConfig` must be the committed name, the released version and the committed `publishConfig`, read the way npm unpacks it. |
| A script prints `::add-mask::` or a fake `::error::` | Workflow commands are stopped with a random token while npm and its scripts print. |
| A config file or plugin changes what the release does | There are none. Settings come from flags and `RELCUT_*` variables only, and an unprefixed `GITHUB_TOKEN` of another step is never picked up. |
| A newer commit landed while the release ran | `publish` tags only while the branch still points at the released commit, in one atomic push that git refuses once the branch has moved. Nothing is tagged then and the next release takes those commits; the branch itself is never updated. |
| A release fails after its tag | Assets, artifacts, the tarball and the npm dist-tags are checked before the tag. A release that still fails after it is finished by running again on the same commit: what is published stays, the rest is done. |
| A release is public before its assets | With assets the GitHub release is a draft until they are uploaded. |
| A commit that is not conventional gets merged | `check` fails the pull request on its own commits; history already on the branch never blocks a release. |
| A `Release-Cut:` or `release:` relcut cannot follow gets merged | `check` fails the pull request on its own commits. On the branch it is a warning and the release goes on without it, so it never blocks the releases after it. |
| A cut moves a branch that is there | The cut branch goes in the same atomic push as the tag, leased as absent: one that exists stops the release before the tag, and no push moves it. |
| A backport releases a version another branch released or will release | A branch releases only below the lowest version outside its history, checked against the remote's tags before the tag; `constraint` narrows it further. `check` stops it before anything is packed or tagged. |
| A backport moves `latest` back | A release below an existing version publishes under the branch's npm dist-tag, and its GitHub release never becomes the latest. |
| A flaky network fails a release halfway | GitHub requests retry on network errors, 5xx and rate limits, and time out; pushes retry. |
| A dry run that publishes | `dry-run` takes `true` or `false`; anything else is refused. |
| A damaged download of relcut in the action, or one an earlier step wrote over | The action checks the binary against the `SHA256SUMS` of the same release, fetched again on every run: a binary in the tool cache that no longer matches is downloaded again. |

What no release step can guard inside one job: npm scripts run as the job's user. They get
no token from relcut, but they can change files a later step runs or reads, such as the
tarball in the pack directory. relcut holds node, npm and git to their fingerprint and ends
what scripts leave running, but only from its own start to its own end: what ran before it
in the job, or between `prepare` and `publish` run as separate steps, can change them and
their record. `release` in one step keeps all of it in one process. Shared libraries a
Homebrew git or node loads are not fingerprinted. And on GitHub's Linux runners the job's
user may start containers: a script that does is root on the host, and past all of this. A release that must not trust its
dependencies at all packs in a job without secrets and publishes in another.

## What you get

- **One static binary of about 2 MB.** Linux builds are static musl binaries that run on
  any runner image; macOS builds for arm64 and x64. Nothing to `npm install` on every run.
- **No dependencies at runtime.** relcut needs git, and npm only when it publishes to npm.
- **No configuration to drift.** Every option is a flag or a `RELCUT_*` variable, the same
  in the action, in a workflow step and on your machine.
- **Readable logs.** Every step is a collapsible group with its command, outcome and
  duration; npm's output sits below the command that printed it.
- **Release branches without ceremony.** No setting: the tags tell a backport from `main`,
  and a `Release-Cut:` footer cuts the next one with the release of `main`.

## Reference

### Versions

| Commit | Release |
| --- | --- |
| `BREAKING CHANGE:` in the footer, or `feat!:` | major |
| `Release-Cut: <branch>` or `RELEASE CUT: <branch>` on the first line of the body | minor at least, and cuts the branch |
| `feat:` (or `feature:`) | minor |
| `fix:`, `perf:`, `revert:`, `Revert "…"` | patch |
| `release:`, or `Release:` on the first line of the body | with `releases: explicit`: patch at least, and releases |
| anything else | none |

`min-bump` raises the release to at least `patch`, `minor` or `major`, whatever the commits
make. Without a commit since the last release nothing releases.

The first release is `1.0.0`. The last release is the highest `v*` tag merged into the
commit; `tag-prefix` changes the `v`. The next one stays below the lowest version above it
that exists outside the commit's history, and is a backport when there is one.

### Options

Every option is a flag of the `relcut` binary, an input of the action, and a variable
`RELCUT_<NAME>`: `--npm-tag`, `npm-tag:`, `RELCUT_NPM_TAG`. A flag wins over its variable.
`log-style` and `yes` are no inputs of the action: in Actions relcut always writes
Actions logs and never asks.
Lists take values separated by commas, newlines or spaces (assets and artifacts only by
commas and newlines). `relcut <command> --help` lists what each command uses.

| Option | Default | |
| --- | --- | --- |
| `branches` | | Branches that release, e.g. `main release-*`. Patterns take `*`. |
| `releases` | `auto` | `auto` releases on every push; `explicit` waits for a `release:` commit or a `Release:` or `Release-Cut:` footer |
| `min-bump` | | Release at least `patch`, `minor` or `major`, whatever the commits make |
| `check-run-id` | | `${{ job.check_run_id }}`: the version goes into the check, and the release once it is out; needs `checks: write` |
| `side-effects` | `reject` | `reject` fails `prepare` when the package's scripts leave a process running or write the runner's files; `warn` undoes it and goes on |
| `constraint` | | Refuse versions outside it: `v2` or `v2.4` |
| `tag-prefix` | `v` | Before the version in tags |
| `publish` | | `npm`; the GitHub release is always made |
| `npm-tag` | `publishConfig.tag` | dist-tags; on a backport the branch name |
| `npm-stage` | `false` | `true` stages the version on npmjs for a maintainer to approve; one dist-tag, set with the approval |
| `npm-token` | | Without it npm uses trusted publishing; GitHub Packages takes `github-token` |
| `npm-install-token` | | Read token, for `npm ci` only |
| `github-token` | action: `github.token` | Locally: `gh auth token` |
| `github-assets` | | Files or JSON for the GitHub release |
| `github-artifacts` | | Artifacts of this workflow run for the GitHub release |
| `github-comments` | `true` | Comment on the released pull requests; none on a first release |
| `release-notes` | | Replaces the generated notes |
| `release-notes-template` | `{notes}` | Also `{version}` `{tag}` `{previous_tag}` `{date}` `{repository}` `{compare_url}` |
| `release-title` | `Version {tag}` | The GitHub release's title; also `{version}` `{date}` `{repository}` |
| `comment-template` | `🚀 Released in [{tag}]({release_url})`, with npm `· {npm_package} on {dist_tags}` | Also `{version}` `{date}` `{repository}`; `{npm_package}` and `{dist_tags}` are empty without npm |
| `working-directory` | `.` | The package to release |
| `pack-dir` | `$RUNNER_TEMP/relcut` | Where `prepare` leaves the tarball for `publish` |
| `keep-tarball` | `false` | Keep the tarball after `publish` |
| `pass-env` | | Credential-like variables the package's scripts may see, as patterns; the npm that holds a token sees none |
| `dry-run` | `false` | `true` publishes nothing |
| `log-style` | `auto` | `github-actions` in Actions, `plain` elsewhere |
| `yes` | `false` | Publish without asking, outside of Actions |

### Outputs

| Output | |
| --- | --- |
| `version` | The released version, e.g. `1.4.3`; empty when nothing releases |
| `tag` | Its tag, e.g. `v1.4.3` |
| `type` | `major`, `minor` or `patch` |
| `release` | `true` when this run releases; `false` again when the branch moved on before the tag |
| `notes` | The release notes |
| `cut` | The branch a `Release-Cut:` footer creates with this release |
| `npm-tarball` | The tarball `prepare` packed |
| `scripts-env`, `scripts-path` | With `side-effects: warn`: what the package's scripts wrote to the runner's env and path files, kept out of later steps |

### The action

```yaml
- uses: marcbachmann/relcut@v1.0.0        # installs and runs relcut
  with:
    command: release                       # or check, prepare, publish
- uses: marcbachmann/relcut/setup@v1.0.0  # only installs it, onto the PATH
- run: relcut check --branches main
```

The action runs on the runner's own node 24 and needs bash, curl and git, nothing else.
It picks the binary for the runner's
system and architecture (Linux and macOS, x64 and arm64), downloads it from the GitHub
release the action is pinned to, checks it against that release's `SHA256SUMS`, and keeps
it in the runner's tool cache, so a second relcut step in the job does not download again.
Pinned to a commit SHA, it finds the release tag on that commit; `version: 1.4.2` overrides
both. Then the node process becomes relcut with `execve`, so nothing stays behind that
holds the tokens. With `publish: npm` relcut needs node and npm on the `PATH`, as
`actions/setup-node` puts them, and installs neither; a release to GitHub alone needs
neither.

## Building

```sh
cargo test
scripts/dist.sh        # all four targets with cargo-zigbuild, into dist/
scripts/demo.sh        # see what relcut logs, against a throwaway repository
```

## License

MIT, see [LICENSE](LICENSE).
