use crate::git::Git;
use crate::var;
use std::sync::OnceLock;

// Where the release runs: the Actions context in a workflow, the checkout's
// origin and branch on a developer's machine.
pub struct Runner {
    pub ci: bool,
    pub repository: String,
    pub server_url: String,
    pub api_url: String,
    pub branch: Option<String>,
    // The branch a pull request merges into.
    pub base: Option<String>,
    // The repository's default branch: the trunk other branches backport to.
    pub default_branch: Option<String>,
}

static RUNNER: OnceLock<Runner> = OnceLock::new();

pub fn runner() -> &'static Runner {
    RUNNER.get().expect("runner::detect runs first")
}

pub fn detect(git: &Git) -> Result<&'static Runner, String> {
    let ci = var("GITHUB_ACTIONS").is_some_and(|v| v == "true");
    let server_url = var("GITHUB_SERVER_URL").unwrap_or_else(|| "https://github.com".into());
    let repository = match var("GITHUB_REPOSITORY") {
        Some(r) => r,
        None if !ci => {
            let origin = git.run(&["remote", "get-url", "origin"])?;
            parse_repository(origin.trim()).ok_or(format!(
                "origin {} is not a GitHub repository",
                origin.trim()
            ))?
        }
        None => return Err("GITHUB_REPOSITORY is not set".into()),
    };
    let pull_request = var("GITHUB_EVENT_NAME").is_some_and(|e| e.starts_with("pull_request"));
    let branch = match var("GITHUB_REF_NAME") {
        _ if pull_request => None,
        Some(b) => Some(b),
        None if !ci => Some(
            git.run(&["rev-parse", "--abbrev-ref", "HEAD"])?
                .trim()
                .to_string(),
        )
        .filter(|b| b != "HEAD"),
        None => None,
    };
    let base = if pull_request {
        var("GITHUB_BASE_REF")
    } else {
        None
    };
    let default_branch = if ci {
        var("GITHUB_EVENT_PATH")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|event| serde_json::from_str::<serde_json::Value>(&event).ok())
            .and_then(|event| Some(event["repository"]["default_branch"].as_str()?.to_string()))
    } else {
        git.run(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .ok()
        .and_then(|head| Some(head.trim().strip_prefix("origin/")?.to_string()))
    };
    let runner = Runner {
        base,
        default_branch,
        ci,
        repository,
        api_url: var("GITHUB_API_URL").unwrap_or_else(|| "https://api.github.com".into()),
        server_url,
        branch,
    };
    Ok(RUNNER.get_or_init(|| runner))
}

impl Runner {
    pub fn repo_url(&self) -> String {
        format!("{}/{}", self.server_url, self.repository)
    }

    pub fn owner(&self) -> String {
        self.repository
            .split('/')
            .next()
            .unwrap_or_default()
            .to_lowercase()
    }
}

// Only outside of Actions, where nobody passes the token in on purpose.
pub fn gh_token(server_url: &str) -> Option<String> {
    let host = server_url.split_once("://").map_or(server_url, |(_, h)| h);
    let mut gh = std::process::Command::new("gh");
    gh.args(["auth", "token", "--hostname", host]);
    crate::credentials::restore(
        &mut gh,
        &["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"],
    );
    let out = gh.output().ok()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}

fn parse_repository(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("git@")
        .and_then(|rest| rest.split_once(':').map(|(_, p)| p))
        .or_else(|| {
            url.split_once("://")
                .map(|(_, rest)| rest.split_once('/').map_or("", |(_, p)| p))
        })?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplitn(3, '/');
    let repo = parts.next().filter(|s| !s.is_empty())?;
    let owner = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_from_origin() {
        for url in [
            "git@github.com:acme/widgets.git",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
        ] {
            assert_eq!(
                parse_repository(url).as_deref(),
                Some("acme/widgets"),
                "{url}"
            );
        }
        assert_eq!(parse_repository("/tmp/origin.git"), None);
    }
}
