use crate::version::Version;
use std::path::PathBuf;
use std::process::Command;

// A hook is the repository's code, and no git of relcut runs one.
fn git(args: &[&str]) -> Command {
    let mut cmd = Command::new(crate::pin::git_path());
    cmd.args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
    crate::credentials::strip(&mut cmd);
    crate::credentials::confine(&mut cmd);
    cmd
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// The id git gives an object of this kind and content: a sha1 of 40 hex
// digits, or sha256 in a repository of that format.
fn object_id(kind: &str, content: &[u8], like: &str) -> String {
    use sha1::Digest;
    let header = format!("{kind} {}\0", content.len());
    if like.len() == 64 {
        let mut hash = sha2::Sha256::new();
        hash.update(header.as_bytes());
        hash.update(content);
        hex(&hash.finalize())
    } else {
        let mut hash = sha1::Sha1::new();
        hash.update(header.as_bytes());
        hash.update(content);
        hex(&hash.finalize())
    }
}

fn output(mut cmd: Command, args: &[&str]) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("git {}: {e}", args[0]))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// The tags that name a version after the prefix, with it.
fn versions<'a>(tags: impl Iterator<Item = &'a str>, prefix: &str) -> Vec<(String, Version)> {
    tags.filter_map(|t| Version::parse(t.strip_prefix(prefix)?).map(|v| (t.to_string(), v)))
        .collect()
}

fn retrying<T>(mut attempt_once: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut attempt = 0;
    loop {
        match attempt_once() {
            Err(e) if attempt < 2 => {
                let wait = std::time::Duration::from_secs(1 << attempt);
                crate::log::retry(&format!("{e}, again in {}s", wait.as_secs()));
                std::thread::sleep(wait);
                attempt += 1;
            }
            result => return result,
        }
    }
}

// The checkout. Nothing here talks to the remote or holds the token.
pub struct Git {
    pub cwd: PathBuf,
}

impl Git {
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = git(args);
        cmd.current_dir(&self.cwd);
        output(cmd, args)
    }

    pub fn is_shallow(&self) -> Result<bool, String> {
        Ok(self.run(&["rev-parse", "--is-shallow-repository"])?.trim() == "true")
    }

    // A shallow checkout answers ranges and ancestry wrongly, without an
    // error. Its history comes in as every branch and tag, commits without
    // their trees. The fetch reads the checkout's git config: only before
    // any script of the repository ran, while it is what the checkout wrote.
    pub fn unshallow(&self, server_url: &str, token: Option<&str>) -> Result<(), String> {
        crate::pin::git()?.unchanged()?;
        let header = token.map(|token| {
            format!(
                "AUTHORIZATION: basic {}",
                base64(format!("x-access-token:{token}").as_bytes())
            )
        });
        let key = format!("http.{server_url}/.extraheader");
        let args = [
            "fetch",
            "--quiet",
            "--unshallow",
            "--filter=tree:0",
            "--tags",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ];
        retrying(|| {
            if !self.is_shallow()? {
                return Ok(());
            }
            let mut cmd = git(&args);
            cmd.current_dir(&self.cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0");
            // An empty value drops any header the checkout's config holds.
            if let Some(header) = &header {
                cmd.env("GIT_CONFIG_COUNT", "2")
                    .env("GIT_CONFIG_KEY_0", &key)
                    .env("GIT_CONFIG_VALUE_0", "")
                    .env("GIT_CONFIG_KEY_1", &key)
                    .env("GIT_CONFIG_VALUE_1", header);
            }
            output(cmd, &args).map(|_| ())
        })
    }

    // The version tags merged into any of the tips, the highest last.
    pub fn releases(&self, prefix: &str, tips: &[&str]) -> Result<Vec<(String, Version)>, String> {
        let tags = self.tags("--merged", prefix, tips)?;
        let mut releases = versions(tags.lines(), prefix);
        releases.sort_by_key(|(_, v)| *v);
        Ok(releases)
    }

    // The version tags outside the tips' history: the releases of other branches.
    pub fn foreign_releases(
        &self,
        prefix: &str,
        tips: &[&str],
    ) -> Result<Vec<(String, Version)>, String> {
        let tags = self.tags("--no-merged", prefix, tips)?;
        Ok(versions(tags.lines(), prefix))
    }

    fn tags(&self, filter: &str, prefix: &str, tips: &[&str]) -> Result<String, String> {
        let pattern = format!("{prefix}*");
        let mut args = vec!["tag"];
        for tip in tips {
            args.extend([filter, tip]);
        }
        args.extend(["--list", &pattern]);
        self.run(&args)
    }

    // A branch as the checkout knows it, from the remote or a local one.
    pub fn branch_tip(&self, name: &str) -> Option<String> {
        [
            format!("refs/remotes/origin/{name}"),
            format!("refs/heads/{name}"),
        ]
        .iter()
        .find_map(|r| self.run(&["rev-parse", "--verify", "--quiet", r]).ok())
        .map(|sha| sha.trim().to_string())
    }

    pub fn is_ancestor(&self, ancestor: &str, of: &str) -> bool {
        self.run(&["merge-base", "--is-ancestor", ancestor, of])
            .is_ok()
    }

    pub fn tags_at_head(&self) -> Result<Vec<String>, String> {
        let tags = self.run(&["tag", "--points-at", "HEAD"])?;
        Ok(tags.lines().map(str::to_string).collect())
    }

    fn log(&self, revisions: &[&str]) -> Result<Vec<(String, String)>, String> {
        let mut args = vec!["log", "--no-merges", "--format=%H%x1f%B%x1e"];
        args.extend(revisions);
        args.push("--");
        let log = self.run(&args)?;
        Ok(log
            .split('\x1e')
            .filter_map(|entry| {
                let (sha, message) = entry.trim_start().split_once('\x1f')?;
                Some((sha.to_string(), message.trim().to_string()))
            })
            .collect())
    }

    // The pull request's own commits: those not yet on the branch it merges into.
    pub fn commits_beyond(&self, base: &str) -> Result<Vec<(String, String)>, String> {
        self.log(&[&format!("origin/{base}..HEAD")])
            .map_err(|e| format!("{e}; check out with fetch-depth: 0"))
    }

    pub fn commits_since(
        &self,
        tag: Option<&str>,
        tips: &[&str],
    ) -> Result<Vec<(String, String)>, String> {
        let not = tag.map(|t| format!("^{t}"));
        self.log(
            &not.iter()
                .map(String::as_str)
                .chain(tips.iter().copied())
                .collect::<Vec<_>>(),
        )
    }

    pub fn commits_between(
        &self,
        tag: Option<&str>,
        to: &str,
    ) -> Result<Vec<(String, String)>, String> {
        self.commits_since(tag, &[to])
    }

    // The commits of `to` itself since `tag`, without those merges brought in.
    pub fn first_parents(&self, tag: Option<&str>, to: &str) -> Result<Vec<String>, String> {
        let range = tag.map_or(to.into(), |t| format!("{t}..{to}"));
        let shas = self.run(&["rev-list", "--first-parent", &range])?;
        Ok(shas.lines().map(str::to_string).collect())
    }

    pub fn head(&self) -> Result<String, String> {
        Ok(self.run(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    // A ref write and nothing else: no editor, no signing program.
    pub fn tag(&self, tag: &str, sha: &str) -> Result<(), String> {
        self.run(&["update-ref", &format!("refs/tags/{tag}"), sha])
            .map(|_| ())
    }

    fn object(&self, kind: &str, id: &str) -> Result<Vec<u8>, String> {
        let mut cmd = git(&["cat-file", kind, id]);
        cmd.current_dir(&self.cwd);
        let out = cmd.output().map_err(|e| format!("git cat-file: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git cat-file {kind} {id}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        if object_id(kind, &out.stdout, id) != id {
            return Err(format!(
                "the {kind} {id} in .git is not what its id says, something rewrote it"
            ));
        }
        Ok(out.stdout)
    }

    // A file as committed in `head`, every object on the way checked against
    // its id: a replace ref, a graft or a rewritten pack in the checkout's
    // .git cannot put other content behind the commit the remote points at.
    pub fn file_at(&self, head: &str, path: &str) -> Result<Vec<u8>, String> {
        let commit = self.object("commit", head)?;
        let tree = String::from_utf8_lossy(&commit)
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("tree "))
            .ok_or_else(|| format!("commit {head} names no tree"))?
            .to_string();
        let mut id = tree;
        for name in path.split('/').filter(|n| !n.is_empty()) {
            let tree = self.object("tree", &id)?;
            id = tree_entry(&tree, name, head.len() / 2)
                .ok_or_else(|| format!("{path} is not committed in {}", &head[..7]))?;
        }
        self.object("blob", &id)
    }

    // The config keys, never their values, that hold a credential any npm
    // script in the checkout could read.
    pub fn stored_credentials(&self) -> Result<Vec<String>, String> {
        let config = self.run(&["config", "--local", "--includes", "--list", "--null"])?;
        Ok(config
            .split('\0')
            .filter_map(|entry| {
                let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
                is_credential(key, value).then(|| key.to_string())
            })
            .collect())
    }
}

// `mode name\0<id>` entries; the id of the one called `name`.
fn tree_entry(tree: &[u8], name: &str, id_len: usize) -> Option<String> {
    let mut at = 0;
    while at < tree.len() {
        let nul = at + tree[at..].iter().position(|b| *b == 0)?;
        let entry = String::from_utf8_lossy(&tree[at..nul]);
        let id = tree.get(nul + 1..nul + 1 + id_len)?;
        if entry.split_once(' ').map(|(_, n)| n) == Some(name) {
            return Some(hex(id));
        }
        at = nul + 1 + id_len;
    }
    None
}

// Where a branch, a tag and the branch a release cuts point on the remote;
// the tag as its commit.
pub struct RemoteRefs {
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub cut: Option<String>,
}

// The remote, reached from an empty repository that borrows the checkout's
// objects: a script can rewrite the checkout's, the user's and, where it is
// the user's, the system git config, so none is read next to the token, a
// header through GIT_CONFIG_*. A proxy or CA for the push comes from the
// environment.
pub struct Remote {
    dir: PathBuf,
    url: String,
    auth: (String, String),
}

impl Remote {
    pub fn new(git: &Git, server_url: &str, url: String, token: &str) -> Result<Remote, String> {
        use std::os::unix::fs::DirBuilderExt;
        let objects = git.run(&["rev-parse", "--git-path", "objects"])?;
        let objects = git.cwd.join(objects.trim());
        let objects = objects
            .canonicalize()
            .map_err(|e| format!("{}: {e}", objects.display()))?;
        let sha256 = git
            .run(&["rev-parse", "--show-object-format"])
            .is_ok_and(|format| format.trim() == "sha256");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("relcut-git-{}-{nanos:x}", std::process::id()));
        let failed = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(failed)?;
        let remote = Remote {
            dir: dir.clone(),
            url,
            auth: (
                format!("http.{server_url}/.extraheader"),
                format!(
                    "AUTHORIZATION: basic {}",
                    base64(format!("x-access-token:{token}").as_bytes())
                ),
            ),
        };
        let config = if sha256 {
            "[core]\n\trepositoryformatversion = 1\n\tbare = true\n[extensions]\n\tobjectformat = sha256\n"
        } else {
            "[core]\n\tbare = true\n"
        };
        std::fs::create_dir_all(dir.join("objects/info")).map_err(failed)?;
        std::fs::create_dir(dir.join("refs")).map_err(failed)?;
        for (file, text) in [
            ("HEAD", "ref: refs/heads/relcut\n".to_string()),
            ("config", config.to_string()),
            (
                "objects/info/alternates",
                format!("{}\n", objects.display()),
            ),
        ] {
            std::fs::write(dir.join(file), text).map_err(failed)?;
        }
        Ok(remote)
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        crate::pin::git()?.unchanged()?;
        let mut cmd = git(args);
        cmd.current_dir(&self.dir)
            .env("GIT_DIR", &self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", &self.auth.0)
            .env("GIT_CONFIG_VALUE_0", &self.auth.1);
        output(cmd, args)
    }

    fn refs_once(&self, branch: &str, tag: &str, cut: Option<&str>) -> Result<RemoteRefs, String> {
        let (branch, tag) = (format!("refs/heads/{branch}"), format!("refs/tags/{tag}"));
        let peeled = format!("{tag}^{{}}");
        let cut = cut.map(|c| format!("refs/heads/{c}"));
        let mut args = vec!["ls-remote", &self.url, &branch, &tag, &peeled];
        args.extend(cut.as_deref());
        let out = self.run(&args)?;
        let find = |name: &str| {
            out.lines().find_map(|line| {
                let (sha, r) = line.split_once('\t')?;
                (r == name).then(|| sha.to_string())
            })
        };
        Ok(RemoteRefs {
            branch: find(&branch),
            tag: find(&peeled).or_else(|| find(&tag)),
            cut: cut.as_deref().and_then(find),
        })
    }

    pub fn refs(&self, branch: &str, tag: &str, cut: Option<&str>) -> Result<RemoteRefs, String> {
        retrying(|| self.refs_once(branch, tag, cut))
    }

    // One atomic push with the branch leased at `sha`: a branch that moved on
    // gets no tag (Ok(false)) and is never changed. git checks the lease
    // against the remote's advertisement; a lost answer counts by its refs.
    // A cut branch is leased as absent, so the push never moves one.
    pub fn push_tag(
        &self,
        branch: &str,
        tag: &str,
        sha: &str,
        cut: Option<(&str, &str)>,
    ) -> Result<bool, String> {
        let lease = format!("--force-with-lease=refs/heads/{branch}:{sha}");
        let branch_ref = format!("{sha}:refs/heads/{branch}");
        let tag_ref = format!("{sha}:refs/tags/{tag}");
        let cut_refs = cut.map(|(name, at)| {
            (
                format!("--force-with-lease=refs/heads/{name}:"),
                format!("{at}:refs/heads/{name}"),
            )
        });
        retrying(|| {
            let mut push = vec!["push", "--atomic", &lease];
            if let Some((cut_lease, _)) = &cut_refs {
                push.push(cut_lease);
            }
            push.extend([self.url.as_str(), &branch_ref, &tag_ref]);
            if let Some((_, cut_ref)) = &cut_refs {
                push.push(cut_ref);
            }
            match self.run(&push) {
                Ok(_) => Ok(true),
                Err(e) => match self.refs_once(branch, tag, None) {
                    Ok(refs) if refs.tag.as_deref() == Some(sha) => Ok(true),
                    Ok(refs) if refs.branch.as_deref() != Some(sha) => Ok(false),
                    _ => Err(e),
                },
            }
        })
    }
}

impl Remote {
    pub fn releases(&self, prefix: &str) -> Result<Vec<(String, Version)>, String> {
        let out = retrying(|| self.run(&["ls-remote", "--tags", "--refs", &self.url]))?;
        let tags = out
            .lines()
            .filter_map(|line| line.split_once('\t')?.1.strip_prefix("refs/tags/"));
        Ok(versions(tags, prefix))
    }

    // Leased as absent, so the push never moves a branch that is there.
    pub fn create_branch(&self, name: &str, sha: &str) -> Result<(), String> {
        let lease = format!("--force-with-lease=refs/heads/{name}:");
        let refspec = format!("{sha}:refs/heads/{name}");
        retrying(|| match self.run(&["push", &lease, &self.url, &refspec]) {
            Ok(_) => Ok(()),
            Err(e) => match self.run(&["ls-remote", &self.url, &format!("refs/heads/{name}")]) {
                Ok(out) if out.split_whitespace().next() == Some(sha) => Ok(()),
                _ => Err(e),
            },
        })
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn is_credential(key: &str, value: &str) -> bool {
    let key = key.to_lowercase();
    let userinfo = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next()?.rsplit_once('@'))
        .is_some_and(|(userinfo, _)| userinfo.contains(':'));
    (key.starts_with("http.") && key.ends_with(".extraheader"))
        || key.starts_with("credential.")
        || userinfo
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_in_git_config() {
        assert!(is_credential(
            "http.https://github.com/.extraheader",
            "AUTHORIZATION: basic abc"
        ));
        assert!(is_credential(
            "remote.origin.url",
            "https://x-access-token:ghs_abc@github.com/o/r.git"
        ));
        assert!(is_credential("credential.helper", "store"));
        assert!(!is_credential(
            "remote.origin.url",
            "https://github.com/o/r.git"
        ));
        assert!(!is_credential(
            "remote.origin.url",
            "ssh://git@github.com/o/r.git"
        ));
        assert!(!is_credential(
            "remote.origin.url",
            "git@github.com:o/r.git"
        ));
        assert!(!is_credential("user.email", "a@b.c"));
    }

    #[test]
    fn base64_encodes_with_padding() {
        assert_eq!(base64(b"x-access-token:abc"), "eC1hY2Nlc3MtdG9rZW46YWJj");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }
}
