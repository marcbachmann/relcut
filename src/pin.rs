use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

// The npm that gets a token, and the node that runs it, as they were before
// any package code ran. A dependency's script runs as the same user and can
// rewrite both, so their bytes are checked again before npm gets the token.
pub struct Npm {
    node: PathBuf,
    cli: PathBuf,
    digest: String,
    entries: Option<Vec<(String, String)>>,
    versions: Option<(String, String)>,
}

static PINNED: OnceLock<Npm> = OnceLock::new();
static GIT: OnceLock<GitPin> = OnceLock::new();

// The git relcut runs, by its full path from the moment relcut started, and
// the bytes of it and its helpers: a `git` a script planted on the PATH, or
// wrote over, answers nothing and gets no token.
pub struct GitPin {
    path: PathBuf,
    exec: PathBuf,
    digest: String,
    version: String,
}

fn git_entries(path: &Path, exec: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = vec![("git".to_string(), sha256(path)?)];
    tree(exec, "git-core", &mut out)?;
    Ok(out)
}

pub fn git() -> Result<&'static GitPin, String> {
    if let Some(git) = GIT.get() {
        return Ok(git);
    }
    let path = which("git").ok_or("git is not on the PATH")?;
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cmd = Command::new(&path);
    cmd.arg("--exec-path");
    crate::credentials::strip(&mut cmd);
    crate::credentials::confine(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let exec = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let exec = exec
        .canonicalize()
        .map_err(|e| format!("git --exec-path {}: {e}", exec.display()))?;
    let mut cmd = Command::new(&path);
    cmd.arg("--version");
    crate::credentials::strip(&mut cmd);
    crate::credentials::confine(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // git version 2.39.5 (Apple Git-154)
    let version = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap_or("unknown")
        .to_string();
    let entries = git_entries(&path, &exec)?;
    Ok(GIT.get_or_init(|| GitPin {
        path,
        exec,
        digest: digest(&entries),
        version,
    }))
}

pub fn git_path() -> &'static Path {
    GIT.get().map_or(Path::new("git"), |git| git.path.as_path())
}

impl GitPin {
    pub fn unchanged(&self) -> Result<(), String> {
        let now = digest(&git_entries(&self.path, &self.exec)?);
        if now == self.digest {
            return Ok(());
        }
        Err("git gets no token: a file of git changed since relcut started".into())
    }

    // v2.52.0 · /usr/bin/git
    pub fn uses(&self) -> String {
        format!("v{} · {}", self.version, self.path.display())
    }

    pub fn record(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, &self.digest).map_err(|e| format!("{}: {e}", path.display()))
    }

    // Ok(false) without a record: publish run without prepare.
    pub fn as_recorded(&self, path: &Path) -> Result<bool, String> {
        match std::fs::read_to_string(path) {
            Ok(digest) if digest.trim() == self.digest => Ok(true),
            Ok(_) => Err("git gets no token: a file of git changed since prepare".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn is_npm_package(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|pkg| pkg["name"] == "npm")
}

// The package behind `npm` on the PATH, or, when that is a wrapper, the one
// installed beside node: beside the `node` on the PATH, as Homebrew links it,
// or beside its binary, as node ships.
fn npm_package(npm: &Path, nodes: &[&Path]) -> Option<PathBuf> {
    let behind = npm.canonicalize().ok().and_then(|cli| {
        cli.ancestors()
            .find(|dir| is_npm_package(dir))
            .map(Path::to_path_buf)
    });
    behind.or_else(|| {
        nodes
            .iter()
            .filter_map(|node| Some(node.parent()?.parent()?.join("lib/node_modules/npm")))
            .find(|dir| is_npm_package(dir))
    })
}

// A shim on the PATH, like asdf's or volta's, is not the node that runs.
// With it, the version that node says it is.
fn node_binary(node: &Path) -> Result<(PathBuf, String), String> {
    let mut cmd = Command::new(node);
    cmd.args([
        "-e",
        "process.stdout.write(process.version + '\\n' + process.execPath)",
    ]);
    crate::credentials::strip(&mut cmd);
    crate::credentials::confine(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("{}: {e}", node.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (version, path) = text.rsplit_once('\n').unwrap_or(("unknown", &text));
    if !out.status.success() || path.is_empty() {
        return Err(format!("{} does not say where it is", node.display()));
    }
    let path = PathBuf::from(path)
        .canonicalize()
        .map_err(|e| format!("{path}: {e}"))?;
    Ok((path, version.to_string()))
}

fn sha256(file: &Path) -> Result<String, String> {
    use std::io::Read;
    let failed = |e: std::io::Error| format!("{}: {e}", file.display());
    let mut f = std::fs::File::open(file).map_err(failed)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        match f.read(&mut buf).map_err(failed)? {
            0 => break,
            n => hash.update(&buf[..n]),
        }
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn tree(dir: &Path, name: &str, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let mut children: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    children.sort();
    for path in children {
        let child = format!(
            "{name}/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let meta = path
            .symlink_metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            tree(&path, &child, out)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| e.to_string())?;
            out.push((child, format!("-> {}", target.display())));
        } else if meta.is_file() {
            out.push((child, sha256(&path)?));
        } else {
            out.push((child, "special".into()));
        }
    }
    Ok(())
}

fn entries(node: &Path, cli: &Path) -> Result<Vec<(String, String)>, String> {
    let package = cli
        .parent()
        .and_then(Path::parent)
        .ok_or("npm has no package directory")?;
    let mut out = vec![("node".to_string(), sha256(node)?)];
    tree(package, "npm", &mut out)?;
    // A certificate file the environment names is a file a script can write.
    for var in ["NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE"] {
        if let Some(file) = std::env::var_os(var).map(PathBuf::from) {
            out.push((var.to_string(), sha256(&file)?));
        }
    }
    // npm's global config, $prefix/etc/npmrc, sits beside the installation.
    let npmrc = node
        .parent()
        .and_then(Path::parent)
        .map(|prefix| prefix.join("etc/npmrc"));
    let npmrc = match npmrc.filter(|p| p.exists()) {
        Some(path) => sha256(&path)?,
        None => "absent".into(),
    };
    out.push(("etc/npmrc".into(), npmrc));
    Ok(out)
}

fn digest(entries: &[(String, String)]) -> String {
    let mut hash = Sha256::new();
    for (name, value) in entries {
        hash.update(format!("{name}\0{value}\n").as_bytes());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn first_difference(before: &[(String, String)], now: &[(String, String)]) -> String {
    let names = |e: &[(String, String)]| -> std::collections::BTreeMap<String, String> {
        e.iter().cloned().collect()
    };
    let (before, now) = (names(before), names(now));
    before
        .iter()
        .find(|(name, value)| now.get(*name) != Some(value))
        .map(|(name, _)| name.clone())
        .or_else(|| now.keys().find(|n| !before.contains_key(*n)).cloned())
        .unwrap_or_default()
}

// Before any package code runs: from then on npm is held to these bytes.
pub fn pin() -> Result<&'static Npm, String> {
    if let Some(npm) = PINNED.get() {
        return Ok(npm);
    }
    let on_path = which("node").ok_or("publishing to npm needs node on the PATH")?;
    let (node, node_version) = node_binary(&on_path)?;
    let npm = which("npm").ok_or("publishing to npm needs npm on the PATH")?;
    let package = npm_package(&npm, &[&on_path, &node])
        .ok_or_else(|| format!("{} is no npm, and there is none beside node", npm.display()))?;
    let cli = package.join("bin/npm-cli.js");
    let npm_version = std::fs::read_to_string(package.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|pkg| pkg["version"].as_str().map(|v| format!("v{v}")))
        .unwrap_or_else(|| "unknown".into());
    let entries = entries(&node, &cli)?;
    Ok(PINNED.get_or_init(|| Npm {
        digest: digest(&entries),
        entries: Some(entries),
        versions: Some((node_version, npm_version)),
        node,
        cli,
    }))
}

// The npm prepare pinned, in this process or, run as its own step, from its
// record.
pub fn recorded(record: &Path) -> Result<&'static Npm, String> {
    if let Some(npm) = PINNED.get() {
        return Ok(npm);
    }
    let text = std::fs::read_to_string(record).map_err(|_| "run prepare before publish")?;
    let json: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", record.display()))?;
    let field = |key: &str| {
        json[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{} has no {key}", record.display()))
    };
    let npm = Npm {
        node: field("node")?.into(),
        cli: field("npm")?.into(),
        digest: field("digest")?,
        entries: None,
        versions: None,
    };
    Ok(PINNED.get_or_init(|| npm))
}

impl Npm {
    // What changed since the pin, if anything did.
    pub fn changed(&self) -> Result<Option<String>, String> {
        let now = entries(&self.node, &self.cli)?;
        if digest(&now) == self.digest {
            return Ok(None);
        }
        Ok(Some(match &self.entries {
            Some(before) => first_difference(before, &now),
            None => "a file of node or npm".into(),
        }))
    }

    pub fn record(&self, path: &Path) -> Result<(), String> {
        let json = json!({
            "node": self.node,
            "npm": self.cli,
            "digest": self.digest,
        });
        std::fs::write(path, json.to_string()).map_err(|e| format!("{}: {e}", path.display()))
    }

    // v26.0.0 · /usr/local/bin/node, and npm's version and package.
    pub fn uses(&self) -> [String; 2] {
        let (node, npm) = self.versions.clone().unwrap_or_default();
        let package = self
            .cli
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&self.cli);
        [
            format!("{node} · {}", self.node.display()),
            format!("{npm} · {}", package.display()),
        ]
    }

    // For the npm that runs the package's scripts.
    pub fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.node);
        cmd.arg(&self.cli).current_dir(dir);
        crate::credentials::strip(&mut cmd);
        crate::credentials::confine(&mut cmd);
        crate::leftover::in_own_group(&mut cmd);
        crate::runner_files::keep_out(&mut cmd);
        cmd
    }

    // For the npm that holds a token: an environment of these variables
    // only, taken from relcut's own, never from the runner's files a script
    // may have written since. A planted NODE_OPTIONS or npm_config_* ends here.
    pub fn private_command(&self, dir: &Path) -> Command {
        const KEPT: [&str; 19] = [
            "CI",
            "GITHUB_ACTIONS",
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "NODE_EXTRA_CA_CERTS",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ];
        let mut cmd = Command::new(&self.node);
        cmd.arg(&self.cli).current_dir(dir).env_clear();
        for key in KEPT {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("NPM_CONFIG_USERCONFIG", dir.join(".npmrc"));
        crate::credentials::confine(&mut cmd);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("relcut-pin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // prefix/bin/{node,npm} and prefix/lib/node_modules/npm, as node ships.
    fn install(root: &Path) -> (PathBuf, PathBuf) {
        let package = root.join("prefix/lib/node_modules/npm");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::create_dir_all(package.join("lib")).unwrap();
        std::fs::write(package.join("package.json"), r#"{"name": "npm"}"#).unwrap();
        std::fs::write(package.join("bin/npm-cli.js"), "require('../lib/cli.js')").unwrap();
        std::fs::write(package.join("lib/cli.js"), "module.exports = 1").unwrap();
        let bin = root.join("prefix/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("node"), "a node").unwrap();
        std::os::unix::fs::symlink("../lib/node_modules/npm/bin/npm-cli.js", bin.join("npm"))
            .unwrap();
        (bin.join("node"), bin.join("npm"))
    }

    #[test]
    fn finds_npm_through_its_link_or_beside_node() {
        let root = scratch("find");
        let (node, npm) = install(&root);
        let package = root
            .join("prefix/lib/node_modules/npm")
            .canonicalize()
            .unwrap();
        assert_eq!(npm_package(&npm, &[&node]), Some(package.clone()));
        let wrapper = root.join("wrapper");
        std::fs::write(&wrapper, "#!/bin/sh\nexec npm \"$@\"\n").unwrap();
        let found =
            |nodes: &[&Path]| npm_package(&wrapper, nodes).map(|p| p.canonicalize().unwrap());
        assert_eq!(found(&[&node]), Some(package.clone()));
        let cellar = root.join("Cellar/node/25/bin/node");
        assert_eq!(found(&[&node, &cellar]), Some(package.clone()));
        assert_eq!(found(&[&cellar]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_and_its_helpers_are_fingerprinted_together() {
        let root = scratch("git");
        let exec = root.join("libexec/git-core");
        std::fs::create_dir_all(&exec).unwrap();
        std::fs::write(root.join("git"), "a git").unwrap();
        std::fs::write(exec.join("git-remote-https"), "a helper").unwrap();
        let before = digest(&git_entries(&root.join("git"), &exec).unwrap());
        std::fs::write(exec.join("git-remote-https"), "another helper").unwrap();
        assert_ne!(
            before,
            digest(&git_entries(&root.join("git"), &exec).unwrap())
        );
        std::fs::write(exec.join("git-remote-https"), "a helper").unwrap();
        assert_eq!(
            before,
            digest(&git_entries(&root.join("git"), &exec).unwrap())
        );
        std::fs::write(root.join("git"), "another git").unwrap();
        assert_ne!(
            before,
            digest(&git_entries(&root.join("git"), &exec).unwrap())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn any_changed_byte_shows_and_names_its_file() {
        let root = scratch("change");
        let (node, npm) = install(&root);
        let cli = npm.canonicalize().unwrap();
        let before = entries(&node, &cli).unwrap();
        assert_eq!(digest(&before), digest(&entries(&node, &cli).unwrap()));
        assert!(
            before
                .iter()
                .any(|(n, v)| n == "etc/npmrc" && v == "absent")
        );

        let changed = |edit: &dyn Fn()| {
            edit();
            let now = entries(&node, &cli).unwrap();
            assert_ne!(digest(&before), digest(&now));
            first_difference(&before, &now)
        };
        let package = root.join("prefix/lib/node_modules/npm");
        assert_eq!(
            changed(&|| std::fs::write(package.join("lib/cli.js"), "evil").unwrap()),
            "npm/lib/cli.js"
        );
        std::fs::write(package.join("lib/cli.js"), "module.exports = 1").unwrap();
        assert_eq!(
            changed(&|| std::fs::write(package.join("lib/extra.js"), "").unwrap()),
            "npm/lib/extra.js"
        );
        std::fs::remove_file(package.join("lib/extra.js")).unwrap();
        assert_eq!(
            changed(&|| std::fs::write(&node, "another node").unwrap()),
            "node"
        );
        std::fs::write(&node, "a node").unwrap();
        std::fs::create_dir_all(root.join("prefix/etc")).unwrap();
        assert_eq!(
            changed(&|| std::fs::write(root.join("prefix/etc/npmrc"), "proxy=x").unwrap()),
            "etc/npmrc"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
