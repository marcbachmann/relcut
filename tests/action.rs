use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        other => panic!("no relcut build for {other:?}"),
    }
}

// A repository with v1.2.3 on its first commit and an untagged one after it,
// and the release of v1.2.3 as files.
struct Fixture {
    root: PathBuf,
    tagged: String,
    untagged: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        Fixture::with(name, b"#!/bin/sh\necho fake relcut\n")
    }

    fn with(name: &str, binary: &[u8]) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("relcut-action-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(
            &work,
            &[
                "-c",
                "user.email=t@e.c",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "feat: first",
            ],
        );
        git(&work, &["tag", "v1.2.3"]);
        git(&work, &["branch", "v1"]);
        let tagged = git(&work, &["rev-parse", "HEAD"]);
        git(
            &work,
            &[
                "-c",
                "user.email=t@e.c",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fix: later",
            ],
        );
        let untagged = git(&work, &["rev-parse", "HEAD"]);

        let release = root.join("releases/v1.2.3");
        std::fs::create_dir_all(&release).unwrap();
        let name = format!("relcut-{}", target());
        std::fs::write(release.join(&name), binary).unwrap();
        let sum: String = Sha256::digest(binary)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(release.join("SHA256SUMS"), format!("{sum}  {name}\n")).unwrap();
        Fixture {
            root,
            tagged,
            untagged,
        }
    }

    fn install(&self, env: &[(&str, &str)]) -> (Output, String) {
        let outputs = self.root.join("outputs");
        std::fs::write(&outputs, "").unwrap();
        let out = Command::new("/bin/bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("setup/install.sh"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("GITHUB_OUTPUT", &outputs)
            .env("RUNNER_TOOL_CACHE", self.root.join("cache"))
            .env("RELCUT_GIT_URL", self.root.join("work"))
            .env(
                "RELCUT_RELEASES_URL",
                format!("file://{}", self.root.join("releases").display()),
            )
            .env("RELCUT_REPOSITORY", "acme/relcut")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (out, std::fs::read_to_string(outputs).unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn log(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn installed(outputs: &str) -> String {
    let path = outputs
        .lines()
        .find_map(|l| l.strip_prefix("path="))
        .expect(outputs);
    String::from_utf8_lossy(&Command::new(path).output().unwrap().stdout).into_owned()
}

#[test]
fn installs_the_version_the_action_is_pinned_to() {
    let f = Fixture::new("pinned");
    for reference in ["v1.2.3", f.tagged.as_str(), "v1"] {
        let _ = std::fs::remove_dir_all(f.root.join("cache"));
        let (out, outputs) = f.install(&[("RELCUT_REF", reference)]);
        assert!(out.status.success(), "{reference}: {}", log(&out));
        assert!(
            outputs.contains("version=1.2.3\n"),
            "{reference}: {outputs}"
        );
        assert_eq!(installed(&outputs), "fake relcut\n");
    }
}

#[test]
fn reads_the_ref_from_the_action_path_when_the_context_has_none() {
    let f = Fixture::new("path");
    let (out, outputs) = f.install(&[
        ("RELCUT_REPOSITORY", ""),
        (
            "GITHUB_ACTION_PATH",
            "/home/runner/work/_actions/acme/relcut/v1.2.3/setup",
        ),
    ]);
    assert!(out.status.success(), "{}", log(&out));
    assert!(outputs.contains("version=1.2.3\n"), "{outputs}");
}

#[test]
fn an_asked_for_version_wins_and_a_cached_one_is_not_downloaded_again() {
    let f = Fixture::new("cache");
    let (out, outputs) = f.install(&[("RELCUT_REF", "main"), ("RELCUT_VERSION_WANTED", "v1.2.3")]);
    assert!(out.status.success(), "{}", log(&out));
    assert!(outputs.contains("version=1.2.3\n"), "{outputs}");
    std::fs::remove_file(f.root.join(format!("releases/v1.2.3/relcut-{}", target()))).unwrap();
    let (out, _) = f.install(&[("RELCUT_REF", "v1.2.3")]);
    assert!(out.status.success(), "{}", log(&out));
}

// A step before this one, a package script of prepare say, wrote over the
// binary in the tool cache; the next step must not run it.
#[test]
fn a_cached_binary_that_was_written_over_is_downloaded_again() {
    let f = Fixture::new("overwritten");
    let (out, outputs) = f.install(&[("RELCUT_REF", "v1.2.3")]);
    assert!(out.status.success(), "{}", log(&out));
    let path = outputs
        .lines()
        .find_map(|l| l.strip_prefix("path="))
        .unwrap()
        .to_string();
    std::fs::write(&path, "#!/bin/sh\necho planted\n").unwrap();
    std::fs::write(
        Path::new(&path).with_file_name("SHA256SUMS"),
        "0000  planted\n",
    )
    .unwrap();
    let (out, outputs) = f.install(&[("RELCUT_REF", "v1.2.3")]);
    assert!(out.status.success(), "{}", log(&out));
    assert_eq!(installed(&outputs), "fake relcut\n");

    std::fs::write(&path, "#!/bin/sh\necho planted\n").unwrap();
    std::fs::remove_file(f.root.join(format!("releases/v1.2.3/relcut-{}", target()))).unwrap();
    let (out, outputs) = f.install(&[("RELCUT_REF", "v1.2.3")]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(!outputs.contains("path="), "{outputs}");
}

#[test]
fn refuses_a_binary_that_does_not_match_its_checksum() {
    let f = Fixture::new("checksum");
    std::fs::write(
        f.root.join(format!("releases/v1.2.3/relcut-{}", target())),
        "tampered",
    )
    .unwrap();
    let (out, outputs) = f.install(&[("RELCUT_REF", "v1.2.3")]);
    assert!(!out.status.success());
    assert!(
        log(&out).contains("does not match its SHA256SUMS"),
        "{}",
        log(&out)
    );
    assert!(!outputs.contains("path="));
}

#[test]
fn says_what_to_do_on_a_ref_without_a_release() {
    let f = Fixture::new("untagged");
    let (out, _) = f.install(&[("RELCUT_REF", f.untagged.as_str())]);
    assert!(!out.status.success());
    assert!(
        log(&out).contains("pin the action to a release, or set version"),
        "{}",
        log(&out)
    );
}

// The runner evaluates an expression anywhere in action.yml, a description
// too, and refuses the whole action over a context it does not allow there.
#[test]
fn no_description_holds_an_expression() {
    for file in ["action.yml", "setup/action.yml"] {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
        let mut description = false;
        for line in text.lines() {
            let key = line.trim_start();
            if key.contains(':') && !key.starts_with('-') && line.len() - key.len() <= 4 {
                description = key.starts_with("description:");
            }
            assert!(
                !(description && line.contains("${{")),
                "{file}: an expression in a description: {line}"
            );
        }
    }
}

// The action hands relcut every input as RELCUT_<NAME>, so an input that is
// no setting would reach relcut as a variable it never reads.
#[test]
fn the_action_has_an_input_for_every_setting_but_the_interactive_ones() {
    let action =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("action.yml")).unwrap();
    let main = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config.rs"))
        .unwrap();
    let settings: Vec<String> = main
        .split("const SETTINGS")
        .nth(1)
        .unwrap()
        .split("];")
        .next()
        .unwrap()
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| !["LOG_STYLE", "YES"].contains(s))
        .map(|s| s.to_lowercase().replace('_', "-"))
        .collect();
    for input in &settings {
        assert!(
            action.contains(&format!("\n  {input}:\n")),
            "action.yml has no input {input}"
        );
    }
    let inputs = action.split("\ninputs:\n").nth(1).unwrap();
    let inputs = inputs.split("\noutputs:\n").next().unwrap();
    for line in inputs.lines() {
        if let Some(name) = line.strip_prefix("  ").and_then(|l| l.strip_suffix(':'))
            && !name.starts_with(' ')
        {
            assert!(
                settings.iter().any(|s| s == name) || ["command", "version"].contains(&name),
                "action.yml has input {name}, which is no setting"
            );
        }
    }
}

fn node_execs() -> bool {
    Command::new("node")
        .args([
            "-e",
            "process.exit(typeof process.execve === 'function' ? 0 : 1)",
        ])
        .status()
        .is_ok_and(|s| s.success())
}

fn run_action(f: &Fixture, env: &[(&str, &str)]) -> (u32, Output, String) {
    let outputs = f.root.join("outputs");
    std::fs::write(&outputs, "").unwrap();
    let child = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("action/main.js"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", &f.root)
        .env("GITHUB_OUTPUT", &outputs)
        .env("RUNNER_TOOL_CACHE", f.root.join("cache"))
        .env("RUNNER_TEMP", &f.root)
        .env("RELCUT_GIT_URL", f.root.join("work"))
        .env(
            "RELCUT_RELEASES_URL",
            format!("file://{}", f.root.join("releases").display()),
        )
        .env("GITHUB_ACTION_REPOSITORY", "acme/relcut")
        .env("GITHUB_ACTION_REF", "v1.2.3")
        .envs(env.iter().copied())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let out = child.wait_with_output().unwrap();
    (pid, out, std::fs::read_to_string(outputs).unwrap())
}

#[test]
fn the_action_becomes_relcut_with_the_inputs_as_settings() {
    if !node_execs() {
        return;
    }
    let f = Fixture::with(
        "node",
        b"#!/bin/sh\necho \"pid=$$ args=$*\"\nenv | grep -E '^(RELCUT_|INPUT_)' | sort\n",
    );
    let (pid, out, outputs) = run_action(
        &f,
        &[
            ("RELCUT_MIN_BUMP", "major"),
            ("INPUT_COMMAND", "check"),
            ("INPUT_BRANCHES", "main release-*"),
            ("INPUT_MIN-BUMP", ""),
            ("INPUT_GITHUB-TOKEN", "secret"),
            ("INPUT_PUBLISH", "npm"),
        ],
    );
    let log = log(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains(&format!("pid={pid} args=check\n")), "{log}");
    assert!(log.contains("RELCUT_BRANCHES=main release-*\n"), "{log}");
    assert!(log.contains("RELCUT_GITHUB_TOKEN=secret\n"), "{log}");
    assert!(log.contains("RELCUT_PUBLISH=npm\n"), "{log}");
    assert!(!log.contains("RELCUT_MIN_BUMP"), "{log}");
    assert!(!log.contains("RELCUT_COMMAND"), "{log}");
    assert!(!log.contains("INPUT_"), "{log}");
    assert_eq!(outputs, "relcut-version=1.2.3\n");
}
