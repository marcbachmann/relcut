mod assets;
mod check;
mod commits;
mod config;
mod credentials;
mod git;
mod github;
mod help;
mod leftover;
mod log;
mod npm;
mod pin;
mod plan;
mod prepare;
mod publish;
mod runner;
mod runner_files;
mod side_effects;
mod version;

use check::check;
use config::{Config, Settings};
use git::Git;
use plan::{Plan, releasable};
use prepare::prepare;
use publish::publish;
use std::io::Write;

// The release workflow builds with the version it releases.
const VERSION: &str = match option_env!("RELCUT_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub fn var(key: &str) -> Option<String> {
    credentials::var(key)
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
}

// `{name}` for each var; anything else in braces stays as it is, and so
// does a `{name}` that a value brings along, in a commit's subject say.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        let named = vars.iter().find(|(key, _)| {
            rest.strip_prefix(*key)
                .is_some_and(|after| after.starts_with('}'))
        });
        match named {
            Some((key, value)) => {
                out.push_str(value);
                rest = &rest[key.len() + 1..];
            }
            None => out.push('{'),
        }
    }
    out + rest
}

pub fn output(key: &str, value: &str) {
    let Some(path) = var("GITHUB_OUTPUT") else {
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let delimiter = format!("EOF_{nanos:x}");
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = writeln!(f, "{key}<<{delimiter}\n{value}\n{delimiter}");
    }
}

pub fn summary(markdown: &str) -> bool {
    if let Some(path) = var("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path)
    {
        return writeln!(f, "{markdown}").is_ok();
    }
    false
}

pub fn today() -> String {
    now()[..10].to_string()
}

// Civil time from the Unix clock, Howard Hinnant's days_from_civil inverted.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (h, min, s) = (
        secs.rem_euclid(86_400) / 3600,
        secs.rem_euclid(3600) / 60,
        secs.rem_euclid(60),
    );
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

// check, and prepare and publish only when check found something to release.
// What relcut runs with: itself, git, and node and npm when it releases to npm.
fn environment(command: &str, config: &Config, plan: &Plan) -> Result<(), String> {
    log::section("Environment", || {
        let line = |label: &str, value: &str| log::info(&format!("{label:<8}{value}"));
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        line("relcut", &format!("v{VERSION} · {exe}"));
        line("git", &pin::git()?.uses());
        if config.npm
            && ["release", "prepare"].contains(&command)
            && releasable(config, plan).is_some()
        {
            let [node, npm] = pin::pin()?.uses();
            line("node", &node);
            line("npm", &npm);
        }
        if let Some(user) = credentials::user() {
            line("user", &user);
        }
        if let Some(dir) = log::elsewhere(&config.dir) {
            line("dir", &dir);
        }
        Ok(())
    })
}

// What check found makes the summary unless publish writes the release's.
fn release(config: &Config, plan: &Plan, git: &Git) -> Result<bool, String> {
    let (passed, checked) = check(config, plan, git)?;
    if !passed || releasable(config, plan).is_none() {
        summary(&checked);
        return Ok(passed);
    }
    prepare(config, plan, git)
        .and_then(|_| publish(config, plan, git))
        .inspect_err(|_| {
            summary(&checked);
        })?;
    Ok(true)
}

// The scripts of prepare may have left a git config that the fetch would
// read next to the token, so a publish after them never fetches.
fn unshallow(command: &str, config: &Config, git: &Git) -> Result<(), String> {
    if !git.is_shallow()? {
        return Ok(());
    }
    if command == "publish" && prepare::git_record(config).exists() {
        return Err(
            "the checkout is shallow after prepare, which leaves it complete; check out with fetch-depth: 0"
                .into(),
        );
    }
    let r = runner::runner();
    let token = config
        .github_token
        .clone()
        .or_else(|| (!r.ci).then(|| runner::gh_token(&r.server_url)).flatten());
    git.unshallow(&r.server_url, token.as_deref())?;
    log::warn(
        "the checkout was shallow and relcut fetched its history; check out with fetch-depth: 0 and filter: tree:0",
    );
    Ok(())
}

fn main() {
    credentials::seal();
    leftover::adopt_orphans();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().cloned().unwrap_or_default();
    if command == "help" || args.iter().any(|a| a == "-h" || a == "--help") {
        let topic = if command == "help" {
            args.get(1).cloned().unwrap_or_default()
        } else {
            command.clone()
        };
        println!("{}", help::command(&topic).unwrap_or_else(help::general));
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("relcut {VERSION}");
        return;
    }
    if !["release", "check", "prepare", "publish"].contains(&command.as_str()) {
        let what = if command.is_empty() {
            "a command is missing".to_string()
        } else {
            format!("unknown command '{command}'")
        };
        eprintln!(
            "relcut: {what}\n\nUsage: relcut <release|check|prepare|publish> [options]\nSee relcut --help"
        );
        std::process::exit(2);
    }
    let result = Settings::parse(args.get(1..).unwrap_or_default())
        .and_then(|s| {
            log::init(s.value("LOG_STYLE").as_deref())?;
            Ok((Config::read(&s)?, s.list("PASS_ENV")))
        })
        .and_then(|(config, pass_env)| {
            credentials::pass(pass_env);
            side_effects::set(config.reject_side_effects);
            pin::git()?;
            let git = Git {
                cwd: config.dir.clone(),
            };
            runner::detect(&git)?;
            unshallow(&command, &config, &git)?;
            let plan = Plan::read(&config, &git)?;
            environment(&command, &config, &plan)?;
            match command.as_str() {
                "release" => release(&config, &plan, &git),
                "check" => check(&config, &plan, &git).map(|(passed, checked)| {
                    if summary(&checked) {
                        check::summarized(&config);
                    }
                    passed
                }),
                "prepare" => prepare(&config, &plan, &git).map(|_| true),
                _ => publish(&config, &plan, &git).map(|_| true),
            }
        });
    match result {
        Ok(true) => log::finish(),
        Ok(false) => {
            log::finish();
            std::process::exit(1)
        }
        Err(e) => {
            let mut head = command.chars();
            let head: String = head
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .chain(head)
                .collect();
            log::fail(&format!("{head} failed"), &e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_known_names_only() {
        let vars = [("tag", "v1.0.0"), ("previous_tag", "")];
        assert_eq!(
            fill("{tag}{previous_tag} {other} {tag}", &vars),
            "v1.0.0 {other} v1.0.0"
        );
        let vars = [("notes", "- handle {tag} in {braces"), ("tag", "v1.0.0")];
        assert_eq!(
            fill("{notes} for {tag}", &vars),
            "- handle {tag} in {braces for v1.0.0"
        );
    }

    #[test]
    fn today_is_a_date() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
        let t = now();
        assert_eq!(t.len(), 20, "{t}");
        assert!(
            t.starts_with(&d) && t.ends_with('Z') && t.as_bytes()[10] == b'T',
            "{t}"
        );
    }
}
