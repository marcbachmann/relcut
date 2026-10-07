use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Repo {
    root: PathBuf,
    work: PathBuf,
    // The default branch the event names: the one the repository starts on.
    trunk: String,
    api: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Repo {
    fn new(name: &str, branch: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("relcut-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&root, &["init", "-q", "--bare", "-b", branch, "origin.git"]);
        git(&work, &["init", "-q", "-b", branch]);
        git(&work, &["config", "user.email", "test@example.com"]);
        git(&work, &["config", "user.name", "test"]);
        git(
            &work,
            &[
                "remote",
                "add",
                "origin",
                root.join("origin.git").to_str().unwrap(),
            ],
        );
        Repo {
            root,
            work,
            trunk: branch.to_string(),
            api: github(),
        }
    }

    fn commit(&self, message: &str) -> &Self {
        git(
            &self.work,
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        self
    }

    fn tag(&self, tag: &str) -> &Self {
        git(&self.work, &["tag", tag]);
        self
    }

    fn push(&self, branch: &str) -> &Self {
        git(&self.work, &["push", "-q", "--tags", "origin", branch]);
        self
    }

    // The checkout actions/checkout makes by default: `at` alone, at depth 1,
    // without tags or other branches. The full one stays beside it.
    fn shallow(&self, at: &str) -> &Self {
        let origin = self.root.join("origin.git");
        git(&origin, &["config", "uploadpack.allowFilter", "true"]);
        std::fs::rename(&self.work, self.root.join("full")).unwrap();
        std::fs::create_dir(&self.work).unwrap();
        git(&self.work, &["init", "-q"]);
        let url = format!("file://{}", origin.display());
        git(&self.work, &["remote", "add", "origin", &url]);
        git(
            &self.work,
            &["fetch", "-q", "--no-tags", "--depth=1", "origin", at],
        );
        git(&self.work, &["checkout", "-q", "--detach", "FETCH_HEAD"]);
        git(&self.work, &["config", "user.email", "test@example.com"]);
        git(&self.work, &["config", "user.name", "test"]);
        self
    }

    fn run(&self, command: &str, env: &[(&str, &str)]) -> (Output, String) {
        self.run_with(&[command], env)
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> (Output, String) {
        let out = self.command(args, env).output().unwrap();
        (
            out,
            std::fs::read_to_string(self.root.join("outputs")).unwrap(),
        )
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let outputs = self.root.join("outputs");
        let _ = std::fs::remove_file(&outputs);
        std::fs::write(&outputs, "").unwrap();
        let event = self.root.join("event.json");
        let payload = serde_json::json!({"repository": {"default_branch": self.trunk}});
        std::fs::write(&event, payload.to_string()).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_relcut"));
        cmd.args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", std::env::var("HOME").unwrap())
            .env("GITHUB_OUTPUT", &outputs)
            .env(
                "GITHUB_SERVER_URL",
                format!("file://{}", self.root.display()),
            )
            .env("GITHUB_REPOSITORY", "origin")
            .env("GITHUB_EVENT_NAME", "push")
            .env("GITHUB_EVENT_PATH", &event)
            .env("GITHUB_API_URL", &self.api)
            .env("GITHUB_ACTIONS", "true")
            .env("NO_COLOR", "1")
            .env("RUNNER_TEMP", &self.root)
            .envs(env.iter().copied());
        cmd
    }
}

// A PATH with a `git` in front that runs `script`, with the real git as $real.
fn git_shim(repo: &Repo, script: &str) -> String {
    let real = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|dir| Path::new(dir).join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let bin = repo.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        format!("#!/bin/sh\nreal=\"{}\"\n{script}", real.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn stdout(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn check_reports_the_next_version_on_any_branch() {
    let repo = Repo::new("check", "feat-x");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .commit("feat(api): more")
        .commit("chore: tidy");
    let (out, outputs) = repo.run(
        "check",
        &[("GITHUB_REF_NAME", "feat-x"), ("RELCUT_BRANCHES", "main")],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("3 commits since v1.0.0"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("  minor  ") && stdout(&out).contains("  ·      "),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("\n   v1.1.0 · 3 commits since v1.0.0 · feat-x does not release"),
        "{}",
        stdout(&out)
    );
    assert!(
        outputs.contains("\nv1.1.0\n")
            && outputs.contains("release<<")
            && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
}

#[test]
fn check_fails_outside_the_constraint() {
    let repo = Repo::new("constraint", "release-2026-09");
    repo.commit("feat: first")
        .tag("v312.0.0")
        .commit("feat: not a fix");
    let env = [
        ("GITHUB_REF_NAME", "release-2026-09"),
        ("RELCUT_BRANCHES", "main, release-*"),
        ("RELCUT_CONSTRAINT", "v312.0"),
    ];
    let (out, _) = repo.run("check", &env);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("::error::v312.1.0 is outside v312.0"),
        "{}",
        stdout(&out)
    );

    repo.commit("fix: a fix");
    let (out, _) = repo.run("check", &[env[0], env[1], ("RELCUT_CONSTRAINT", "v312")]);
    assert!(out.status.success(), "{}", stdout(&out));
}

#[test]
fn publish_tags_and_pushes_the_release() {
    let repo = Repo::new("publish", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, outputs) = repo.run("publish", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.0.1\n"), "{outputs}");
    let origin = repo.root.join("origin.git");
    assert_eq!(
        git(&origin, &["rev-parse", "v1.0.1^{commit}"]),
        git(&repo.work, &["rev-parse", "HEAD"])
    );

    let (out, _) = repo.run("publish", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("Tagged v1.0.1") && stdout(&out).contains("already on origin"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["tag"]), "v1.0.0\nv1.0.1");
}

#[test]
fn publish_leaves_a_branch_that_moved_on_to_its_newer_run() {
    let repo = Repo::new("moved", "main");
    repo.commit("fix: first")
        .push("main")
        .commit("fix: second")
        .push("main");
    git(&repo.work, &["reset", "-q", "--hard", "HEAD~1"]);
    let (out, outputs) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("::notice::main moved on")
            && stdout(&out).contains("\n   Left v1.0.0 untagged · main moved on")
            && !stdout(&out).contains("\n   Tagged"),
        "{}",
        stdout(&out)
    );
    let last = outputs.rfind("release<<").unwrap();
    assert!(outputs[last..].contains("\nfalse\n"), "{outputs}");
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

// Releases "fix: first" while origin's main moves to `to` ("fix: second" is
// HEAD, "fix: zero" HEAD~2) right after relcut's check, as a push landing
// between the check and the tag would: a git on PATH answers ls-remote, then
// moves main.
fn publish_racing_a_push(name: &str, to: &str) -> (Repo, Output) {
    let repo = Repo::new(name, "main");
    repo.commit("fix: zero")
        .commit("fix: first")
        .push("main")
        .commit("fix: second");
    git(
        &repo.work,
        &["push", "-q", "origin", "HEAD:refs/heads/next"],
    );
    let target = git(&repo.work, &["rev-parse", to]);
    git(&repo.work, &["reset", "-q", "--hard", "HEAD~1"]);
    let path = git_shim(
        &repo,
        &format!(
            "\"$real\" \"$@\"; status=$?\ncase \" $* \" in *\" ls-remote \"*) \"$real\" --git-dir \"{}\" update-ref refs/heads/main {target} ;; esac\nexit $status\n",
            repo.root.join("origin.git").display(),
        ),
    );
    let (out, _) = repo.run(
        "publish",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "t"),
            ("PATH", &path),
        ],
    );
    (repo, out)
}

#[test]
fn publish_tags_nothing_when_the_branch_moves_on_after_its_check() {
    let (repo, out) = publish_racing_a_push("raced", "HEAD");
    let origin = repo.root.join("origin.git");
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("the push lost to a newer commit"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("::notice::main moved on"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["tag"]), "");
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        git(&origin, &["rev-parse", "next"])
    );
}

#[test]
fn publish_never_moves_a_branch_that_was_reset_after_its_check() {
    let (repo, out) = publish_racing_a_push("reset", "HEAD~2");
    let origin = repo.root.join("origin.git");
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("::notice::main moved on"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["tag"]), "");
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        git(&repo.work, &["rev-parse", "HEAD~1"])
    );
    assert!(
        stdout(&out).contains("the push lost to a newer commit"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn prepare_sets_the_version_and_packs_without_credentials() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("prepare", "main");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/pkg", "version": "0.0.0-placeholder", "scripts": {"prepack": "node -e \"require('fs').writeFileSync('built', Object.keys(process.env).filter(k => /token|secret|password|_auth/i.test(k)).sort().join(','))\""}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/pkg", "version": "0.0.0-placeholder", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/pkg", "version": "0.0.0-placeholder"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("feat: first");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
        ("RELCUT_NPM_TOKEN", "secret"),
        ("NODE_AUTH_TOKEN", "secret"),
        ("MY_REGISTRY_TOKEN", "allowed"),
        ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://example.com"),
        ("GITHUB_RUN_ID", "777"),
    ];
    let (out, outputs) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let pkg = std::fs::read_to_string(repo.work.join("package.json")).unwrap();
    assert!(pkg.contains(r#""version": "1.0.0""#), "{pkg}");
    let ci = serde_json::from_str::<serde_json::Value>(&pkg).unwrap()["ci"].clone();
    assert!(ci["slug"].is_null(), "{ci}");
    assert!(ci["directory"].is_null() && ci["path"].is_null(), "{ci}");
    assert_eq!(
        ci["repository"],
        format!("file://{}/origin", repo.root.display()),
        "{ci}"
    );
    assert_eq!(ci["branch"], "main", "{ci}");
    assert_eq!(ci["tag"], "v1.0.0", "{ci}");
    assert_eq!(
        ci["commit"],
        git(&repo.work, &["rev-parse", "HEAD"]),
        "{ci}"
    );
    assert_eq!(
        ci["buildUrl"],
        format!("file://{}/origin/actions/runs/777", repo.root.display()),
        "{ci}"
    );
    let date = ci["date"].as_str().unwrap();
    assert!(date.len() == 20 && date.ends_with('Z'), "{date}");
    assert_eq!(
        std::fs::read_to_string(repo.work.join("built")).unwrap(),
        ""
    );
    git(&repo.work, &["checkout", "--", "package.json"]);

    let (out, _) = repo.run_with(&["prepare", "--pass-env", "MY_REGISTRY_*"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(
        std::fs::read_to_string(repo.work.join("built")).unwrap(),
        "MY_REGISTRY_TOKEN"
    );
    assert!(
        repo.root.join("relcut/x-pkg-1.0.0.tgz").exists(),
        "{outputs}"
    );
}

#[test]
fn check_outputs_the_notes_and_publish_takes_edited_ones() {
    let repo = Repo::new("notes", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("feat(api): add a thing")
        .push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "github"),
        ("RELCUT_RELEASE_NOTES_TEMPLATE", "# {tag}\n\n{notes}"),
    ];
    let (out, outputs) = repo.run("check", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("notes<<"), "{outputs}");
    assert!(
        outputs.contains("# v1.1.0\n\n### ✨\u{a0} Features\n\n- **api:** add a thing"),
        "{outputs}"
    );
    assert!(
        outputs.contains("**Full changelog**: [`v1.0.0...v1.1.0`]"),
        "{outputs}"
    );

    let edited = [
        env[0],
        env[1],
        env[2],
        ("RELCUT_GITHUB_TOKEN", "t"),
        ("RELCUT_DRY_RUN", "true"),
        ("RELCUT_RELEASE_NOTES", "Hand written"),
    ];
    let (out, _) = repo.run("publish", &edited);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out)
            .contains("would create v1.1.0, 'v1.1.0', with these notes:\n\n    Hand written\n")
            && stdout(&out).contains("\n    Hand written\n"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn flags_win_over_the_environment() {
    let repo = Repo::new("flags", "main");
    repo.commit("feat: first");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "release-*"),
        ("RELCUT_TAG_PREFIX", "x"),
    ];
    let (out, outputs) = repo.run_with(&["check", "--branches", "main", "--tag-prefix=v"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        !stdout(&out).contains("does not release"),
        "{}",
        stdout(&out)
    );
    assert!(
        outputs.contains("\nv1.0.0\n") && outputs.contains("\ntrue\n"),
        "{outputs}"
    );
}

#[test]
fn publish_npm_alone_tags_and_makes_the_github_release_too() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("npm-only", "main");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/npm-only", "version": "0.0.0-placeholder", "repository": {"url": "git+https://github.com/elsewhere/npm-only.git"}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/npm-only", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/npm-only"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let (out, _) = repo.run_with(&["prepare", "--branches=main", "--publish=npm"], &env);
    assert!(out.status.success(), "{}", stdout(&out));

    // Trusted publishing: npm's provenance names the repository the build ran
    // in, and refuses a manifest that names another one, after the tag.
    let args = [
        "publish",
        "--branches=main",
        "--publish=npm",
        "--npm-tag=next",
        "--dry-run",
    ];
    let (out, _) = repo.run_with(&args, &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("trusted publishing needs repository.url in package.json naming origin, not elsewhere/npm-only"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/npm-only", "version": "0.0.0-placeholder", "repository": "git+ssh://git@github.com/Origin.git"}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("fix: repository").push("main");
    let (out, _) = repo.run_with(&["prepare", "--branches=main", "--publish=npm"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let (out, _) = repo.run_with(&args, &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("── Publish ─")
            && log.contains("\n   Would tag v1.0.0 · ")
            && log.contains(
                "\n   Would publish to registry.npmjs.org · @x/npm-only@1.0.0 on next · "
            )
            && log.contains("\n   Would create the GitHub release · v1.0.0 · ")
            && !log.contains("dry run · "),
        "{log}"
    );
    assert!(log.contains("@x/npm-only@1.0.0"), "{log}");
    let footer = log.rsplit("────\n").nth(1).unwrap_or_default();
    assert!(
        footer.contains("\n   Dry run of v1.0.0, nothing published · npm, github · ")
            && footer.contains(
                "\n   npm         https://www.npmjs.com/package/@x/npm-only/v/1.0.0\n   Registry    registry.npmjs.org\n   Dist-tags   next\n"
            ),
        "{log}"
    );
    assert!(log.contains("would create v1.0.0"), "{log}");
}

fn npm_package(repo: &Repo, name: &str) {
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "{name}", "version": "0.0.0-placeholder", "repository": "https://github.com/origin"}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        format!(r#"{{"name": "{name}", "lockfileVersion": 3, "requires": true, "packages": {{"": {{"name": "{name}"}}}}}}"#),
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
}

#[test]
fn github_packages_refuses_another_scope_before_tagging() {
    let repo = Repo::new("gpr-scope", "main");
    npm_package(&repo, "@acme/server");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@acme/server", "publishConfig": {"registry": "https://npm.pkg.github.com/"}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let (out, _) = repo.run_with(&["publish", "--branches=main", "--publish=npm"], &env);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains(
            "GitHub Packages takes origin's packages as @origin/<name>, not @acme/server"
        ),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

#[test]
fn publish_takes_no_github_packages_target() {
    let repo = Repo::new("gpr-target", "main");
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let (out, _) = repo.run_with(
        &["publish", "--branches=main", "--publish=github-packages"],
        &env,
    );
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains(
            "publish takes npm for GitHub Packages too: set publishConfig.registry in package.json to https://npm.pkg.github.com"
        ),
        "{}",
        stdout(&out)
    );
}

// The scope registry is the one npm publishes to, over publishConfig.registry,
// and the GitHub token goes with it.
#[test]
fn github_packages_publishes_the_tarball_to_its_registry() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("gpr", "main");
    npm_package(&repo, "@origin/pkg");
    let mut pkg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo.work.join("package.json")).unwrap()).unwrap();
    pkg["publishConfig"] = serde_json::json!({
        "registry": "https://registry.npmjs.org/",
        "@origin:registry": "https://npm.pkg.github.com/"
    });
    std::fs::write(repo.work.join("package.json"), pkg.to_string()).unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let flags = ["--branches=main", "--publish=npm", "--npm-tag=next"];
    let (out, _) = repo.run_with(&[&["prepare"], &flags[..]].concat(), &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let (out, _) = repo.run_with(&[&["publish", "--dry-run"], &flags[..]].concat(), &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("Publishing to https://npm.pkg.github.com/ with tag next"),
        "{log}"
    );
    assert!(!log.contains("registry.npmjs.org"), "{log}");
}

// A staged version is not on the registry until its approval: one dist-tag,
// a registry with a stage queue and an npm that has `npm stage`, all before
// the tag.
#[test]
fn npm_stage_stages_the_tarball_or_refuses_before_tagging() {
    let Ok(npm) = Command::new("npm").arg("--version").output() else {
        return;
    };
    let version: Vec<u64> = String::from_utf8_lossy(&npm.stdout)
        .trim()
        .split('.')
        .map_while(|p| p.parse().ok())
        .collect();
    let stages = version[0] >= 12 || (version[0] == 11 && version[1] >= 15);
    let (registry, seen) = registry();
    let repo = Repo::new("npm-stage", "main");
    npm_package(&repo, "@origin/staged");
    let mut pkg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo.work.join("package.json")).unwrap()).unwrap();
    pkg["publishConfig"] = serde_json::json!({ "registry": registry });
    std::fs::write(repo.work.join("package.json"), pkg.to_string()).unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let flags = ["--branches=main", "--publish=npm", "--npm-stage"];
    let (out, _) = repo.run_with(&[&["prepare"], &flags[..2]].concat(), &env);
    assert!(out.status.success(), "{}", stdout(&out));

    let (out, _) = repo.run_with(
        &[&["publish", "--npm-tag=next,beta"], &flags[..]].concat(),
        &env,
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("a staged version takes one dist-tag, set with its approval, not next, beta"),
        "{log}"
    );

    let one = [&["publish", "--npm-tag=next"], &flags[..]].concat();
    if stages {
        let (out, _) = repo.run_with(&[&one[..], &["--dry-run"]].concat(), &env);
        let log = stdout(&out);
        assert!(out.status.success(), "{log}");
        assert!(log.contains("$ npm stage publish "), "{log}");
        assert!(
            log.contains(&format!("Staging to {registry} with tag next")),
            "{log}"
        );
    } else {
        let (out, _) = repo.run_with(&one, &env);
        let log = stdout(&out);
        assert!(!out.status.success(), "{log}");
        assert!(log.contains("has no npm stage, it needs 11.15.0"), "{log}");
    }
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().all(|(line, _)| !line.starts_with("PUT")),
        "{seen:?}"
    );
}

// prepare stamps ci.commit into the tarball's package.json, so the registry
// names the commit a version came from: this one's is not published again,
// any other stops the release before the tag.
#[test]
fn a_version_on_the_registry_is_this_commits_or_stops_the_release() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let published_from = |name: &str, commit: Option<&str>| {
        let packument = Packument::default();
        let (registry, seen) = registry_with(packument.clone());
        let repo = Repo::new(name, "main");
        npm_package(&repo, "@origin/there");
        let mut pkg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(repo.work.join("package.json")).unwrap())
                .unwrap();
        pkg["publishConfig"] = serde_json::json!({ "registry": registry });
        std::fs::write(repo.work.join("package.json"), pkg.to_string()).unwrap();
        git(&repo.work, &["add", "package.json"]);
        repo.commit("feat: first").push("main");
        let head = git(&repo.work, &["rev-parse", "HEAD"]);
        let mut manifest = serde_json::json!({"name": "@origin/there", "version": "1.0.0"});
        if let Some(commit) = commit {
            manifest["ci"] = serde_json::json!({"commit": commit.replace("HEAD", &head)});
        }
        let there = serde_json::json!({
            "name": "@origin/there",
            "dist-tags": {"latest": "1.0.0"},
            "versions": {"1.0.0": manifest},
        });
        *packument.lock().unwrap() = Some(there.to_string());
        let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
        let flags = ["--branches=main", "--publish=npm"];
        let (out, _) = repo.run_with(&[&["prepare"], &flags[..]].concat(), &env);
        assert!(out.status.success(), "{}", stdout(&out));
        let (out, _) = repo.run_with(&[&["publish"], &flags[..]].concat(), &env);
        let puts = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, _)| line.starts_with("PUT"))
            .count();
        let tags = git(&repo.root.join("origin.git"), &["tag"]);
        (out.status.success(), stdout(&out), head, puts, tags)
    };

    let other = "1111111111111111111111111111111111111111";
    let (ok, log, head, puts, tags) = published_from("there-other", Some(other));
    assert!(!ok, "{log}");
    assert!(
        log.contains("@origin/there@1.0.0 is on 127.0.0.1")
            && log.contains(&format!(
                "already, published from commit {other}, not {head}"
            )),
        "{log}"
    );
    assert_eq!((puts, tags.as_str()), (0, ""), "{log}");

    let (ok, log, _, puts, tags) = published_from("there-unstamped", None);
    assert!(!ok, "{log}");
    assert!(
        log.contains("it has no ci.commit, which prepare writes"),
        "{log}"
    );
    assert_eq!((puts, tags.as_str()), (0, ""), "{log}");

    let (ok, log, _, puts, tags) = published_from("there-this", Some("HEAD"));
    assert!(ok, "{log}");
    assert!(
        log.contains("@origin/there@1.0.0 is there already"),
        "{log}"
    );
    assert_eq!((puts, tags.as_str()), (0, "v1.0.0"), "{log}");
}

#[test]
fn github_packages_stages_nothing() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("gpr-stage", "main");
    npm_package(&repo, "@origin/pkg");
    let mut pkg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo.work.join("package.json")).unwrap()).unwrap();
    pkg["publishConfig"] = serde_json::json!({"registry": "https://npm.pkg.github.com/"});
    std::fs::write(repo.work.join("package.json"), pkg.to_string()).unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let flags = ["--branches=main", "--publish=npm"];
    let (out, _) = repo.run_with(&[&["prepare"], &flags[..]].concat(), &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let (out, _) = repo.run_with(&[&["publish", "--npm-stage"], &flags[..]].concat(), &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("GitHub Packages has no stage queue"), "{log}");
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

#[test]
fn outside_of_actions_publish_asks_and_refuses_without_a_terminal() {
    let repo = Repo::new("local", "main");
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_ACTIONS", "false"),
        ("GITHUB_REF_NAME", ""),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run_with(&["publish", "--branches=main"], &env);
    assert!(!out.status.success());
    assert!(stdout(&out).contains("Publish v1.0.0 of origin from main (github)? Not in GitHub Actions and no terminal to ask: pass --yes"), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");

    let (out, _) = repo.run_with(&["publish", "--branches=main", "--yes"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
}

#[test]
fn publish_config_tag_registry_and_access_apply_unless_flags_say_otherwise() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (registry, _) = registry();
    let repo = Repo::new("publish-config", "main");
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@origin/pc", "version": "0.0.0-placeholder", "scripts": {{"prepack": "node -e \"require('fs').appendFileSync('package-lock.json', ' ')\""}}, "repository": "https://github.com/origin", "publishConfig": {{"tag": "beta", "access": "restricted", "registry": "{registry}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@origin/pc", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@origin/pc"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("feat: first").push("main");
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let (out, _) = repo.run_with(&["prepare", "--branches=main", "--publish=npm"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("::notice::npm pack changed package-lock.json"),
        "{}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("changed package.json"),
        "{}",
        stdout(&out)
    );

    let (out, _) = repo.run_with(
        &["publish", "--branches=main", "--publish=npm", "--dry-run"],
        &env,
    );
    assert!(
        stdout(&out).contains(&format!(
            "Publishing to {registry} with tag beta and restricted access"
        )),
        "{}",
        stdout(&out)
    );
    assert!(
        repo.root.join("relcut/origin-pc-1.0.0.tgz").exists(),
        "a dry run keeps the tarball"
    );

    let (out, _) = repo.run_with(
        &[
            "publish",
            "--branches=main",
            "--publish=npm",
            "--npm-tag=next",
            "--dry-run",
        ],
        &env,
    );
    assert!(
        stdout(&out).contains("with tag next and restricted access"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn only_prefixed_variables_configure_relcut() {
    let repo = Repo::new("prefix", "main");
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("GITHUB_TOKEN", "from-another-step"),
    ];
    let (out, _) = repo.run("publish", &env);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("publish needs github-token"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn publish_runs_no_git_hook_of_the_repository() {
    let repo = Repo::new("hooks", "main");
    repo.commit("feat: first").push("main");
    let hooks = repo.root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let marker = repo.root.join("hook-ran");
    let hook = hooks.join("pre-push");
    std::fs::write(&hook, format!("#!/bin/sh\nenv > {}\n", marker.display())).unwrap();
    Command::new("chmod").arg("+x").arg(&hook).status().unwrap();
    git(
        &repo.work,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );

    let (out, _) = repo.run(
        "publish",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
    assert!(!marker.exists(), "the pre-push hook ran");
}

// What ps and /proc show of relcut's environment to the processes it starts.
#[test]
fn no_child_finds_a_token_in_the_environment_of_relcut() {
    let repo = Repo::new("sealed", "main");
    repo.commit("feat: first").push("main");
    let seen = repo.root.join("seen");
    let path = git_shim(
        &repo,
        &format!(
            "if [ -r /proc/$PPID/environ ]; then tr '\\0' '\\n' < /proc/$PPID/environ; else ps eww -o command= -p $PPID; fi >> \"{seen}\" 2>&1\nenv >> \"{seen}\"\nexec \"$real\" \"$@\"\n",
            seen = seen.display()
        ),
    );
    let (out, _) = repo.run(
        "publish",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "ghs-sealed-1"),
            ("RELCUT_NPM_TOKEN", "npm-sealed-2"),
            ("SOME_API_SECRET", "other-sealed-3"),
            ("PATH", &path),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
    let seen = std::fs::read_to_string(seen).unwrap();
    assert!(seen.contains("PATH="), "{seen}");
    for value in ["ghs-sealed-1", "npm-sealed-2", "other-sealed-3"] {
        assert!(!seen.contains(value), "{value} in {seen}");
    }
}

// What `setpriv --no-new-privs` sets: npm and its scripts cannot gain
// privileges, so sudo is no way to read what relcut keeps from them.
#[cfg(target_os = "linux")]
#[test]
fn npm_and_its_scripts_never_gain_privileges() {
    let repo = Repo::new("confined", "main");
    npm_package(&repo, "@x/pkg");
    repo.commit("feat: first");
    let seen = repo.root.join("seen");
    let path = node_install(
        &repo,
        "#!/bin/sh\ncase \"$1\" in -e) printf %s \"$0\" ;; *) exec sh \"$@\" ;; esac\n",
        &format!(
            "#!/bin/sh\ngrep NoNewPrivs /proc/self/status >> \"{}\"\nif [ \"$1\" = pack ]; then : > \"$4/x-pkg-1.0.0.tgz\"; fi\n",
            seen.display()
        ),
    );
    let (out, _) = repo.run(
        "prepare",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("PATH", &path),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let seen = std::fs::read_to_string(seen).unwrap();
    assert_eq!(seen.lines().count(), 2, "{seen}");
    assert!(
        seen.lines()
            .all(|l| l.split_whitespace().last() == Some("1")),
        "{seen}"
    );
}

// prefix/bin/{node,npm} and prefix/lib/node_modules/npm, as node ships them,
// with these scripts as node and as npm's cli. A PATH with it in front.
fn node_install(repo: &Repo, node: &str, npm_cli: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let prefix = repo.root.join("prefix");
    let package = prefix.join("lib/node_modules/npm");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::write(package.join("package.json"), r#"{"name": "npm"}"#).unwrap();
    for (file, script) in [
        (prefix.join("bin/node"), node),
        (package.join("bin/npm-cli.js"), npm_cli),
    ] {
        std::fs::write(&file, script).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(
        "../lib/node_modules/npm/bin/npm-cli.js",
        prefix.join("bin/npm"),
    )
    .unwrap();
    format!(
        "{}:{}",
        prefix.join("bin").display(),
        std::env::var("PATH").unwrap()
    )
}

// A copy of the npm on the PATH, first on the PATH, to change without
// touching the real one. None only where there is no npm at all.
fn npm_copy(repo: &Repo) -> Option<(String, PathBuf)> {
    Command::new("npm").arg("--version").output().ok()?;
    let is_npm = |dir: &Path| {
        std::fs::read_to_string(dir.join("package.json"))
            .is_ok_and(|text| text.contains(r#""name": "npm""#))
    };
    let which = |name: &str| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    };
    let node = String::from_utf8(
        Command::new("node")
            .args(["-p", "process.execPath"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let beside = |node: &Path| {
        node.parent()?
            .parent()
            .map(|p| p.join("lib/node_modules/npm"))
    };
    let real = which("npm")
        .and_then(|npm| npm.canonicalize().ok())
        .and_then(|cli| cli.ancestors().find(|d| is_npm(d)).map(Path::to_path_buf))
        .or_else(|| which("node").and_then(|n| beside(&n)).filter(|d| is_npm(d)))
        .or_else(|| beside(Path::new(node.trim())).filter(|d| is_npm(d)))
        .expect("npm runs, but its package is nowhere to be found");
    let prefix = repo.root.join("npm-prefix");
    std::fs::create_dir_all(prefix.join("lib/node_modules")).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let package = prefix.join("lib/node_modules/npm");
    let copied = Command::new("cp")
        .arg("-R")
        .arg(&real)
        .arg(&package)
        .status()
        .unwrap();
    assert!(copied.success());
    let writable = Command::new("chmod")
        .args(["-R", "u+w"])
        .arg(&package)
        .status()
        .unwrap();
    assert!(writable.success());
    std::os::unix::fs::symlink(
        "../lib/node_modules/npm/bin/npm-cli.js",
        prefix.join("bin/npm"),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        prefix.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    Some((path, package))
}

fn scripted_package(repo: &Repo, scripts: &str) {
    let (registry, _) = registry();
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/pkg", "version": "0.0.0-placeholder", "scripts": {scripts}, "publishConfig": {{"registry": "{registry}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/pkg", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/pkg"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
}

// A script runs as the job's user, and node and npm usually sit in the
// runner's tool cache, which that user can write.
#[test]
fn npm_that_a_script_changed_gets_no_token() {
    let repo = Repo::new("npm-changed", "main");
    let Some((path, _)) = npm_copy(&repo) else {
        return;
    };
    scripted_package(
        &repo,
        r#"{"prepack": "echo '// changed' >> \"$npm_execpath\""}"#,
    );
    repo.commit("feat: first").push("main");
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret"),
            ("RELCUT_GITHUB_TOKEN", "t"),
            ("PATH", &path),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains(
            "npm gets no token: npm/bin/npm-cli.js changed while the package's scripts ran"
        ),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

#[test]
fn publish_holds_npm_to_what_prepare_fingerprinted() {
    let repo = Repo::new("npm-between", "main");
    let Some((path, npm)) = npm_copy(&repo) else {
        return;
    };
    scripted_package(&repo, "{}");
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
        ("RELCUT_NPM_TOKEN", "npm-secret"),
        ("RELCUT_GITHUB_TOKEN", "t"),
        ("PATH", &path),
    ];
    let (out, _) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("\n   node    v"), "{}", stdout(&out));
    let (out, _) = repo.run_with(&["publish", "--dry-run"], &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("$ npm publish "), "{}", stdout(&out));

    let cli = npm.join("lib/cli.js");
    let mut text = std::fs::read_to_string(&cli).unwrap();
    text.push_str("\n// changed between the steps\n");
    std::fs::write(&cli, text).unwrap();
    let (out, _) = repo.run("publish", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("npm gets no token: a file of node or npm changed since prepare"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

// An npm registry that takes every publish, and keeps what it was sent as
// ("METHOD /path", "user-agent · authorization").
fn registry() -> (String, Requests) {
    registry_with(Packument::default())
}

type Packument = std::sync::Arc<std::sync::Mutex<Option<String>>>;

// A registry that answers every GET with the packument, while it has one.
fn registry_with(packument: Packument) -> (String, Requests) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let requests = Requests::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut length, mut agent, mut auth) = (0, String::new(), String::new());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if let Some(v) = lower.strip_prefix("user-agent:") {
                    agent = v.trim().to_string();
                }
                if let Some(v) = line.strip_prefix("authorization:") {
                    auth = v.trim().to_string();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let method = request.split_whitespace().next().unwrap_or_default();
            let (status, body) = match (method, packument.lock().unwrap().clone()) {
                ("PUT", _) => (200, "{}".to_string()),
                ("GET", Some(packument)) => (200, packument),
                _ => (404, "{}".to_string()),
            };
            let line = request.trim().trim_end_matches(" HTTP/1.1").to_string();
            seen.lock()
                .unwrap()
                .push((line, format!("{agent} · {auth}")));
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    (url, requests)
}

// A postpack script rewrites the registry in package.json, writes a project
// .npmrc and plants a .npmrc above the private directory; a planted
// NODE_OPTIONS names code for every node to load. npm publish takes none of
// it: the committed registry, a directory of its own and a fixed environment.
#[test]
fn npm_publish_takes_nothing_a_script_left_behind() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (committed, seen_by_committed) = registry();
    let (planted, seen_by_planted) = registry();
    let repo = Repo::new("publish-isolated", "main");
    let marker = repo.root.join("loaded");
    std::fs::write(
        repo.root.join("hook.js"),
        format!(
            "require('fs').appendFileSync('{}', 'token=' + process.env.NPM_TOKEN + '\\n')",
            marker.display()
        ),
    )
    .unwrap();
    let planted_host = planted.trim_start_matches("http:").trim_end_matches('/');
    let above = repo.root.join("tmp");
    std::fs::create_dir_all(&above).unwrap();
    std::fs::write(above.join("package.json"), "{}").unwrap();
    let npmrc = format!(
        "@x:registry={planted}\nproxy={planted}\n{planted_host}/:_authToken=${{NPM_TOKEN}}\nuser-agent=planted-by-a-script\n"
    );
    std::fs::write(above.join(".npmrc"), &npmrc).unwrap();
    std::fs::write(
        repo.work.join("swap.js"),
        format!(
            r#"const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json"));
pkg.publishConfig = {{registry: "{planted}"}};
fs.writeFileSync("package.json", JSON.stringify(pkg));
fs.writeFileSync(".npmrc", {npmrc:?});
"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/isolated", "version": "0.0.0-placeholder", "scripts": {{"postpack": "node swap.js"}}, "publishConfig": {{"registry": "{committed}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/isolated", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/isolated"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let hook = format!("--require={}", repo.root.join("hook.js").display());
    let (out, _) = repo.run_with(
        &["release", "--side-effects=warn"],
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret-isolated"),
            ("RELCUT_GITHUB_TOKEN", "t"),
            ("NODE_OPTIONS", &hook),
            ("TMPDIR", above.to_str().unwrap()),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    // One process: what prepare showed, publish does not show again.
    assert_eq!(log.matches("\n   user    ").count(), 1, "{log}");
    assert!(
        log.contains("\n   Ready to tag v1.0.0 · ") && !log.contains("unchanged since"),
        "{log}"
    );
    let published = requests(&seen_by_committed);
    assert!(
        published.iter().any(|(line, headers)| {
            line.starts_with("PUT ")
                && headers.starts_with("npm/")
                && headers.ends_with("· Bearer npm-secret-isolated")
        }),
        "{published:?}"
    );
    assert_eq!(requests(&seen_by_planted), [], "{log}");
    let loaded = std::fs::read_to_string(&marker).unwrap();
    assert!(
        loaded.contains("token=undefined") && !loaded.contains("npm-secret-isolated"),
        "{loaded}"
    );
}

// npm publish takes every config key from the tarball's own publishConfig,
// which a prepack script writes: a scoped registry or a proxy there decides
// where the token goes, whatever --registry says.
#[test]
fn a_publish_config_planted_into_the_tarball_is_refused_before_the_tag() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (committed, _) = registry();
    let (planted, seen_by_planted) = registry();
    let repo = Repo::new("tarball-config", "main");
    std::fs::write(
        repo.work.join("plant.js"),
        format!(
            r#"const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json"));
pkg.publishConfig = {{"@x:registry": "{planted}", "proxy": "{planted}"}};
fs.writeFileSync("package.json", JSON.stringify(pkg));
"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/tarball", "version": "0.0.0-placeholder", "scripts": {{"prepack": "node plant.js"}}, "publishConfig": {{"registry": "{committed}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/tarball", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/tarball"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let (out, _) = repo.run_with(
        &["release", "--side-effects=warn"],
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret-tarball"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("npm gets no token: the tarball's publishConfig is not the committed one"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
    assert_eq!(requests(&seen_by_planted), [], "{log}");
}

// npm publishes under the name inside the tarball, whatever relcut passes.
#[test]
fn a_tarball_with_another_name_is_refused_before_the_tag() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (committed, seen) = registry();
    let repo = Repo::new("tarball-name", "main");
    std::fs::write(
        repo.work.join("rename.js"),
        r#"const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json"));
pkg.name = "@x/another";
fs.writeFileSync("package.json", JSON.stringify(pkg));
"#,
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/named", "version": "0.0.0-placeholder", "scripts": {{"prepack": "node rename.js"}}, "publishConfig": {{"registry": "{committed}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/named", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/named"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let (out, _) = repo.run_with(
        &["release", "--side-effects=warn"],
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret-named"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains(r#"npm gets no token: the tarball's name is "@x/another", not @x/named"#),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
    assert_eq!(requests(&seen), [], "{log}");
}

// The checkout's .git is the job user's: a replace ref, and a rewritten
// object behind the commit the remote points at, must not choose the
// registry. Only the objects whose ids add up to HEAD count.
#[test]
fn the_committed_manifest_is_the_one_behind_the_commit_hash() {
    let (committed, _) = registry();
    let (planted, _) = registry();
    let repo = Repo::new("forged-head", "main");
    npm_package(&repo, "@x/forged");
    let manifest = format!(
        r#"{{"name": "@x/forged", "publishConfig": {{"registry": "{committed}", "@x:registry": "https://npm.pkg.github.com"}}}}"#
    );
    std::fs::write(repo.work.join("package.json"), &manifest).unwrap();
    git(&repo.work, &["add", "package.json"]);
    repo.commit("feat: first").push("main");
    let head = git(&repo.work, &["rev-parse", "HEAD"]);
    let blob = git(&repo.work, &["rev-parse", "HEAD:package.json"]);
    let forged = manifest
        .replace(&committed, &planted)
        .replace("@x/forged", "@x/forged-by-replace");
    std::fs::write(repo.work.join("package.json"), &forged).unwrap();
    git(&repo.work, &["add", "package.json"]);
    let tree = git(&repo.work, &["write-tree"]);
    let other = git(
        &repo.work,
        &["commit-tree", &tree, "-p", &head, "-m", "forged"],
    );
    git(&repo.work, &["replace", &head, &other]);
    git(&repo.work, &["checkout", "-q", "--", "package.json"]);
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
        ("RELCUT_NPM_TOKEN", "npm-secret-forged"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run("publish", &env);
    let log = stdout(&out);
    assert!(
        log.contains("as @origin/<name>, not @x/forged\n") || log.contains("not @x/forged%0A"),
        "{log}"
    );
    assert_eq!(git(&repo.work, &["rev-parse", "HEAD"]), head);

    let path = repo
        .work
        .join(format!(".git/objects/{}/{}", &blob[..2], &blob[2..]));
    let mut forged_object = format!("blob {}\0", forged.len()).into_bytes();
    forged_object.extend_from_slice(forged.as_bytes());
    use std::io::Write;
    let mut zipped = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zipped.write_all(&forged_object).unwrap();
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, zipped.finish().unwrap()).unwrap();
    git(&repo.work, &["replace", "-d", &head]);
    let (out, _) = repo.run("publish", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains(&format!(
            "the blob {blob} in .git is not what its id says, something rewrote it"
        )),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

// Every fact relcut has about the repository, and the push token, go
// through git. A prepack script puts its own `git` first on the PATH;
// relcut keeps running the one it found when it started.
#[test]
fn a_git_planted_on_the_path_by_a_script_is_never_run() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (committed, seen) = registry();
    let repo = Repo::new("planted-git", "main");
    let bin = repo.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let marker = repo.root.join("planted-git-ran");
    let real = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|dir| Path::new(dir).join("git"))
        .find(|p| p.is_file())
        .unwrap();
    std::fs::write(
        repo.work.join("plant.js"),
        format!(
            r##"const fs = require("fs");
fs.writeFileSync("{bin}/git", "#!/bin/sh\necho ran >> {marker}\nexec {real} \"$@\"\n", {{mode: 0o755}});
"##,
            bin = bin.display(),
            marker = marker.display(),
            real = real.display()
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/planted-git", "version": "0.0.0-placeholder", "scripts": {{"prepack": "node plant.js"}}, "publishConfig": {{"registry": "{committed}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/planted-git", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/planted-git"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret-git"),
            ("RELCUT_GITHUB_TOKEN", "ghs-secret-git"),
            ("PATH", &path),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(bin.join("git").is_file(), "{log}");
    assert!(!marker.exists(), "the planted git ran: {log}");
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
    assert!(
        requests(&seen)
            .iter()
            .any(|(line, _)| line.starts_with("PUT ")),
        "{log}"
    );
}

// git's helpers are part of its fingerprint: one that a script changed
// after relcut started gets no token.
#[test]
fn a_git_helper_changed_by_a_script_gets_no_token() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (committed, _) = registry();
    let repo = Repo::new("git-helper", "main");
    let helpers = repo.root.join("helpers");
    std::fs::create_dir_all(&helpers).unwrap();
    std::fs::write(helpers.join("git-remote-https"), "a helper").unwrap();
    let path = git_shim(
        &repo,
        &format!(
            "if [ \"$1\" = --exec-path ]; then echo \"{}\"; exit 0; fi\nexec \"$real\" \"$@\"\n",
            helpers.display()
        ),
    );
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/helper", "version": "0.0.0-placeholder", "scripts": {{"prepack": "node -e \"require('fs').appendFileSync('{}', ' changed')\""}}, "publishConfig": {{"registry": "{committed}"}}}}"#,
            helpers.join("git-remote-https").display()
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/helper", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/helper"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "npm-secret-helper"),
            ("RELCUT_GITHUB_TOKEN", "ghs-secret-helper"),
            ("PATH", &path),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("Prepared v1.0.0"), "{log}");
    assert!(
        log.contains("git gets no token: a file of git changed since relcut started"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

// Reads a file's size after relcut returned and again a little later: a
// process a script left running would still be writing to it.
fn stopped_growing(file: &Path) -> bool {
    let size = || std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let before = size();
    std::thread::sleep(std::time::Duration::from_millis(400));
    before == size() && before > 0
}

// A prepack script leaves a loop running in the background, its standard
// streams closed so npm returns; nothing a script started outlives prepare.
#[test]
fn no_process_a_script_started_outlives_prepare() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("leftover", "main");
    let ticks = repo.root.join("ticks");
    std::fs::write(
        repo.work.join("linger.sh"),
        format!(
            "echo tick >> \"{0}\"\n( i=0; while [ $i -lt 400 ]; do echo tick >> \"{0}\"; i=$((i+1)); sleep 0.02; done ) </dev/null >/dev/null 2>&1 &\n",
            ticks.display()
        ),
    )
    .unwrap();
    scripted_package(&repo, r#"{"prepack": "sh linger.sh"}"#);
    git(&repo.work, &["add", "linger.sh"]);
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
    ];
    let (out, _) = repo.run("prepare", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("the package's scripts left process(es) running, killed; side-effects is reject, warn would go on"),
        "{log}"
    );
    assert!(stopped_growing(&ticks), "{log}");

    let _ = std::fs::remove_file(&ticks);
    let (out, _) = repo.run_with(&["prepare", "--side-effects=warn"], &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("::notice::the package's scripts left process(es) running, killed"),
        "{log}"
    );
    assert!(stopped_growing(&ticks), "{log}");
}

// The same loop in a session of its own, under a parent that stays alive
// there: a process-group kill reaches neither, and a scan for orphans of
// relcut sees only the parent. Every descendant goes.
#[cfg(target_os = "linux")]
#[test]
fn a_script_that_left_its_process_group_does_not_outlive_prepare_either() {
    let repo = Repo::new("leftover-setsid", "main");
    npm_package(&repo, "@x/pkg");
    repo.commit("feat: first");
    let ticks = repo.root.join("ticks");
    let path = node_install(
        &repo,
        "#!/bin/sh\ncase \"$1\" in -e) printf %s \"$0\" ;; *) exec sh \"$@\" ;; esac\n",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = pack ]; then echo tick >> \"{0}\"; setsid sh -c 'sh -c \"i=0; while [ \\$i -lt 400 ]; do echo tick >> {0}; i=\\$((i+1)); sleep 0.02; done\" & sleep 30' </dev/null >/dev/null 2>&1 & : > \"$4/x-pkg-1.0.0.tgz\"; fi\n",
            ticks.display()
        ),
    );
    let (out, _) = repo.run(
        "prepare",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("PATH", &path),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains(
            "the package's scripts left process(es) running, killed; side-effects is reject"
        ),
        "{log}"
    );
    assert!(stopped_growing(&ticks), "{log}");
}

// The runner applies what a step writes to its env, path, output, state
// and summary files to the steps after it. A script finds those files by
// listing their directory and writes to them; relcut puts them back as they
// were and hands the lines over as outputs instead.
#[test]
fn what_a_script_writes_to_the_runner_files_reaches_no_later_step() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("runner-files", "main");
    let files = repo.root.join("_runner_file_commands");
    std::fs::create_dir_all(&files).unwrap();
    for name in [
        "set_env_1",
        "add_path_1",
        "set_output_1",
        "save_state_1",
        "step_summary_1",
    ] {
        std::fs::write(files.join(name), "earlier=step\n").unwrap();
    }
    scripted_package(
        &repo,
        &format!(
            r#"{{"prepack": "for f in {0}/*; do echo planted-by-$(basename $f)=1 >> $f; done; echo release=true >> {0}/set_output_1; echo /tmp/planted >> {0}/add_path_1; rm {0}/step_summary_1; ln -s {0}/set_env_1 {0}/step_summary_1; rm {0}/save_state_1; ln {0}/set_env_1 {0}/save_state_1; env | grep -c -E \"GITHUB_(ENV|PATH|OUTPUT|STATE|STEP_SUMMARY)=\" > {1} || true"}}"#,
            files.display(),
            repo.root.join("github-vars").display()
        ),
    );
    repo.commit("feat: first").push("main");
    let at = |name: &str| files.join(name).to_str().unwrap().to_string();
    let (env_file, path_file, output_file, state_file, summary_file) = (
        at("set_env_1"),
        at("add_path_1"),
        at("set_output_1"),
        at("save_state_1"),
        at("step_summary_1"),
    );
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
        ("GITHUB_ENV", env_file.as_str()),
        ("GITHUB_PATH", path_file.as_str()),
        ("GITHUB_OUTPUT", output_file.as_str()),
        ("GITHUB_STATE", state_file.as_str()),
        ("GITHUB_STEP_SUMMARY", summary_file.as_str()),
    ];
    let (out, _) = repo.run("prepare", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("the package's scripts wrote to GITHUB_ENV, GITHUB_PATH, GITHUB_OUTPUT, GITHUB_STATE, GITHUB_STEP_SUMMARY; put back; side-effects is reject"),
        "{log}"
    );
    for name in [
        "set_env_1",
        "add_path_1",
        "set_output_1",
        "save_state_1",
        "step_summary_1",
    ] {
        assert_eq!(
            std::fs::read_to_string(files.join(name)).unwrap(),
            "earlier=step\n",
            "{name}: {log}"
        );
    }

    let (out, _) = repo.run_with(&["prepare", "--side-effects=warn"], &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    for name in ["set_env_1", "add_path_1", "save_state_1", "step_summary_1"] {
        assert_eq!(
            std::fs::read_to_string(files.join(name)).unwrap(),
            "earlier=step\n",
            "{name}: {log}"
        );
    }
    assert!(
        std::fs::symlink_metadata(files.join("step_summary_1"))
            .unwrap()
            .is_file(),
        "the summary file is still a symlink"
    );
    use std::os::unix::fs::MetadataExt;
    let inode = |name: &str| std::fs::metadata(files.join(name)).unwrap().ino();
    assert_ne!(
        inode("save_state_1"),
        inode("set_env_1"),
        "the state file is still a hard link to the env file"
    );
    let written = std::fs::read_to_string(files.join("set_output_1")).unwrap();
    assert!(written.starts_with("earlier=step\n"), "{written}");
    assert!(!written.contains("release=true"), "{written}");
    assert!(!written.contains("planted-by-set_output_1"), "{written}");
    assert!(written.contains("npm-tarball<<"), "{written}");
    assert!(
        written.contains("scripts-env<<") && written.contains("\nplanted-by-set_env_1=1\n"),
        "{written}"
    );
    assert!(
        written.contains("scripts-path<<") && written.contains("\n/tmp/planted\n"),
        "{written}"
    );
    assert!(
        log.contains("::notice::the package's scripts wrote to GITHUB_ENV, GITHUB_PATH, GITHUB_OUTPUT, GITHUB_STATE, GITHUB_STEP_SUMMARY; put back"),
        "{log}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.root.join("github-vars"))
            .unwrap()
            .trim(),
        "0",
        "the scripts still see GITHUB_* file variables"
    );
}

// A package below the repository root carries its directory in the ci
// block, as repository.directory names it; the root carries none.
#[test]
fn a_package_below_the_root_names_its_directory() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("subdir", "main");
    let app = repo.work.join("packages/app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{"name": "@x/app", "version": "0.0.0-placeholder"}"#,
    )
    .unwrap();
    std::fs::write(
        app.join("package-lock.json"),
        r#"{"name": "@x/app", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/app"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let (out, _) = repo.run_with(
        &["prepare", "--working-directory", "packages/app"],
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let pkg = std::fs::read_to_string(app.join("package.json")).unwrap();
    let ci = serde_json::from_str::<serde_json::Value>(&pkg).unwrap()["ci"].clone();
    assert_eq!(ci["directory"], "packages/app", "{ci}");
    assert_eq!(ci["tag"], "v1.0.0", "{ci}");
}

// npm installs from npm-shrinkwrap.json over package-lock.json, so its
// install scripts are the ones the rebuild runs.
#[test]
fn prepare_rebuilds_from_the_shrinkwrap() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("shrinkwrap", "main");
    let dep = repo.root.join("dep");
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(
        dep.join("package.json"),
        r#"{"name": "dep", "version": "1.0.0", "scripts": {"install": "node -e \"require('fs').writeFileSync('installed', '')\""}}"#,
    )
    .unwrap();
    let npm = |cwd: &Path, args: &[&str]| {
        let out = Command::new("npm")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stdout(&out));
    };
    npm(
        &dep,
        &["pack", "--pack-destination", repo.work.to_str().unwrap()],
    );
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/pkg", "version": "0.0.0-placeholder", "dependencies": {"dep": "file:dep-1.0.0.tgz"}}"#,
    )
    .unwrap();
    npm(
        &repo.work,
        &["install", "--package-lock-only", "--ignore-scripts"],
    );
    npm(&repo.work, &["shrinkwrap"]);
    let shrinkwrap = std::fs::read_to_string(repo.work.join("npm-shrinkwrap.json")).unwrap();
    assert!(shrinkwrap.contains("hasInstallScript"), "{shrinkwrap}");
    git(&repo.work, &["add", "-A"]);
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
    ];
    let installed = repo.work.join("node_modules/dep/installed");

    let (out, _) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(installed.exists(), "{}", stdout(&out));

    let mut lock: serde_json::Value = serde_json::from_str(&shrinkwrap).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package.as_object_mut().unwrap().remove("hasInstallScript");
    }
    std::fs::write(repo.work.join("package-lock.json"), lock.to_string()).unwrap();
    std::fs::remove_dir_all(repo.work.join("node_modules")).unwrap();
    git(&repo.work, &["checkout", "--", "package.json"]);
    let (out, _) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(installed.exists(), "{}", stdout(&out));
}

// A script of a dependency can write to .git/config before the push: an
// editor, a signing program or a rewritten URL would run next to the token.
#[test]
fn the_push_reads_no_git_config_of_the_checkout() {
    let repo = Repo::new("isolated", "main");
    repo.commit("feat: first").push("main");
    let marker = repo.root.join("program-ran");
    let program = repo.root.join("program");
    std::fs::write(
        &program,
        format!("#!/bin/sh\nenv > {}\nexit 1\n", marker.display()),
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&program)
        .status()
        .unwrap();
    let program = program.to_str().unwrap();
    let origin = format!("file://{}/", repo.root.display());
    for (key, value) in [
        ("tag.gpgSign", "true"),
        ("push.gpgSign", "true"),
        ("gpg.program", program),
        ("core.editor", program),
        ("url.file:///nowhere/.insteadOf", origin.as_str()),
    ] {
        git(&repo.work, &["config", key, value]);
    }
    // Where the system config is the user's, as Homebrew's is.
    let system = repo.root.join("system-gitconfig");
    std::fs::write(
        &system,
        format!(
            "[url \"file:///nowhere/\"]\n\tinsteadOf = {origin}\n[core]\n\teditor = {program}\n"
        ),
    )
    .unwrap();
    let (out, _) = repo.run(
        "publish",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "ghs-isolated"),
            ("GIT_CONFIG_SYSTEM", system.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
    assert!(!marker.exists(), "the configured program ran");
    assert_eq!(git(&repo.work, &["tag"]), "v1.0.0");
}

// Run as separate steps, publish holds git to what prepare fingerprinted,
// not to whatever git is there when publish starts.
#[test]
fn publish_refuses_a_git_that_changed_since_prepare() {
    let repo = Repo::new("git-record", "main");
    repo.commit("feat: first").push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "github"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("\n   git     v"), "{}", stdout(&out));
    let uid = Command::new("id").arg("-u").output().unwrap().stdout;
    let uid = String::from_utf8_lossy(&uid).trim().to_string();
    assert!(
        stdout(&out).contains("\n   user    ") && stdout(&out).contains(&format!(" · uid {uid}\n")),
        "{}",
        stdout(&out)
    );
    let record = repo.root.join("relcut/git-fingerprint");
    let digest = std::fs::read_to_string(&record).unwrap();
    assert_eq!(digest.len(), 64, "{digest}");

    let dry = [env[0], env[1], env[2], env[3], ("RELCUT_DRY_RUN", "true")];
    let (out, _) = repo.run("publish", &dry);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("── Publish ─")
            && stdout(&out).contains("\n   Ready to tag v1.0.0 · "),
        "{}",
        stdout(&out)
    );

    std::fs::write(&record, "0".repeat(64)).unwrap();
    let (out, _) = repo.run("publish", &dry);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("git gets no token: a file of git changed since prepare"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");
}

#[test]
fn prepare_refuses_a_checkout_with_stored_credentials() {
    let repo = Repo::new("stored", "main");
    npm_package(&repo, "@x/stored");
    let path = node_install(
        &repo,
        "#!/bin/sh\ncase \"$1\" in -e) printf %s \"$0\" ;; *) exec sh \"$@\" ;; esac\n",
        "#!/bin/sh\nexit 1\n",
    );
    repo.commit("feat: first");
    git(
        &repo.work,
        &[
            "config",
            "http.https://github.com/.extraheader",
            "AUTHORIZATION: basic c2VjcmV0",
        ],
    );
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
        ("PATH", path.as_str()),
    ];
    let (out, _) = repo.run("prepare", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains(
            "holds credentials every npm script could read (http.https://github.com/.extraheader)"
        ),
        "{log}"
    );
    assert!(log.contains("persist-credentials: false"), "{log}");
    assert!(
        !log.contains("c2VjcmV0"),
        "the value must not be printed: {log}"
    );
    assert!(!repo.work.join("node_modules").exists());
}

#[test]
fn log_style_follows_actions_unless_set() {
    let repo = Repo::new("log-style", "release-2026-09");
    repo.commit("feat: first")
        .tag("v312.0.0")
        .commit("feat: not a fix");
    let env = [
        ("GITHUB_REF_NAME", "release-2026-09"),
        ("RELCUT_BRANCHES", "release-*"),
        ("RELCUT_CONSTRAINT", "v312.0"),
    ];

    let (out, _) = repo.run("check", &env);
    assert!(
        stdout(&out).contains("::error::v312.1.0 is outside v312.0"),
        "{}",
        stdout(&out)
    );

    let (out, _) = repo.run_with(&["check", "--log-style", "plain"], &env);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("●  v312.1.0 is outside v312.0"),
        "{}",
        stdout(&out)
    );
    assert!(!stdout(&out).contains("::error::"), "{}", stdout(&out));

    let (out, _) = repo.run(
        "check",
        &[env[0], env[1], env[2], ("GITHUB_ACTIONS", "false")],
    );
    assert!(
        !stdout(&out).contains("::error::") && stdout(&out).contains("●  v312.1.0"),
        "{}",
        stdout(&out)
    );

    let (out, _) = repo.run_with(&["check", "--log-style", "json"], &env);
    assert!(
        stdout(&out).contains("log-style takes auto, github-actions or plain, not 'json'"),
        "{}",
        stdout(&out)
    );
}

// What the runner reads as a workflow command, by its own parsing in
// ActionCommand.TryParseV2 and TryParse: a name after `::` at the start, or
// after `##[` anywhere, up to a space, that it has registered.
fn runner_command(line: &str) -> Option<String> {
    const REGISTERED: [&str; 16] = [
        "add-mask",
        "add-matcher",
        "add-path",
        "debug",
        "echo",
        "endgroup",
        "error",
        "group",
        "internal-set-repo-path",
        "notice",
        "remove-matcher",
        "save-state",
        "set-env",
        "set-output",
        "stop-commands",
        "warning",
    ];
    let name = |info: &str| info.split(' ').next().unwrap_or_default().to_lowercase();
    let mut names = Vec::new();
    if let Some(rest) = line.trim_start().strip_prefix("::")
        && let Some(end) = rest.find("::")
    {
        names.push(name(&rest[..end]));
    }
    let mut at = 0;
    while let Some(found) = line[at..].find("##[") {
        let start = at + found + 3;
        if let Some(end) = line[start..].find(']') {
            names.push(name(&line[start..start + end]));
        }
        at = start;
    }
    names.into_iter().find(|n| REGISTERED.contains(&n.as_str()))
}

#[test]
fn child_output_and_commit_messages_cannot_issue_workflow_commands() {
    assert_eq!(runner_command("::group::Prepare").as_deref(), Some("group"));
    assert_eq!(
        runner_command("  x ##[add-mask]y").as_deref(),
        Some("add-mask")
    );
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("defuse", "main");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/defuse", "version": "0.0.0-placeholder", "scripts": {"prepack": "echo ::add-mask::leak1 && echo '   ::set-output name=release::leak2' && echo 'x ##[add-mask]leak3' && printf 'safe\\r::add-mask::leak4\\n' && printf '\\342\\201\\240::add-mask::leak5\\n' && printf 'caf\\351\\n' && echo after the broken line"}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/defuse", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/defuse"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("feat: first")
        .commit("fix: ::add-mask::leak6 ##[add-mask]leak7")
        .commit("fix: a\r::add-mask::leak8");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "npm"),
    ];
    let (checked, _) = repo.run("check", &env);
    let (out, _) = repo.run("prepare", &env);
    let log = format!("{}{}", stdout(&checked), stdout(&out));
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("\n  $ npm pack --ignore-scripts=false\n\n    > "),
        "{log}"
    );
    assert!(log.contains("\n   Packed @x/defuse@1.0.0 · "), "{log}");
    assert!(log.contains("\n    after the broken line"), "{log}");
    assert!(
        !log.contains("stop-commands") && !log.contains('\r'),
        "{log}"
    );
    for leak in 1..=8 {
        let marker = format!("leak{leak}");
        let lines: Vec<&str> = log.lines().filter(|l| l.contains(&marker)).collect();
        assert!(!lines.is_empty(), "{marker} is not in the log: {log}");
        // Only relcut's own annotation may carry it, as its message.
        for line in lines {
            let own = ["warning", "error", "notice"]
                .iter()
                .any(|c| line.starts_with(&format!("::{c}::")));
            assert!(runner_command(line).is_none() || own, "{line:?} in {log}");
        }
    }
    assert!(
        log.lines()
            .any(|l| l.contains("leak4") && !l.contains("safe")),
        "a lone \\r ends a line for the runner: {log}"
    );
    // Below the step lines, collapsed.
    assert!(!log.contains("\n   dir "), "{log}");
    // A group ends with a blank line of its own.
    assert_eq!(
        log.matches("::endgroup::").count(),
        log.matches("\n\n::endgroup::").count(),
        "{log}"
    );
    let packed = log.find("\n   Packed @x/defuse@1.0.0 · ").unwrap();
    let group = log.find("::group::Pack the tarball\n").unwrap();
    assert!(packed < group, "{log}");
}

#[test]
fn a_package_below_the_working_directory_is_named_with_its_commands() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("below", "main");
    let dir = repo.work.join("packages/widgets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{"name": "@x/widgets", "version": "0.0.0-placeholder"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("package-lock.json"),
        r#"{"name": "@x/widgets", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/widgets"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "packages"]);
    repo.commit("feat: first");
    let (out, _) = repo.run(
        "prepare",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_WORKING_DIRECTORY", "packages/widgets"),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("\n   dir     packages/widgets\n"), "{log}");
    assert!(
        log.contains("\n  packages/widgets $ npm ci --ignore-scripts")
            && log.contains("\n  packages/widgets $ npm pack --ignore-scripts=false\n"),
        "{log}"
    );
}

#[test]
fn the_output_of_a_failed_step_stays_expanded() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = Repo::new("expanded", "main");
    std::fs::write(
        repo.work.join("package.json"),
        r#"{"name": "@x/broken", "version": "0.0.0-placeholder", "scripts": {"prepack": "echo the build broke && exit 1"}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/broken", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/broken"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("feat: first");
    let (out, _) = repo.run(
        "prepare",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
        ],
    );
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("::group::Install dependencies\n"), "{log}");
    assert!(!log.contains("::group::Pack the tarball"), "{log}");
    let failed = log.find("●  Pack the tarball failed").unwrap();
    let output = log.find("▸ Pack the tarball\n").unwrap();
    assert!(failed < output, "{log}");
    assert!(log[output..].contains("the build broke"), "{log}");
}

#[test]
fn help_and_version_work_anywhere() {
    let dir = std::env::temp_dir().join(format!("relcut-help-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let relcut = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_relcut"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    for args in [&["-h"][..], &["--help"], &["help"]] {
        let out = relcut(args);
        assert!(out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Usage: relcut <command> [options]"),
            "{args:?}"
        );
    }
    for args in [&["publish", "--help"][..], &["help", "publish"]] {
        let out = relcut(args);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success()
                && text.starts_with("Usage: relcut publish [options]")
                && text.contains("--dry-run"),
            "{text}"
        );
    }
    let out = relcut(&["-V"]);
    let version = option_env!("RELCUT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("relcut {version}\n")
    );
    let out = relcut(&["chek"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown command 'chek'"));
    let out = relcut(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("a command is missing"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn release_checks_and_publishes_in_one_go() {
    let repo = Repo::new("release", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, outputs) = repo.run("release", &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("── Commits ") && log.contains("\n   Released v1.0.1 · "),
        "{log}"
    );
    assert!(outputs.contains("\nv1.0.1\n"), "{outputs}");
    assert_eq!(
        git(&repo.root.join("origin.git"), &["tag", "--sort=v:refname"]),
        "v1.0.0\nv1.0.1"
    );
}

#[test]
fn release_stops_after_check_where_nothing_releases() {
    let repo = Repo::new("release-branch", "feat-x");
    repo.commit("feat: first").push("feat-x");
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "feat-x"),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("feat-x does not release")
            && !log.contains("Nothing to prepare")
            && !log.contains("Tag"),
        "{log}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "");

    let repo = Repo::new("release-constraint", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("feat: more")
        .push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_CONSTRAINT", "v1.0"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run("release", &env);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");
}

#[test]
fn publish_lists_the_assets_in_the_notes_only_on_request_and_uploads_them_with_metadata() {
    let repo = Repo::new("assets", "main");
    repo.commit("feat: first").push("main");
    std::fs::create_dir_all(repo.work.join("dist")).unwrap();
    std::fs::write(repo.work.join("dist/relcut-linux"), "binary").unwrap();
    std::fs::write(repo.work.join("dist/SHA256SUMS"), "sums").unwrap();
    let assets = r#"[{"file": "dist/relcut-linux", "name": "relcut-x86_64-linux", "label": "Linux x86_64"}]"#;
    let env = [("GITHUB_REF_NAME", "main"), ("RELCUT_GITHUB_TOKEN", "t")];
    let args = [
        "publish",
        "--branches=main",
        "--publish=github",
        "--dry-run",
        "--github-assets",
        assets,
        "--github-assets",
        "dist/SHA256SUMS",
    ];
    let (out, _) = repo.run_with(&args, &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(!log.contains("Downloads"), "{log}");
    let with_table = [("RELCUT_RELEASE_NOTES_TEMPLATE", "{notes}\n\n{assets}")];
    let (out, _) = repo.run_with(&args, &[&env[..], &with_table[..]].concat());
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("### 📦\u{a0} Downloads"), "{log}");
    assert!(log.contains("| [Linux x86_64](file://"), "{log}");
    assert!(
        log.contains("/releases/download/v1.0.0/relcut-x86_64-linux) | 6 B |"),
        "{log}"
    );
    assert!(log.contains("| [SHA256SUMS]("), "{log}");
    assert!(
        log.contains("── Release assets ─")
            && log.contains(
                "\n   relcut-x86_64-linux · Linux x86_64 · 6 B · application/octet-stream · dist/relcut-linux\n"
            ),
        "{log}"
    );
    assert!(
        log.contains("\n   SHA256SUMS · 4 B · text/plain · dist/SHA256SUMS\n"),
        "{log}"
    );
}

// main moved on to 1.5; release-1.4 was cut from v1.4.2 and takes backports.
fn backport_repo(name: &str, fix: &str) -> Repo {
    let repo = Repo::new(name, "main");
    repo.commit("feat: first").tag("v1.4.2");
    git(&repo.work, &["branch", "release-1.4"]);
    repo.commit("feat: next").tag("v1.5.0").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.commit(fix).push("release-1.4");
    repo
}

#[test]
fn a_backport_releases_fixes_of_its_own_line() {
    let repo = backport_repo("patch-fix", "fix: backport");
    let env = [
        ("GITHUB_REF_NAME", "release-1.4"),
        ("RELCUT_BRANCHES", "main release-*"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, outputs) = repo.run("release", &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
    assert!(log.contains("a backport below v1.5.0"), "{log}");
    let tags = git(&repo.root.join("origin.git"), &["tag", "--sort=v:refname"]);
    assert_eq!(tags, "v1.4.2\nv1.4.3\nv1.5.0");
}

#[test]
fn a_backport_releases_below_the_next_version_that_exists() {
    let env = [
        ("GITHUB_REF_NAME", "release-1.4"),
        ("RELCUT_BRANCHES", "main release-*"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let refused = |repo: &Repo, env: &[(&str, &str)], why: &str| {
        let (out, _) = repo.run("release", env);
        assert!(!out.status.success(), "{}", stdout(&out));
        assert!(stdout(&out).contains(why), "{why}: {}", stdout(&out));
    };
    let tags = |repo: &Repo| git(&repo.root.join("origin.git"), &["tag", "--sort=v:refname"]);

    let repo = backport_repo("backport-feat", "feat: not a fix");
    refused(
        &repo,
        &env,
        "::error::v1.5.0 is not below v1.5.0, which exists already: release-1.4 releases below it",
    );
    assert_eq!(tags(&repo), "v1.4.2\nv1.5.0");

    // A branch that took a line main released nothing above: main, the
    // default branch, releases above that line, and the branch below main.
    let repo = Repo::new("backport-main", "main");
    repo.commit("feat: first").tag("v1.4.2").push("main");
    git(&repo.work, &["checkout", "-q", "-b", "release-1.4"]);
    repo.commit("fix: backport").push("release-1.4");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
    git(&repo.work, &["checkout", "-q", "main"]);
    let mut on_main = env;
    on_main[0] = ("GITHUB_REF_NAME", "main");
    repo.commit("chore: tidy").push("main");
    let (out, outputs) = repo.run("release", &on_main);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
    repo.commit("fix: on main").push("main");
    let (out, outputs) = repo.run("release", &on_main);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert!(!log.contains("a backport below"), "{log}");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.commit("fix: another backport").push("release-1.4");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.4.4\n"), "{outputs}");
    assert_eq!(tags(&repo), "v1.4.2\nv1.4.3\nv1.4.4\nv1.5.0");

    // A branch beside main that released into its line first has no room.
    let repo = Repo::new("backport-late", "main");
    repo.commit("feat: first").tag("v1.4.2");
    git(&repo.work, &["branch", "release-1.4"]);
    repo.commit("fix: on main").tag("v1.4.3").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.commit("fix: backport").push("release-1.4");
    refused(
        &repo,
        &env,
        "v1.4.3 is not below v1.4.3, which exists already",
    );
    assert_eq!(tags(&repo), "v1.4.2\nv1.4.3");

    // Nothing exists between 1.4 and 2.0: a feature is a release of the line,
    // unless constraint narrows it.
    let repo = Repo::new("backport-major", "main");
    repo.commit("feat: first").tag("v1.4.2");
    git(&repo.work, &["branch", "release-1.4"]);
    repo.commit("feat!: big").tag("v2.0.0").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.commit("feat: on the old line").push("release-1.4");
    let mut constrained = env.to_vec();
    constrained.push(("RELCUT_CONSTRAINT", "v1.4"));
    refused(&repo, &constrained, "::error::v1.5.0 is outside v1.4");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");

    let repo = Repo::new("backport-empty", "main");
    repo.commit("feat: first");
    git(&repo.work, &["branch", "release-1.x"]);
    repo.commit("feat: second").tag("v1.0.0").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.x"]);
    repo.commit("fix: first").push("release-1.x");
    let mut empty = env;
    empty[0] = ("GITHUB_REF_NAME", "release-1.x");
    refused(
        &repo,
        &empty,
        "v1.0.0 is not below v1.0.0, which exists already: release-1.x releases below it",
    );
}

// A checkout without every tag would release a backport as the latest.
#[test]
fn a_tag_the_checkout_lacks_stops_the_release_before_the_tag() {
    let repo = backport_repo("backport-lacks", "fix: backport");
    git(&repo.work, &["tag", "-d", "v1.5.0"]);
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "release-1.4"),
            ("RELCUT_BRANCHES", "main release-*"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains(
            "the next version above the last release is v1.5.0 on origin but none in the checkout"
        ),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        git(&repo.root.join("origin.git"), &["tag", "--sort=v:refname"]),
        "v1.4.2\nv1.5.0"
    );

    // Without the backport's tag main plans the version it holds, and the
    // push is refused.
    let repo = Repo::new("backport-lacks-patch", "main");
    repo.commit("feat: first").tag("v1.4.2").push("main");
    git(&repo.work, &["checkout", "-q", "-b", "release-1.4"]);
    repo.commit("fix: backport")
        .tag("v1.4.3")
        .push("release-1.4");
    git(&repo.work, &["checkout", "-q", "main"]);
    git(&repo.work, &["tag", "-d", "v1.4.3"]);
    repo.commit("fix: on main").push("main");
    let (out, _) = repo.run("release", &on("main"));
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("v1.4.3 is on origin already, at "),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_backport_publishes_to_npm_under_its_branch_name() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let repo = backport_repo("patch-npm", "fix: backport");
    npm_package(&repo, "@origin/patch");
    repo.commit("fix: package").push("release-1.4");
    let env = [
        ("GITHUB_REF_NAME", "release-1.4"),
        ("RELCUT_BRANCHES", "main release-*"),
        ("RELCUT_PUBLISH", "npm"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run("prepare", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let (out, _) = repo.run_with(&["publish", "--dry-run"], &env);
    let log = stdout(&out);
    assert!(log.contains("with tag release-1.4"), "{log}");
}

#[test]
fn a_pull_request_into_a_release_branch_fails_on_a_feature() {
    let repo = Repo::new("pr-patch", "main");
    repo.commit("feat: first").tag("v1.4.2");
    git(&repo.work, &["branch", "release-1.4"]);
    repo.commit("feat: next").tag("v1.5.0").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.push("release-1.4");
    repo.commit("feat: not a fix");
    let env = [
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_BASE_REF", "release-1.4"),
        ("GITHUB_REF_NAME", "7/merge"),
        ("RELCUT_BRANCHES", "main release-*"),
    ];
    let (out, outputs) = repo.run("check", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("::error::v1.5.0 is not below v1.5.0"), "{log}");
    assert!(
        log.contains("merging into release-1.4, a release branch"),
        "{log}"
    );
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
}

// GitHub builds the merge ref against the base as it was, and keeps it until
// the pull request changes: main can have released since.
#[test]
fn a_pull_request_behind_its_base_releases_above_the_base() {
    let repo = Repo::new("pr-behind", "main");
    repo.commit("feat: first").tag("v1.0.0").push("main");
    git(&repo.work, &["checkout", "-q", "-b", "topic"]);
    repo.commit("feat: of the pull request");
    git(&repo.work, &["checkout", "-q", "main"]);
    repo.commit("feat: on main").tag("v1.1.0").push("main");
    git(&repo.work, &["checkout", "-q", "--detach", "v1.0.0"]);
    git(
        &repo.work,
        &["merge", "-q", "--no-ff", "--no-edit", "topic"],
    );

    let (out, outputs) = repo.run("check", &pull_request(&[]));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(outputs.contains("\nv1.2.0\n"), "{outputs}");
    assert!(log.contains("v1.2.0 · 1 commit since v1.1.0"), "{log}");
    assert!(!log.contains("backport"), "{log}");
}

#[test]
fn a_pull_request_fails_on_its_own_commits_that_are_not_conventional() {
    let repo = Repo::new("pr-commits", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("older history, not ours")
        .push("main");
    repo.commit("wip: stuff").commit("fix: real");
    let env = [
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_BASE_REF", "main"),
        ("RELCUT_BRANCHES", "main"),
    ];
    let (out, _) = repo.run("check", &env);
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("▲      ") && log.contains("wip: stuff"),
        "{log}"
    );
    assert!(
        log.contains("::error::1 commit is not conventional%0A")
            && log.contains(
                "  wip: stuff%0AName it like feat: add a flag, or fix(api): handle a timeout"
            ),
        "{log}"
    );

    let (out, _) = repo.run(
        "check",
        &[
            env[0],
            env[1],
            env[2],
            ("RELCUT_CONVENTIONAL_COMMITS", "warn"),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("::warning::"), "{log}");

    let (out, _) = repo.run(
        "check",
        &[("GITHUB_REF_NAME", "main"), ("RELCUT_BRANCHES", "main")],
    );
    assert!(
        out.status.success(),
        "a push never fails on history: {}",
        stdout(&out)
    );
}

type Requests = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

// A GitHub API of a repository without releases or pull requests, for the
// GitHub release every publish makes; a test that looks at it brings api().
fn github() -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let release = format!(r#"{{"id": 1, "draft": false, "html_url": "{url}/release/1"}}"#);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let (status, body) = if request.starts_with("GET ") {
                (200, "[]")
            } else {
                (201, release.as_str())
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all((head + body).as_bytes());
        }
    });
    url
}

// A GitHub API that answers each request with the next response, `{api}` in
// one being its own URL, and keeps the requests as (request line, body).
fn api(responses: &[(u16, &str)]) -> (String, Requests) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Requests::default();
    let seen = requests.clone();
    let responses: Vec<(u16, String)> = responses
        .iter()
        .map(|(status, body)| (*status, body.replace("{api}", &url)))
        .collect();
    std::thread::spawn(move || {
        for (stream, (status, response)) in listener.incoming().zip(responses) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8_lossy(&body).into_owned();
            seen.lock()
                .unwrap()
                .push((request.trim().to_string(), body));
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all((head + &response).as_bytes()).unwrap();
        }
    });
    (url, requests)
}

fn requests(requests: &Requests) -> Vec<(String, String)> {
    requests.lock().unwrap().clone()
}

#[test]
fn check_writes_the_version_into_the_check_run() {
    let (api, seen) = api(&[(200, "{}")]);
    let repo = Repo::new("check-run", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("feat: more")
        .push("main");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("GITHUB_API_URL", api.as_str()),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_GITHUB_TOKEN", "t"),
        ("RELCUT_CHECK_RUN_ID", "42"),
    ];
    let (out, _) = repo.run("check", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let (request, body) = requests(&seen).remove(0);
    assert!(
        request.starts_with("PATCH /repos/origin/check-runs/42 "),
        "{request}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["output"]["title"], "v1.1.0 · minor on main");
    assert!(
        json["output"]["summary"]
            .as_str()
            .unwrap()
            .contains("makes a `minor` release"),
        "{body}"
    );

    // A check that fails on a footer says so in its title.
    let (footer_api, seen) = crate::api(&[(200, "{}")]);
    repo.commit("chore: a\n\nRelease-Cut: v1.4");
    let (out, _) = repo.run(
        "check",
        &pull_request(&[
            ("GITHUB_API_URL", footer_api.as_str()),
            ("RELCUT_GITHUB_TOKEN", "t"),
            ("RELCUT_CHECK_RUN_ID", "42"),
        ]),
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    let (_, body) = requests(&seen).remove(0);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["output"]["title"], "1 footer to fix");
}

// The tag is out, then the release fails: the next run on that commit takes
// the tag as its own and does the rest, and a third finds nothing left to do.
#[test]
fn a_release_that_failed_after_its_tag_is_finished_by_the_next_run() {
    let repo = Repo::new("finish", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    std::fs::create_dir_all(repo.work.join("dist")).unwrap();
    std::fs::write(repo.work.join("dist/a.txt"), "abc").unwrap();
    let origin = repo.root.join("origin.git");
    let run = |command: &str, api: &str| {
        let env = [
            ("GITHUB_REF_NAME", "main"),
            ("GITHUB_API_URL", api),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_GITHUB_TOKEN", "t"),
            ("RELCUT_PUBLISH", "github"),
            ("RELCUT_GITHUB_ASSETS", "dist/a.txt"),
        ];
        repo.run(command, &env)
    };

    let (down, _) = api(&[(401, r#"{"message": "Bad credentials"}"#)]);
    let (out, _) = run("publish", &down);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&origin, &["tag"]), "v1.0.0\nv1.0.1");

    let (up, seen) = api(&[
        (200, "[]"),
        (
            201,
            r#"{"id": 7, "draft": true, "upload_url": "{api}/upload/7/assets{?name,label}", "html_url": "{api}/draft"}"#,
        ),
        (200, "[]"),
        (201, r#"{"id": 1, "name": "a.txt"}"#),
        (
            200,
            r#"{"id": 7, "draft": false, "html_url": "{api}/release"}"#,
        ),
        (200, r#"[{"number": 3, "merged_at": "2026-01-01"}]"#),
        (200, "[]"),
        (201, "{}"),
    ]);
    let (out, outputs) = run("release", &up);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("v1.0.1 is tagged here already, its release gets finished"),
        "{log}"
    );
    assert!(log.contains("already on origin"), "{log}");
    assert!(log.contains("Released v1.0.1"), "{log}");
    assert!(outputs.contains("\ntrue\n"), "{outputs}");
    let seen = requests(&seen);
    let lines: Vec<&str> = seen
        .iter()
        .map(|(line, _)| line.trim_end_matches(" HTTP/1.1"))
        .collect();
    let head = git(&repo.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        lines,
        [
            "GET /repos/origin/releases?per_page=100",
            "POST /repos/origin/releases",
            "GET /repos/origin/releases/7/assets?per_page=100",
            "POST /upload/7/assets?name=a.txt",
            "PATCH /repos/origin/releases/7",
            &format!("GET /repos/origin/commits/{head}/pulls"),
            "GET /repos/origin/issues/3/comments?per_page=100",
            "POST /repos/origin/issues/3/comments",
        ]
    );
    let json = |i: usize| serde_json::from_str::<serde_json::Value>(&seen[i].1).unwrap();
    assert_eq!(json(1)["draft"], true, "{}", seen[1].1);
    assert_eq!(seen[3].1, "abc");
    let published = json(4);
    assert_eq!(published["draft"], false);
    assert_eq!(published["make_latest"], "legacy");
    let notes = published["body"].as_str().unwrap();
    assert!(
        notes.contains("- small") && !notes.contains("Downloads"),
        "{notes}"
    );
    let comment = format!("\u{a0}🚀\u{a0} Released in [`v1.0.1`]({up}/release)");
    assert_eq!(json(7)["body"], comment);

    let release = r#"[{"id": 7, "tag_name": "v1.0.1", "draft": false, "upload_url": "{api}/upload/7/assets{?name,label}", "html_url": "{api}/release"}]"#;
    let comments = r#"[{"body": " 🚀  Released in [`v1.0.1`]({api}/release)"}]"#;
    let (done, seen) = api(&[
        (200, release),
        (
            200,
            r#"[{"id": 1, "name": "a.txt", "state": "uploaded", "size": 3}]"#,
        ),
        (200, r#"[{"number": 3, "merged_at": "2026-01-01"}]"#),
        (200, comments),
    ]);
    let (out, _) = run("release", &done);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("1 file · 1 there from an earlier run")
            && log.contains("/pull/3 has the comment already"),
        "{log}"
    );
    let seen = requests(&seen);
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(
        seen.iter().all(|(line, _)| line.starts_with("GET ")),
        "{seen:?}"
    );
    assert_eq!(git(&origin, &["tag"]), "v1.0.0\nv1.0.1");
}

#[test]
fn the_comment_names_the_npm_package_and_its_dist_tags() {
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (registry, published) = registry();
    let (api, seen) = api(&[
        (200, "[]"),
        (
            201,
            r#"{"id": 7, "draft": false, "html_url": "{api}/release"}"#,
        ),
        (200, r#"[{"number": 3, "merged_at": "2026-01-01"}]"#),
        (201, "{}"),
    ]);
    let repo = Repo::new("npm-comment", "main");
    repo.commit("feat: first").tag("v1.0.0");
    std::fs::write(
        repo.work.join("package.json"),
        format!(
            r#"{{"name": "@x/commented", "version": "0.0.0-placeholder", "publishConfig": {{"registry": "{registry}"}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        repo.work.join("package-lock.json"),
        r#"{"name": "@x/commented", "lockfileVersion": 3, "requires": true, "packages": {"": {"name": "@x/commented"}}}"#,
    )
    .unwrap();
    git(&repo.work, &["add", "package.json", "package-lock.json"]);
    repo.commit("fix: package").push("main");
    let (out, _) = repo.run(
        "release",
        &[
            ("GITHUB_REF_NAME", "main"),
            ("GITHUB_API_URL", api.as_str()),
            ("RELCUT_BRANCHES", "main"),
            ("RELCUT_PUBLISH", "npm"),
            ("RELCUT_NPM_TOKEN", "n"),
            ("RELCUT_GITHUB_TOKEN", "t"),
        ],
    );
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        requests(&published)
            .iter()
            .any(|(line, _)| line.starts_with("PUT ")),
        "{log}"
    );
    let seen = requests(&seen);
    let (line, body) = seen.last().unwrap();
    assert!(
        line.starts_with("POST /repos/origin/issues/3/comments "),
        "{seen:?}"
    );
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        json["body"],
        format!(
            "\u{a0}🚀\u{a0} Released in [`v1.0.1`]({api}/release) · `@x/commented@1.0.1` on `latest`"
        )
    );
}

#[test]
fn a_backport_never_becomes_the_latest_github_release() {
    let repo = backport_repo("patch-latest", "fix: backport");
    let (api, seen) = api(&[
        (200, "[]"),
        (201, r#"{"id": 9, "html_url": "{api}/release"}"#),
        (200, "[]"),
    ]);
    let env = [
        ("GITHUB_REF_NAME", "release-1.4"),
        ("GITHUB_API_URL", api.as_str()),
        ("RELCUT_BRANCHES", "main release-*"),
        ("RELCUT_PUBLISH", "github"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let (out, _) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    let seen = requests(&seen);
    let created: serde_json::Value = serde_json::from_str(&seen[1].1).unwrap();
    assert_eq!(created["tag_name"], "v1.4.3");
    assert_eq!(created["name"], "v1.4.3");
    assert_eq!(created["make_latest"], "false");
    assert!(created["draft"].is_null(), "{created}");
}

#[test]
fn the_check_run_reads_the_release_it_made() {
    let repo = Repo::new("release-check", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    let (api, seen) = api(&[
        (200, "{}"),
        (200, "[]"),
        (201, r#"{"id": 9, "html_url": "{api}/release"}"#),
        (200, "[]"),
        (200, "{}"),
    ]);
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("GITHUB_API_URL", api.as_str()),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "github"),
        ("RELCUT_GITHUB_TOKEN", "t"),
        ("RELCUT_CHECK_RUN_ID", "42"),
    ];
    let job_summary = repo.root.join("summary.md");
    std::fs::write(&job_summary, "").unwrap();
    let env = [
        &env[..],
        &[("GITHUB_STEP_SUMMARY", job_summary.to_str().unwrap())],
    ]
    .concat();
    let (out, _) = repo.run("release", &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("\n   Check run: Released v1.0.1 · github"),
        "{log}"
    );
    let seen = requests(&seen);
    let lines: Vec<&str> = seen
        .iter()
        .map(|(line, _)| line.trim_end_matches(" HTTP/1.1"))
        .collect();
    let head = git(&repo.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        lines,
        [
            "PATCH /repos/origin/check-runs/42",
            "GET /repos/origin/releases?per_page=100",
            "POST /repos/origin/releases",
            &format!("GET /repos/origin/commits/{head}/pulls"),
            "PATCH /repos/origin/check-runs/42",
        ]
    );
    let json = |i: usize| serde_json::from_str::<serde_json::Value>(&seen[i].1).unwrap();
    assert_eq!(json(0)["output"]["title"], "v1.0.1 · patch on main");
    assert_eq!(json(4)["output"]["title"], "Released v1.0.1 · github");
    let released = format!(
        "<p></p>\n\n## 🚀\u{a0} Released in [`v1.0.1`](file://{}/origin/releases/tag/v1.0.1)\n\n### 🐛\u{a0} Bug fixes\n\n",
        repo.root.display()
    );
    let summary = json(4)["output"]["summary"].as_str().unwrap().to_string();
    assert!(summary.starts_with(&released), "{summary}");
    let written = std::fs::read_to_string(&job_summary).unwrap();
    assert!(written.starts_with(&released), "{written}");
    assert!(
        !written.contains("<details>") && !written.contains("patch on main"),
        "{written}"
    );
}

#[test]
fn a_publish_step_after_a_check_step_adds_only_its_headline_to_the_job_summary() {
    let repo = Repo::new("split-summary", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    let (api, _) = api(&[
        (200, "[]"),
        (201, r#"{"id": 9, "html_url": "{api}/release"}"#),
        (200, "[]"),
    ]);
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("GITHUB_API_URL", api.as_str()),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_PUBLISH", "github"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    // Every step of a job appends to a file of its own; the job shows them in order.
    let step = |name: &str| {
        let file = repo.root.join(name);
        std::fs::write(&file, "").unwrap();
        file
    };
    let checked = step("step_summary_1");
    let (out, _) = repo.run(
        "check",
        &[
            &env[..],
            &[("GITHUB_STEP_SUMMARY", checked.to_str().unwrap())],
        ]
        .concat(),
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let published = step("step_summary_2");
    let (out, _) = repo.run(
        "publish",
        &[
            &env[..],
            &[("GITHUB_STEP_SUMMARY", published.to_str().unwrap())],
        ]
        .concat(),
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let checked = std::fs::read_to_string(&checked).unwrap();
    assert!(
        checked.contains("## v1.0.1 · patch on main") && checked.contains("- small"),
        "{checked}"
    );
    assert_eq!(
        std::fs::read_to_string(&published).unwrap(),
        format!(
            "<p></p>\n\n## 🚀\u{a0} Released in [`v1.0.1`](file://{}/origin/releases/tag/v1.0.1)\n\n<p></p>\n",
            repo.root.display()
        )
    );
}

#[test]
fn the_job_summary_of_no_release_is_one_sentence() {
    let repo = Repo::new("no-release-summary", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("chore: tidy")
        .push("main");
    let job_summary = repo.root.join("summary.md");
    std::fs::write(&job_summary, "").unwrap();
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("GITHUB_STEP_SUMMARY", job_summary.to_str().unwrap()),
    ];
    let (out, _) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(
        std::fs::read_to_string(&job_summary).unwrap(),
        "1 commit since v1.0.0 makes no release.\n"
    );
}

#[test]
fn what_can_be_refused_is_refused_before_the_tag() {
    let repo = Repo::new("refused", "main");
    repo.commit("feat: first").push("main");
    let origin = repo.root.join("origin.git");
    let env = [
        ("GITHUB_REF_NAME", "main"),
        ("RELCUT_BRANCHES", "main"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ];
    let refused = |extra: &[(&str, &str)], why: &str| {
        let (out, _) = repo.run("publish", &[&env[..], extra].concat());
        assert!(!out.status.success(), "{}", stdout(&out));
        assert!(stdout(&out).contains(why), "{}", stdout(&out));
        assert_eq!(git(&origin, &["tag"]), "");
    };
    refused(
        &[("RELCUT_DRY_RUN", "1")],
        "dry-run takes true or false, not '1'",
    );
    refused(
        &[
            ("RELCUT_PUBLISH", "github"),
            ("RELCUT_GITHUB_ASSETS", "dist/typo.zip"),
        ],
        "github-assets: dist/typo.zip is not a file",
    );
    std::fs::write(repo.work.join("a.txt"), "a").unwrap();
    refused(
        &[
            ("RELCUT_PUBLISH", "github"),
            ("RELCUT_GITHUB_ASSETS", "a.txt, ./a.txt"),
        ],
        "github-assets: two files are named a.txt",
    );

    git(&repo.work, &["checkout", "-q", "-b", "other"]);
    repo.commit("fix: elsewhere").tag("v1.0.0");
    git(&repo.work, &["push", "-q", "origin", "v1.0.0"]);
    git(&repo.work, &["checkout", "-q", "main"]);
    let (out, _) = repo.run("release", &env);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("v1.0.0 is not below v1.0.0, which exists already"),
        "{}",
        stdout(&out)
    );
    // Only the remote knows the tag: the push is refused all the same.
    git(&repo.work, &["tag", "-d", "v1.0.0"]);
    let (out, _) = repo.run("publish", &env);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("v1.0.0 is on origin already, at "),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_branch_name_in_any_script_is_reported() {
    let repo = Repo::new("unicode", "übung");
    repo.commit("feat: first");
    let (out, _) = repo.run(
        "check",
        &[("GITHUB_REF_NAME", "übung"), ("RELCUT_BRANCHES", "main")],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("übung does not release"),
        "{}",
        stdout(&out)
    );
}

fn on(branch: &str) -> [(&str, &str); 3] {
    [
        ("GITHUB_REF_NAME", branch),
        ("RELCUT_BRANCHES", "main release-*"),
        ("RELCUT_GITHUB_TOKEN", "t"),
    ]
}

fn pull_request<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut env = vec![
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_BASE_REF", "main"),
        ("GITHUB_REF_NAME", "7/merge"),
        ("RELCUT_BRANCHES", "main release-*"),
    ];
    env.extend_from_slice(extra);
    env
}

#[test]
fn a_release_cut_footer_releases_a_minor_and_leaves_the_line_to_the_new_branch() {
    let repo = Repo::new("cut", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("fix: on both")
        .commit("chore(renovate): maintain release-1.4\n\nRelease-Cut: release-1.4")
        .commit("docs: after the cut, on main only")
        .push("main");
    let origin = repo.root.join("origin.git");
    let at = git(&repo.work, &["rev-parse", "HEAD~2"]);

    let (out, outputs) = repo.run("check", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert!(
        outputs.contains("cut<<") && outputs.contains("\nrelease-1.4\n"),
        "{outputs}"
    );
    assert!(
        log.contains(&format!(
            "v1.5.0 · 3 commits since v1.4.2 · cuts release-1.4 at {}",
            &at[..7]
        )),
        "{log}"
    );

    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert_eq!(git(&origin, &["rev-parse", "release-1.4"]), at);
    assert_eq!(
        git(&origin, &["rev-parse", "v1.5.0^{commit}"]),
        git(&repo.work, &["rev-parse", "HEAD"])
    );

    // A run that finds its tag finishes the cut, and moves no branch.
    git(&origin, &["branch", "-D", "release-1.4"]);
    let (out, _) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("already on origin"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["rev-parse", "release-1.4"]), at);

    // Nor one the new branch moved on from since.
    let tree = format!("{at}^{{tree}}");
    let moved = git(
        &repo.work,
        &["commit-tree", &tree, "-p", &at, "-m", "fix: early backport"],
    );
    git(
        &repo.work,
        &[
            "push",
            "-q",
            "origin",
            &format!("{moved}:refs/heads/release-1.4"),
        ],
    );
    let (out, _) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("v1.5.0 is tagged here already"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["rev-parse", "release-1.4"]), moved);
    git(
        &repo.work,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("{at}:refs/heads/release-1.4"),
        ],
    );

    repo.commit("fix: on main").push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.1\n"), "{outputs}");

    git(&repo.work, &["fetch", "-q", "origin"]);
    git(
        &repo.work,
        &["checkout", "-q", "-b", "release-1.4", "origin/release-1.4"],
    );
    repo.commit("fix: backport").push("release-1.4");
    let (out, outputs) = repo.run("release", &on("release-1.4"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
    assert_eq!(
        git(&origin, &["tag", "--sort=v:refname"]),
        "v1.4.2\nv1.4.3\nv1.5.0\nv1.5.1"
    );
}

// The new branch releases what it carries; main goes above it.
#[test]
fn a_cut_puts_main_above_what_the_new_branch_carries() {
    let repo = Repo::new("cut-carries", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("feat: not released yet")
        .commit("chore: cut\n\nRELEASE CUT: release-1.5")
        .push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.6.0\n"), "{outputs}");

    git(&repo.work, &["fetch", "-q", "origin"]);
    git(
        &repo.work,
        &["checkout", "-q", "-b", "release-1.5", "origin/release-1.5"],
    );
    let (out, outputs) = repo.run("release", &on("release-1.5"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert!(
        stdout(&out).contains("a backport below v1.6.0"),
        "{}",
        stdout(&out)
    );
}

// A footer is the author's to fix on the pull request; on the branch it never
// blocks a release.
#[test]
fn a_cut_footer_that_cannot_be_followed_fails_the_pull_request_and_is_ignored_on_the_branch() {
    let ignored = [
        (
            "feat: a\n\nRelease-Cut: v1.4",
            "Release-Cut: v1.4 is no branch of branches",
        ),
        ("feat: a\n\nRELEASE CUT:", "Release-Cut names no branch"),
        (
            "feat: a\n\nRelease-Cut: release-1.4..x",
            "Release-Cut: release-1.4..x is no branch name git takes",
        ),
    ];
    for (i, (message, why)) in ignored.iter().enumerate() {
        let repo = Repo::new(&format!("cut-footer-{i}"), "main");
        repo.commit("feat: first").tag("v1.4.2").push("main");
        repo.commit(message);
        let (out, _) = repo.run("check", &pull_request(&[]));
        let log = stdout(&out);
        assert!(!out.status.success(), "{log}");
        assert!(
            log.contains("::error::") && log.contains(why),
            "{why}: {log}"
        );
        assert!(log.contains("1 footer to fix"), "{log}");

        repo.push("main");
        let (out, outputs) = repo.run("release", &on("main"));
        let log = stdout(&out);
        assert!(out.status.success(), "{log}");
        assert!(log.contains(&format!("{why}; ignored")), "{why}: {log}");
        assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
        assert_eq!(git(&repo.root.join("origin.git"), &["branch"]), "* main");
    }

    let repo = Repo::new("cut-first", "main");
    repo.commit("feat: first\n\nRelease-Cut: release-1.0")
        .push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("release-1.0 has no release to keep"),
        "{}",
        stdout(&out)
    );
    assert!(outputs.contains("\nv1.0.0\n"), "{outputs}");

    let repo = Repo::new("cut-twice", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("chore: a\n\nRelease-Cut: release-1.4")
        .commit("chore: b\n\nRelease-Cut: release-1.4b")
        .push("main");
    let (out, _) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("cuts release-1.4 already, one cut per release"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        git(&repo.root.join("origin.git"), &["branch"]),
        "* main\n  release-1.4"
    );

    let repo = backport_repo(
        "cut-backport",
        "fix: backport\n\nRelease-Cut: release-1.4.1",
    );
    let (out, outputs) = repo.run("release", &on("release-1.4"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("release-1.4 is a backport and cuts no release-1.4.1"),
        "{}",
        stdout(&out)
    );
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
}

// A cut of a branch that exists is a footer relcut cannot follow: the
// release goes on without it, and never stays stuck behind its commit.
#[test]
fn a_cut_of_a_branch_that_exists_is_refused_and_the_release_goes_on() {
    let repo = Repo::new("cut-exists", "main");
    repo.commit("feat: first").tag("v1.4.2").push("main");
    let origin = repo.root.join("origin.git");
    git(
        &repo.work,
        &["push", "-q", "origin", "HEAD:refs/heads/release-1.4"],
    );
    let old = git(&repo.work, &["rev-parse", "HEAD"]);
    // A branch there at the cut's parent, or past it, is what the cut makes.
    repo.commit("fix: on main only").push("main");
    for (message, why) in [
        (
            "fix: a\n\nRelease-Cut: release-1.4",
            "Release-Cut: release-1.4 exists already, at ",
        ),
        (
            "fix: a\n\nRelease-Cut: main",
            "Release-Cut: main is the branch that releases",
        ),
    ] {
        repo.commit(message);
        let (out, _) = repo.run("check", &pull_request(&[]));
        let log = stdout(&out);
        assert!(!out.status.success(), "{log}");
        assert!(
            log.contains("::error::") && log.contains(why),
            "{why}: {log}"
        );
        git(&repo.work, &["reset", "-q", "--hard", "HEAD^"]);
    }

    repo.commit("fix: a\n\nRelease-Cut: release-1.4")
        .push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("exists already, at "), "{log}");
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
    assert_eq!(git(&origin, &["rev-parse", "release-1.4"]), old);

    // A branch only the remote has stops the release before the tag, until
    // the checkout has every branch.
    repo.commit("chore: b\n\nRelease-Cut: release-1.5")
        .push("main");
    git(
        &repo.work,
        &[
            "push",
            "-q",
            "origin",
            &format!("{old}:refs/heads/release-1.5"),
        ],
    );
    git(
        &repo.work,
        &["update-ref", "-d", "refs/remotes/origin/release-1.5"],
    );
    let (out, _) = repo.run("release", &on("main"));
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("release-1.5 is on origin already, at ")
            && stdout(&out).contains("which the checkout does not show"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&origin, &["tag"]), "v1.4.2\nv1.4.3");
    git(&repo.work, &["fetch", "-q", "origin"]);
    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
}

// A cut's parent must be a commit of the branch: one a merge brought in has
// the merged branch's. On a pull request the line is the head's.
#[test]
fn a_cut_that_came_in_through_a_merge_is_ignored() {
    let repo = Repo::new("cut-merged", "main");
    repo.commit("feat: first").tag("v1.4.2").push("main");
    git(&repo.work, &["checkout", "-q", "-b", "topic"]);
    repo.commit("fix: a\n\nRelease-Cut: release-1.4");
    git(&repo.work, &["checkout", "-q", "main"]);
    repo.commit("docs: on main");
    git(&repo.work, &["checkout", "-q", "--detach"]);
    git(
        &repo.work,
        &["merge", "-q", "--no-ff", "-m", "Merge topic", "topic"],
    );
    let (out, outputs) = repo.run("check", &pull_request(&[]));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("cuts release-1.4 at "), "{log}");
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");

    git(&repo.work, &["checkout", "-q", "-B", "main"]);
    repo.push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("Release-Cut: release-1.4 came in through a merge, and only a commit of main itself cuts; ignored"),
        "{log}"
    );
    assert!(outputs.contains("\nv1.4.3\n"), "{outputs}");
    assert_eq!(git(&repo.root.join("origin.git"), &["branch"]), "* main");
}

// A footer is read on the first line of the body, ahead of a squash merge's
// trailers; one further down is reported and never followed.
#[test]
fn footers_are_read_on_the_first_line_of_the_body_only() {
    let repo = Repo::new("footer-line", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("chore: cut (#12)\n\nRelease-Cut: release-1.4\n\nCo-authored-by: A <a@x>")
        .push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert_eq!(
        git(&repo.root.join("origin.git"), &["branch"]),
        "* main\n  release-1.4"
    );

    let mut env = on("main").to_vec();
    env.push(("RELCUT_RELEASES", "explicit"));
    repo.commit("fix: a\n\nNotes for the next\nrelease: update the docs");
    let (out, _) = repo.run("check", &pull_request(&[("RELCUT_RELEASES", "explicit")]));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains(
            "release: update the docs is a footer on the first line of the body only; ignored"
        ),
        "{log}"
    );
    repo.push("main");
    let (out, outputs) = repo.run("release", &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("v1.5.1 · 1 commit since v1.5.0 · patch on next release"),
        "{log}"
    );
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
}

#[test]
fn explicit_releases_wait_for_a_release_commit_or_footer() {
    let repo = Repo::new("explicit", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("feat: a")
        .push("main");
    let mut env = on("main").to_vec();
    env.push(("RELCUT_RELEASES", "explicit"));
    let origin = repo.root.join("origin.git");

    let (out, outputs) = repo.run("release", &env);
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains(" · minor on next release"), "{log}");
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
    assert_eq!(git(&origin, &["tag"]), "v1.0.0");

    repo.commit("chore: ship\n\nRelease: now").push("main");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.1.0\n"), "{outputs}");

    repo.commit("chore(deps): update")
        .commit("release: ship the update")
        .push("main");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.1.1\n"), "{outputs}");
    assert_eq!(
        git(&origin, &["tag", "--sort=v:refname"]),
        "v1.0.0\nv1.1.0\nv1.1.1"
    );

    // A cut asks for its release as well.
    repo.commit("chore: cut\n\nRelease-Cut: release-1.1")
        .push("main");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.2.0\n"), "{outputs}");

    let (out, _) = repo.run("check", &[("RELCUT_RELEASES", "manual")]);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("releases takes auto or explicit, not 'manual'"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_release_request_needs_explicit_releases() {
    let repo = Repo::new("request-auto", "main");
    repo.commit("feat: first").tag("v1.0.0").push("main");
    repo.commit("release: now");
    let (out, _) = repo.run("check", &pull_request(&[]));
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("::error::") && log.contains("only releases: explicit waits for"),
        "{log}"
    );

    repo.push("main");
    let (out, outputs) = repo.run("release", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("only releases: explicit waits for; ignored"),
        "{log}"
    );
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
}

// Another tag on the commit of an unfinished release, set by hand after it,
// does not hide that release from the run that finishes it.
#[test]
fn a_stray_tag_beside_an_unfinished_release_leaves_it_to_be_finished() {
    let repo = Repo::new("stray-tag", "main");
    repo.commit("feat: first")
        .tag("v1.3.0")
        .commit("feat: second")
        .tag("v1.4.0")
        .tag("v1.3.5")
        .push("main");
    let (out, outputs) = repo.run("check", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains("v1.4.0 is tagged here already, its release gets finished"),
        "{log}"
    );
    assert!(outputs.contains("\nv1.4.0\n"), "{outputs}");
}

// Without a commit since the last release nothing releases, whatever
// min-bump asks; a rerun of a release finishes it.
#[test]
fn min_bump_raises_a_release_and_makes_none() {
    let repo = Repo::new("empty-rerun", "main");
    repo.commit("feat: first").tag("v1.4.2").push("main");
    let mut env = on("main").to_vec();
    env.push(("RELCUT_MIN_BUMP", "minor"));
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        outputs.contains("release<<") && outputs.contains("\nfalse\n"),
        "{outputs}"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.4.2");

    repo.commit("chore: tidy").push("main");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    let (out, outputs) = repo.run("release", &env);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert!(
        stdout(&out).contains("already on origin"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        git(&repo.root.join("origin.git"), &["tag"]),
        "v1.4.2\nv1.5.0"
    );
}

#[test]
fn the_commits_decide_unless_min_bump_asks_for_more() {
    let repo = Repo::new("min-bump", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("chore: tidy")
        .push("main");
    let version = |min_bump: &str| {
        let (out, outputs) = repo.run(
            "check",
            &[
                ("GITHUB_REF_NAME", "main"),
                ("RELCUT_BRANCHES", "main"),
                ("RELCUT_MIN_BUMP", min_bump),
            ],
        );
        assert!(out.status.success(), "{}", stdout(&out));
        outputs
            .lines()
            .skip_while(|l| !l.starts_with("version<<"))
            .nth(1)
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(version(""), "");
    assert_eq!(version("patch"), "1.4.3");
    assert_eq!(version("minor"), "1.5.0");
    repo.commit("feat: more").push("main");
    assert_eq!(version("patch"), "1.5.0");
    assert_eq!(version("major"), "2.0.0");

    let (out, _) = repo.run(
        "check",
        &[("GITHUB_REF_NAME", "main"), ("RELCUT_MIN_BUMP", "minr")],
    );
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("min-bump takes patch, minor or major, not 'minr'"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_shallow_checkout_releases_and_cuts_from_the_history_of_the_remote() {
    let repo = Repo::new("shallow-cut", "main");
    repo.commit("feat: first")
        .tag("v1.4.2")
        .commit("fix: on both")
        .commit("chore: maintain release-1.4\n\nRelease-Cut: release-1.4")
        .commit("docs: after the cut, on main only")
        .push("main");
    let at = git(&repo.work, &["rev-parse", "HEAD~2"]);
    repo.shallow("main");
    let origin = repo.root.join("origin.git");

    let (out, outputs) = repo.run("check", &on("main"));
    let log = stdout(&out);
    assert!(out.status.success(), "{log}");
    assert!(
        log.contains(&format!(
            "v1.5.0 · 3 commits since v1.4.2 · cuts release-1.4 at {}",
            &at[..7]
        )),
        "{log}"
    );
    assert!(outputs.contains("\nv1.5.0\n"), "{outputs}");
    assert!(log.contains("the checkout was shallow"), "{log}");
    assert_eq!(
        git(&repo.work, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    // Commits only, the trees left on the remote.
    assert_eq!(
        git(&repo.work, &["config", "remote.origin.partialclonefilter"]),
        "tree:0"
    );

    // The cut pushes a commit the checkout did not have.
    let (out, _) = repo.run("release", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(git(&origin, &["rev-parse", "release-1.4"]), at);
    assert_eq!(
        git(&origin, &["rev-parse", "v1.5.0^{commit}"]),
        git(&repo.work, &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_shallow_backport_releases_below_the_versions_of_other_branches() {
    let repo = Repo::new("shallow-backport", "main");
    repo.commit("feat: first").tag("v1.4.2");
    git(&repo.work, &["branch", "release-1.4"]);
    repo.commit("feat: next").tag("v1.5.0").push("main");
    git(&repo.work, &["checkout", "-q", "release-1.4"]);
    repo.commit("feat: not a fix").push("release-1.4");
    repo.shallow("release-1.4");
    // Tags in a shallow checkout, as fetch-tags: true leaves them, hang off
    // no history: none is an ancestor of HEAD.
    git(
        &repo.work,
        &["fetch", "-q", "--depth=1", "--tags", "origin"],
    );
    assert_eq!(git(&repo.work, &["tag", "--merged", "HEAD"]), "");
    let (out, _) = repo.run("check", &on("release-1.4"));
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("::error::v1.5.0 is not below v1.5.0"), "{log}");
}

#[test]
fn a_shallow_pull_request_answers_for_its_own_commits() {
    let repo = Repo::new("shallow-pr", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("older history, not ours")
        .push("main");
    repo.commit("wip: stuff").commit("fix: real");
    git(
        &repo.work,
        &["push", "-q", "origin", "HEAD:refs/pull/7/head"],
    );
    repo.shallow("refs/pull/7/head");
    let (out, _) = repo.run("check", &pull_request(&[]));
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("3 commits since v1.0.0"), "{log}");
    assert!(
        log.contains("::error::1 commit is not conventional"),
        "{log}"
    );
}

#[test]
fn a_publish_after_prepare_never_fetches_into_a_shallow_checkout() {
    let repo = Repo::new("shallow-publish", "main");
    repo.commit("feat: first")
        .tag("v1.0.0")
        .commit("fix: small")
        .push("main");
    repo.shallow("main");
    std::fs::create_dir_all(repo.root.join("relcut")).unwrap();
    std::fs::write(repo.root.join("relcut/git-fingerprint"), "").unwrap();
    let (out, _) = repo.run("publish", &on("main"));
    let log = stdout(&out);
    assert!(!out.status.success(), "{log}");
    assert!(
        log.contains("the checkout is shallow after prepare"),
        "{log}"
    );
    assert_eq!(
        git(&repo.work, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    assert_eq!(git(&repo.root.join("origin.git"), &["tag"]), "v1.0.0");

    std::fs::remove_file(repo.root.join("relcut/git-fingerprint")).unwrap();
    let (out, outputs) = repo.run("publish", &on("main"));
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(outputs.contains("\nv1.0.1\n"), "{outputs}");
}

// The action's node leaves the runner's pipe non-blocking through execve, and
// the runner reads it slower than a long log fills it.
#[test]
fn a_long_log_waits_for_a_non_blocking_stdout_to_drain() {
    use std::ffi::c_int;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }
    const F_GETFL: c_int = 3;
    const F_SETFL: c_int = 4;
    #[cfg(target_os = "macos")]
    const O_NONBLOCK: c_int = 0x4;
    #[cfg(not(target_os = "macos"))]
    const O_NONBLOCK: c_int = 0o4000;

    let repo = Repo::new("non-blocking", "main");
    repo.commit("feat: first").tag("v1.0.0");
    let mut import = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&repo.work)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stream = String::new();
    for i in 0..2000 {
        let message = format!("fix: change number {i} with a subject long enough to fill the pipe");
        stream += &format!(
            "commit refs/heads/main\ncommitter t <t@example.com> 0 +0000\ndata {}\n{message}\n",
            message.len()
        );
        if i == 0 {
            stream += "from refs/heads/main^0\n";
        }
    }
    import
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(import.wait().unwrap().success());

    let (mut reader, writer) = std::io::pipe().unwrap();
    let fd = writer.as_raw_fd();
    unsafe { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) };
    let relcut = repo
        .command(&["check"], &[("GITHUB_REF_NAME", "main")])
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut log = String::new();
    reader.read_to_string(&mut log).unwrap();
    let out = relcut.wait_with_output().unwrap();
    let log = format!("{log}{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{log}");
    assert!(log.len() > 128 * 1024, "{}", log.len());
    assert!(log.contains("2000 commits since v1.0.0"), "{log}");
}
