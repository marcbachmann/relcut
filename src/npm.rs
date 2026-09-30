use crate::git::Git;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const NPMJS: &str = "https://registry.npmjs.org";
pub const GITHUB_PACKAGES: &str = "https://npm.pkg.github.com";

fn with_token(cmd: &mut Command, dir: &Path, registry: &str, token: &str) -> Result<(), String> {
    let rc = dir.join(".npmrc");
    std::fs::write(
        &rc,
        format!("//{}/:_authToken=${{NPM_TOKEN}}\n", host(registry)),
    )
    .map_err(|e| e.to_string())?;
    cmd.env("NPM_CONFIG_USERCONFIG", rc).env("NPM_TOKEN", token);
    Ok(())
}

pub const LOCKFILES: [&str; 2] = ["npm-shrinkwrap.json", "package-lock.json"];

// The lockfile npm installed from: npm-shrinkwrap.json unless it is missing
// or empty, as npm reads them.
fn lockfile(dir: &Path) -> Result<(&'static str, String), String> {
    for name in LOCKFILES {
        match std::fs::read_to_string(dir.join(name)) {
            Ok(text) if !text.is_empty() => return Ok((name, text)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
    Err("npm ci needs an npm-shrinkwrap.json or package-lock.json".to_string())
}

// The lockfile npm ci installs from, and how many of its packages have an
// install script.
pub fn install_scripts(dir: &Path) -> Result<(&'static str, usize), String> {
    let (name, lock) = lockfile(dir)?;
    let lock: Value = serde_json::from_str(&lock).map_err(|e| format!("{name}: {e}"))?;
    let scripted = lock["packages"].as_object().map_or(0, |p| {
        p.values().filter(|p| p["hasInstallScript"] == true).count()
    });
    Ok((name, scripted))
}

// No dependency script runs during the install, which may hold a read token;
// the ones the lockfile marks run afterwards in `npm rebuild`, without it.
pub fn install(
    npm: &crate::pin::Npm,
    dir: &Path,
    token: Option<&str>,
    temp: &Path,
    scripted: bool,
) -> Result<(), String> {
    let mut ci = npm.command(dir);
    ci.args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]);
    if let Some(token) = token {
        std::fs::create_dir_all(temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        with_token(&mut ci, temp, NPMJS, token)?;
    }
    status(ci, true)?;
    if scripted {
        let mut rebuild = npm.command(dir);
        rebuild.args(["rebuild", "--ignore-scripts=false"]);
        status(rebuild, true)?;
    }
    Ok(())
}

pub fn read(dir: &Path) -> Result<Value, String> {
    let file = dir.join("package.json");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))
}

// The version, and under `ci` where this build came from: repository, the
// directory below its root, date, commit, buildUrl, branch and tag.
pub fn stamp(dir: &Path, version: &str, ci: Value) -> Result<(), String> {
    let mut pkg = read(dir)?;
    pkg["version"] = Value::String(version.into());
    pkg["ci"] = ci;
    let text = serde_json::to_string_pretty(&pkg).unwrap() + "\n";
    std::fs::write(dir.join("package.json"), text).map_err(|e| e.to_string())
}

// npm pack runs the package's prepack, prepare and postpack scripts, without
// any credential; publish then uploads this tarball as it is. Its output goes
// to the log, so the tarball is the one .tgz in an emptied destination.
pub fn pack(npm: &crate::pin::Npm, dir: &Path, dest: &Path) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let tarballs = || -> Result<Vec<std::path::PathBuf>, String> {
        let entries = std::fs::read_dir(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        Ok(entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "tgz"))
            .collect())
    };
    for old in tarballs()? {
        std::fs::remove_file(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    }
    let mut cmd = npm.command(dir);
    cmd.args(["pack", "--ignore-scripts=false", "--pack-destination"])
        .arg(dest);
    status(cmd, true)?;
    match tarballs()?.as_slice() {
        [one] => Ok(one.clone()),
        other => Err(format!(
            "npm pack left {} tarballs in {}",
            other.len(),
            dest.display()
        )),
    }
}

pub struct Publish<'a> {
    pub npm: &'a crate::pin::Npm,
    pub private: &'a Private,
    pub registry: &'a str,
    pub tarball: &'a Path,
    pub name: &'a str,
    pub version: &'a str,
    pub tags: &'a [String],
    pub token: Option<&'a str>,
    pub stage: bool,
    pub dry_run: bool,
}

// An empty directory of relcut's own for the npm that holds a token: no
// project .npmrc and nothing else a package script left in the checkout.
pub struct Private {
    pub dir: PathBuf,
}

impl Private {
    pub fn new() -> Result<Private, String> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("relcut-npm-{}-{nanos:x}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(dir.join(".npmrc"), "").map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Private { dir })
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// With trusted publishing, `npm dist-tag` takes the OIDC token from npm
// 11.21.0 on 11.x and 12.2.0 on 12.x; `npm publish --tag` from 11.5.1.
fn oidc_dist_tags(npm_version: &str) -> bool {
    match numbers(npm_version).as_slice() {
        [11, minor, ..] => *minor >= 21,
        [12, minor, ..] => *minor >= 2,
        [major, ..] => *major > 12,
        [] => false,
    }
}

// `npm stage` came with npm 11.15.0 and is in every 12.x.
fn stages(npm_version: &str) -> bool {
    match numbers(npm_version).as_slice() {
        [11, minor, ..] => *minor >= 15,
        [major, ..] => *major >= 12,
        [] => false,
    }
}

fn numbers(npm_version: &str) -> Vec<u64> {
    npm_version
        .trim()
        .split('.')
        .map_while(|p| p.parse().ok())
        .collect()
}

fn version(npm: &crate::pin::Npm, private: &Private) -> Result<String, String> {
    let out = npm
        .private_command(&private.dir)
        .arg("--version")
        .output()
        .map_err(|e| format!("npm --version: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn takes_stage(npm: &crate::pin::Npm, private: &Private) -> Result<(), String> {
    let version = version(npm, private)?;
    if stages(&version) {
        return Ok(());
    }
    Err(format!(
        "npm {version} has no npm stage, it needs 11.15.0: pass a newer npm or leave npm-stage out"
    ))
}

pub fn oidc_takes_dist_tags(npm: &crate::pin::Npm, private: &Private) -> Result<(), String> {
    let version = version(npm, private)?;
    if oidc_dist_tags(&version) {
        return Ok(());
    }
    Err(format!(
        "npm {version} cannot add dist-tags through trusted publishing, it needs 11.21.0 or 12.2.0: pass one npm-tag, an npm-token or a newer npm"
    ))
}

// What semver reads as a version range, which npm refuses as a dist-tag:
// v1.4, 1.x, >=2. Rather one name too many than a publish that fails after
// the tag.
pub fn version_range(tag: &str) -> bool {
    let part = |p: &str| {
        matches!(p, "x" | "X" | "*") || (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let comparator = |c: &str| {
        let c = c
            .trim_start_matches(['<', '>', '=', '~', '^'])
            .trim_start_matches(['v', 'V']);
        let c = c.split('+').next().unwrap_or_default();
        let (core, pre) = match c.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (c, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        c.is_empty()
            || (parts.len() <= 3
                && parts.iter().all(|p| part(p))
                && (pre.is_none() || parts.len() == 3))
    };
    tag.split("||")
        .all(|set| set.split_whitespace().all(|c| c == "-" || comparator(c)))
}

pub fn host(registry: &str) -> &str {
    registry
        .split_once("://")
        .map_or(registry, |(_, h)| h)
        .trim_end_matches('/')
}

// Where npm publishes the package, as npm-registry-fetch's pickRegistry reads
// the publishConfig: the name's scope registry, then the one of
// publishConfig.scope, then registry.
pub fn publish_registry(manifest: &Value) -> String {
    let config = &manifest["publishConfig"];
    let scoped = |scope: &str| {
        let scope = scope.trim_start_matches('@');
        config[format!("@{scope}:registry")].as_str()
    };
    manifest["name"]
        .as_str()
        .and_then(|name| name.strip_prefix('@')?.split_once('/'))
        .and_then(|(scope, _)| scoped(scope))
        .or_else(|| {
            config["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .and_then(scoped)
        })
        .or_else(|| config["registry"].as_str())
        .unwrap_or(NPMJS)
        .to_string()
}

// The repository path a manifest's `repository` names, a string or {url}:
// git+https://github.com/o/r.git, git@github.com:o/r.git, github:o/r, o/r.
pub fn repository(manifest: &Value) -> Option<String> {
    let url = manifest["repository"]
        .as_str()
        .or_else(|| manifest["repository"]["url"].as_str())?;
    let url = url.strip_prefix("git+").unwrap_or(url);
    let path = if let Some(rest) = url.strip_prefix("github:") {
        rest
    } else if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/').map_or("", |(_, path)| path)
    } else if let Some((_, rest)) = url.split_once(':') {
        rest
    } else {
        url
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    (!path.is_empty()).then(|| path.to_string())
}

// The manifest as committed: what a package script wrote into the checkout
// afterwards decides nothing about where a token goes.
pub fn committed(git: &Git, head: &str) -> Result<Value, String> {
    let prefix = git.run(&["rev-parse", "--show-prefix"])?;
    let path = format!("{}package.json", prefix.trim());
    let text = git.file_at(head, &path)?;
    serde_json::from_slice(&text).map_err(|e| format!("package.json at HEAD: {e}"))
}

// npm publish takes every config key from the tarball's own publishConfig,
// which a prepack script writes: a scoped registry or a proxy there decides
// where the token goes, whatever --registry says.
pub fn tarball_manifest(tarball: &Path) -> Result<Value, String> {
    use std::io::Read;
    let failed = |e: std::io::Error| format!("{}: {e}", tarball.display());
    let file = std::fs::File::open(tarball).map_err(failed)?;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(file)
        .read_to_end(&mut tar)
        .map_err(failed)?;
    manifest_in(&tar).map_err(|what| format!("{}: {what}", tarball.display()))
}

// npm unpacks the tarball with its first path component stripped, then reads
// package.json: the last entry that lands there is the manifest, and only a
// plain file may land there.
fn manifest_in(tar: &[u8]) -> Result<Value, String> {
    let (mut at, mut manifest, mut long_name) = (0, None, None::<String>);
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range])
                .trim_end_matches('\0')
                .to_string()
        };
        let size = usize::from_str_radix(field(124..136).trim(), 8)
            .map_err(|_| "not a tar archive".to_string())?;
        let body = tar
            .get(at + 512..at + 512 + size)
            .ok_or_else(|| "truncated".to_string())?;
        let name = long_name.take().unwrap_or_else(|| match field(345..500) {
            prefix if prefix.is_empty() => field(0..100),
            prefix => format!("{prefix}/{}", field(0..100)),
        });
        let kind = header[156];
        if matches!(kind, b'x' | b'L') {
            let text = String::from_utf8_lossy(body);
            long_name = if kind == b'L' {
                Some(text.trim_end_matches('\0').to_string())
            } else {
                text.lines()
                    .filter_map(|line| line.split_once(' ')?.1.strip_prefix("path="))
                    .next_back()
                    .map(str::to_string)
            };
        } else if lands_at_package_json(&name) {
            if !matches!(kind, b'0' | 0 | b'7') {
                return Err("package.json in the tarball is not a plain file".into());
            }
            manifest = Some(body.to_vec());
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    let manifest = manifest.ok_or_else(|| "no package.json".to_string())?;
    serde_json::from_slice(&manifest).map_err(|e| format!("package.json: {e}"))
}

// After the first component goes, `.` and empty segments collapse the way
// path.resolve does; `..` is refused by npm's tar and never lands.
fn lands_at_package_json(name: &str) -> bool {
    let mut parts = name.split('/');
    parts.next();
    let rest: Vec<&str> = parts.filter(|p| !p.is_empty() && *p != ".").collect();
    rest == ["package.json"]
}

// Without a token npm goes through trusted publishing.
// `--prefix` names the project directory outright: without it npm walks up
// from the working directory and takes the .npmrc of any ancestor that holds
// a package.json, which under /tmp anyone can plant.
fn registry_command(p: &Publish, args: &[&str]) -> Result<Command, String> {
    let mut cmd = p.npm.private_command(&p.private.dir);
    cmd.args(args)
        .arg("--prefix")
        .arg(&p.private.dir)
        .args(["--registry", p.registry]);
    match p.token {
        Some(token) => with_token(&mut cmd, &p.private.dir, p.registry, token)?,
        None => crate::credentials::restore(
            &mut cmd,
            &[
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
                "ACTIONS_ID_TOKEN_REQUEST_URL",
            ],
        ),
    }
    Ok(cmd)
}

pub enum Published {
    No,
    From(String),
    Unstamped,
}

// prepare stamps ci.commit into the package.json it packs, so the registry
// names the commit a version was published from. Tarballs cannot be compared:
// ci.date makes every repack another one.
pub fn published(p: &Publish) -> Published {
    let spec = format!("{}@{}", p.name, p.version);
    registry_command(p, &["view", &spec, "version", "ci.commit", "--json"])
        .and_then(|mut cmd| cmd.output().map_err(|e| e.to_string()))
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
        .map_or(Published::No, |fields| published_as(&fields, p.version))
}

// npm view prints the one field a version has as a bare value, two as an
// object keyed by their names.
fn published_as(fields: &Value, version: &str) -> Published {
    if fields == version {
        return Published::Unstamped;
    }
    if fields["version"] != version {
        return Published::No;
    }
    match fields["ci.commit"].as_str() {
        Some(commit) => Published::From(commit.to_string()),
        None => Published::Unstamped,
    }
}

// npm view shows no staged version, and staging it a second time fails: the
// queue of the package says whether an earlier run left it there.
fn staged(p: &Publish) -> bool {
    registry_command(p, &["stage", "list", p.name, "--json"])
        .and_then(|mut cmd| cmd.output().map_err(|e| e.to_string()))
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
        .and_then(|items| {
            let items = items.as_array()?;
            Some(items.iter().any(|item| item["version"] == p.version))
        })
        .unwrap_or(false)
}

// The first tag is set with the publish, any other with npm dist-tag
// afterwards. A version the registry already has from this commit is not
// published again, nor one an earlier run staged: Ok(false). A staged version
// takes its one tag with the approval.
pub fn publish(p: &Publish, there: bool, resumed: bool) -> Result<bool, String> {
    let (first, rest) = match p.tags.split_first() {
        Some((first, rest)) => (Some(first.as_str()), rest),
        None => (None, &[][..]),
    };
    let fresh = !(there || (resumed && p.stage && staged(p)));
    if fresh {
        let tarball = p.tarball.to_str().ok_or("tarball path is not UTF-8")?;
        let mut args = if p.stage { vec!["stage"] } else { vec![] };
        args.extend(["publish", tarball, "--ignore-scripts"]);
        if let Some(tag) = first {
            args.extend(["--tag", tag]);
        }
        if p.dry_run {
            args.push("--dry-run");
        }
        status(registry_command(p, &args)?, false)?;
    }
    for tag in rest {
        let spec = format!("{}@{}", p.name, p.version);
        if p.dry_run {
            crate::log::info(&format!("would run npm dist-tag add {spec} {tag}"));
        } else {
            status(
                registry_command(p, &["dist-tag", "add", &spec, tag])?,
                false,
            )?;
        }
    }
    Ok(fresh)
}

// `scripts`: the child runs the package's own code, and nothing it started
// may outlive it.
fn status(mut cmd: Command, scripts: bool) -> Result<(), String> {
    let line = shown(&cmd);
    // A command that runs the package's scripts runs in its directory; the
    // others in relcut's own, which says nothing to the reader.
    let dir = cmd.get_current_dir().filter(|_| scripts);
    crate::log::command(dir, &line);
    let files = scripts.then(crate::runner_files::take);
    let ran = crate::log::run(&mut cmd, scripts);
    if let Some(files) = files {
        files.restore();
    }
    let s = ran.map_err(|e| format!("{line}: {e}"))?;
    if s.success() {
        Ok(())
    } else {
        Err(format!("{line} failed with {s}"))
    }
}

// The pinned npm runs as node with npm's cli first; the log says npm, and
// leaves out the tarball and relcut's own directories, which the step names.
fn shown(cmd: &Command) -> String {
    let mut args = cmd.get_args().skip(1).map(|a| a.to_string_lossy());
    let mut shown = vec!["npm".to_string()];
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--prefix" | "--pack-destination" => {
                args.next();
            }
            a if a.ends_with(".tgz") => {}
            a => shown.push(a.to_string()),
        }
    }
    shown.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_shows_npm_without_relcuts_paths() {
        let mut cmd = Command::new("node");
        cmd.args([
            "/x/npm-cli.js",
            "publish",
            "/tmp/relcut/x-1.0.0.tgz",
            "--dry-run",
        ])
        .args([
            "--prefix",
            "/tmp/relcut-npm-1",
            "--registry",
            "https://r.example",
        ]);
        assert_eq!(
            shown(&cmd),
            "npm publish --dry-run --registry https://r.example"
        );
    }

    #[test]
    fn the_publish_registry_is_the_one_npm_picks() {
        let pick = |m: Value| publish_registry(&m);
        let gpr = "https://npm.pkg.github.com";
        assert_eq!(pick(serde_json::json!({"name": "@o/p"})), NPMJS);
        assert_eq!(
            pick(serde_json::json!({"name": "p", "publishConfig": {"registry": gpr}})),
            gpr
        );
        assert_eq!(
            pick(
                serde_json::json!({"name": "@o/p", "publishConfig": {"registry": "https://a", "@o:registry": gpr}})
            ),
            gpr
        );
        assert_eq!(
            pick(
                serde_json::json!({"name": "@o/p", "publishConfig": {"registry": gpr, "@x:registry": "https://a"}})
            ),
            gpr
        );
        assert_eq!(
            pick(
                serde_json::json!({"name": "p", "publishConfig": {"scope": "o", "@o:registry": gpr}})
            ),
            gpr
        );
        assert_eq!(
            pick(
                serde_json::json!({"name": "@o/p", "publishConfig": {"scope": "@x", "@x:registry": gpr}})
            ),
            gpr
        );
    }

    fn entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum::<u32>() + 8 * 32;
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = header;
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    #[test]
    fn the_manifest_is_the_entry_npm_lands_at_package_json() {
        let benign = br#"{"publishConfig": {"registry": "https://a/"}}"#;
        let evil = br#"{"publishConfig": {"proxy": "http://b/"}}"#;
        let registry = |tar: Vec<u8>| manifest_in(&tar).map(|m| m["publishConfig"].clone());
        let plain = entry("package/package.json", b'0', benign);
        assert_eq!(registry(plain.clone()).unwrap()["registry"], "https://a/");
        for later in [
            "package/./package.json",
            "package//package.json",
            "./package.json",
            "x/package.json",
        ] {
            let tar = [plain.clone(), entry(later, b'0', evil)].concat();
            assert_eq!(registry(tar).unwrap()["proxy"], "http://b/", "{later}");
        }
        for elsewhere in [
            "package/../package.json",
            "package/sub/package.json",
            "package.json",
        ] {
            let tar = [plain.clone(), entry(elsewhere, b'0', evil)].concat();
            assert_eq!(
                registry(tar).unwrap()["registry"],
                "https://a/",
                "{elsewhere}"
            );
        }
        let link = [plain.clone(), entry("package/package.json", b'2', b"")].concat();
        assert!(registry(link).unwrap_err().contains("not a plain file"));
        let mut pax = entry("package/x", b'x', b"29 path=package/./package.json\n");
        pax.extend(entry("package/x", b'0', evil));
        assert_eq!(
            registry([plain.clone(), pax].concat()).unwrap()["proxy"],
            "http://b/"
        );
        let mut long = entry("././@LongLink", b'L', b"package/package.json\0");
        long.extend(entry("package/short", b'0', evil));
        assert_eq!(
            registry([plain, long].concat()).unwrap()["proxy"],
            "http://b/"
        );
    }

    #[test]
    fn the_repository_a_manifest_names() {
        let named = |r: &str| repository(&serde_json::json!({"repository": r}));
        for url in [
            "git+https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "git+ssh://git@github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "github:acme/widgets",
            "acme/widgets",
        ] {
            assert_eq!(named(url).as_deref(), Some("acme/widgets"), "{url}");
        }
        let object = serde_json::json!({"repository": {"type": "git", "url": "git+https://github.com/acme/widgets.git"}});
        assert_eq!(repository(&object).as_deref(), Some("acme/widgets"));
        assert_eq!(repository(&serde_json::json!({})), None);
    }

    #[test]
    fn dist_tags_that_read_as_a_version_range() {
        for range in [
            "v1.4",
            "v1",
            "1.x",
            "*",
            ">=1.2",
            "^2",
            "1.2.3-beta",
            "1 - 2",
            "1 || 2",
        ] {
            assert!(version_range(range), "{range}");
        }
        for name in [
            "latest",
            "next",
            "beta",
            "release-1.4",
            "release/1.4",
            "2026-09",
            "v2-beta",
        ] {
            assert!(!version_range(name), "{name}");
        }
    }

    #[test]
    fn dist_tags_through_oidc_need_npm_11_21_or_12_2() {
        for ok in ["11.21.0", "11.22.1\n", "12.2.0", "12.10.0", "13.0.0"] {
            assert!(oidc_dist_tags(ok), "{ok}");
        }
        for old in ["11.5.1", "11.20.9", "12.1.0", "10.9.2", ""] {
            assert!(!oidc_dist_tags(old), "{old}");
        }
    }

    #[test]
    fn the_registry_names_the_commit_a_version_was_published_from() {
        let read = |fields: Value| published_as(&fields, "1.2.3");
        let stamped = serde_json::json!({"version": "1.2.3", "ci.commit": "abc"});
        assert!(matches!(read(stamped), Published::From(c) if c == "abc"));
        assert!(matches!(
            read(serde_json::json!("1.2.3")),
            Published::Unstamped
        ));
        assert!(matches!(
            read(serde_json::json!({"version": "1.2.3"})),
            Published::Unstamped
        ));
        for other in [
            serde_json::json!("1.2.4"),
            serde_json::json!({"version": "1.2.4", "ci.commit": "abc"}),
            serde_json::json!([]),
            Value::Null,
        ] {
            assert!(matches!(read(other.clone()), Published::No), "{other}");
        }
    }

    #[test]
    fn npm_stage_needs_npm_11_15() {
        for ok in ["11.15.0", "11.21.0\n", "12.0.0", "12.2.0", "13.0.0"] {
            assert!(stages(ok), "{ok}");
        }
        for old in ["11.14.1", "11.5.1", "10.9.2", ""] {
            assert!(!stages(old), "{old}");
        }
    }

    #[test]
    fn the_lockfile_is_the_one_npm_installs_from() {
        let dir = std::env::temp_dir().join(format!("relcut-lockfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (shrinkwrap, lock) = (
            dir.join("npm-shrinkwrap.json"),
            dir.join("package-lock.json"),
        );

        assert!(lockfile(&dir).is_err());
        std::fs::write(&lock, "lock").unwrap();
        assert_eq!(
            lockfile(&dir).unwrap(),
            ("package-lock.json", "lock".into())
        );
        std::fs::write(&shrinkwrap, "").unwrap();
        assert_eq!(
            lockfile(&dir).unwrap(),
            ("package-lock.json", "lock".into())
        );
        std::fs::write(&shrinkwrap, "shrinkwrap").unwrap();
        assert_eq!(
            lockfile(&dir).unwrap(),
            ("npm-shrinkwrap.json", "shrinkwrap".into())
        );
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(
            lockfile(&dir).unwrap(),
            ("npm-shrinkwrap.json", "shrinkwrap".into())
        );

        std::fs::remove_file(&shrinkwrap).unwrap();
        std::fs::create_dir(&shrinkwrap).unwrap();
        std::fs::write(&lock, "lock").unwrap();
        let err = lockfile(&dir).unwrap_err();
        assert!(err.starts_with("npm-shrinkwrap.json: "), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
