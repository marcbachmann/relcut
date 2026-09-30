use crate::assets::Asset;
use crate::config::Config;
use crate::git::{Git, Remote};
use crate::plan::{Plan, made_by_cut, releasable};
use crate::prepare::{git_record, npm_record, tarball_record};
use crate::runner::{self, runner};
use crate::version::Version;
use crate::{assets, fill, github, log, npm, output, short_sha, summary, today, var};
use log::step;
use std::path::PathBuf;

// GitHub takes 125000 characters of release notes.
const NOTES_LIMIT: usize = 120_000;

enum Tag {
    Pushed,
    // On the remote from an earlier run that failed after it: this run
    // finishes that release and repeats nothing that is already there.
    Present,
    MovedOn,
}

struct NpmRelease {
    npm: &'static crate::pin::Npm,
    private: npm::Private,
    tarball: PathBuf,
    name: String,
    tags: Vec<String>,
    registry: String,
    token: Option<String>,
    stage: bool,
    version: String,
    published: npm::Published,
}

impl NpmRelease {
    fn request(&self, dry_run: bool) -> npm::Publish<'_> {
        npm::Publish {
            npm: self.npm,
            private: &self.private,
            registry: &self.registry,
            tarball: &self.tarball,
            name: &self.name,
            version: &self.version,
            tags: &self.tags,
            token: self.token.as_deref(),
            stage: self.stage,
            dry_run,
        }
    }

    // Without a tag npm publishes on latest.
    fn dist_tags(&self) -> Vec<&str> {
        match self.tags.as_slice() {
            [] => vec!["latest"],
            tags => tags.iter().map(String::as_str).collect(),
        }
    }
}

pub fn publish(config: &Config, plan: &Plan, git: &Git) -> Result<(), String> {
    let started = std::time::Instant::now();
    let Some(version) = releasable(config, plan) else {
        log::result(log::Outcome::Ok, "Nothing to publish", started, &[]);
        return Ok(());
    };
    let tag = plan.tag(config).unwrap();
    let branch = plan.branch.as_deref().unwrap();
    let r = runner();
    let token = github_token(config)?;
    let head = git.head()?;
    let what = if config.npm { "npm, github" } else { "github" }.to_string();

    assets::check(&config.assets)?;
    log::section("Release assets", || {
        for asset in &config.assets {
            log::ok(&assets::describe(asset)?);
        }
        Ok(())
    })?;
    let gh = github::GitHub::new(&r.api_url, &r.repository, &token);
    let mut assets = config.assets.clone();
    assets.extend(artifacts(config, &gh)?);
    assets::check(&assets)?;

    let (released, npm) = log::section("Publish", || {
        // The tag is the point of no return: whatever can be refused is
        // refused before it.
        let npm = log::quietly(&format!("Ready to tag {tag}"), || {
            step("Check git", "git unchanged", || {
                let pinned = crate::pin::git()?;
                pinned.unchanged()?;
                pinned.as_recorded(&git_record(config))?;
                Ok(())
            })?;
            npm_release(config, plan, git, &head, branch, version, &token)
        })?;
        if !r.ci && !config.dry_run && !config.yes {
            confirm(&format!(
                "Publish {tag} of {} from {branch} ({what})?",
                r.repository
            ))?;
        }
        let remote = Remote::new(git, &r.server_url, format!("{}.git", r.repo_url()), &token)?;
        let tagged = tag_release(config, plan, &remote, git, &tag, &head)?;
        if matches!(tagged, Tag::MovedOn) {
            return Ok((false, npm));
        }
        let resumed = matches!(tagged, Tag::Present);
        output("version", &version.to_string());
        output("tag", &tag);
        if let Some(npm) = &npm {
            publish_npm(config, npm, version, resumed)?;
        }
        github_release(
            config,
            plan,
            &gh,
            &tag,
            version,
            &assets,
            npm.as_ref(),
            resumed,
        )?;
        Ok((true, npm))
    })?;
    if !released {
        output("release", "false");
        summary(&format!("`{tag}` not released: {branch} moved on."));
        log::result(
            log::Outcome::Warn,
            &format!("{tag} not released"),
            started,
            &[format!("{branch} moved on")],
        );
        return Ok(());
    }

    if !config.dry_run {
        log::fact("Release", &format!("{}/releases/tag/{tag}", r.repo_url()));
    }
    if let Some(npm) = &npm {
        let page = match npm::host(&npm.registry) {
            "registry.npmjs.org" => {
                format!("https://www.npmjs.com/package/{}/v/{version}", npm.name)
            }
            "npm.pkg.github.com" => format!(
                "{}/pkgs/npm/{}",
                r.repo_url(),
                npm.name.rsplit('/').next().unwrap_or_default()
            ),
            _ => format!("{}@{version}", npm.name),
        };
        log::fact("npm", &page);
        log::fact("Registry", npm::host(&npm.registry));
        log::fact("Dist-tags", &npm.dist_tags().join(", "));
    }
    for asset in &assets {
        log::fact("Assets", &assets::describe(asset)?);
    }
    if let Some(npm) = npm.filter(|_| !config.keep_tarball && !config.dry_run) {
        let _ = std::fs::remove_file(&npm.tarball);
        let _ = std::fs::remove_file(tarball_record(config));
        let _ = std::fs::remove_file(npm_record(config));
    }
    if !config.keep_tarball && !config.dry_run {
        let _ = std::fs::remove_file(git_record(config));
    }
    let done = if config.dry_run {
        format!("Dry run of {tag}, nothing published")
    } else {
        format!("Released {tag}")
    };
    let notes = config
        .release_notes
        .clone()
        .or_else(|| plan.release_notes(config, ""))
        .unwrap_or_default();
    let head = if config.dry_run {
        format!("<p></p>\n\n## Dry run of `{tag}`")
    } else {
        format!(
            "<p></p>\n\n## 🚀\u{a0} Released in [`{tag}`]({}/releases/tag/{tag})",
            r.repo_url()
        )
    };
    let released = format!("{head}\n\n{}\n\n<p></p>", notes.trim_end());
    if crate::check::summary_written(config) {
        summary(&format!("{head}\n\n<p></p>"));
    } else {
        summary(&released);
    }
    if let Some(id) = config.check_run_id.as_deref().filter(|_| !config.dry_run) {
        let title = format!("Released {tag} · {what}");
        match gh.update_check_run(id, &title, &released) {
            Ok(()) => log::ok(&format!("Check: {title}")),
            Err(e) => log::warn(&format!("updating the check run: {e}")),
        }
    }
    log::result(log::Outcome::Ok, &done, started, &[what]);
    Ok(())
}

fn github_token(config: &Config) -> Result<String, String> {
    let r = runner();
    match &config.github_token {
        Some(token) => Ok(token.clone()),
        None if !r.ci => runner::gh_token(&r.server_url).ok_or_else(|| {
            "publish needs github-token, or gh auth login outside of Actions".into()
        }),
        None => Err("publish needs github-token".into()),
    }
}

fn npm_release(
    config: &Config,
    plan: &Plan,
    git: &Git,
    head: &str,
    branch: &str,
    version: Version,
    github_token: &str,
) -> Result<Option<NpmRelease>, String> {
    if !config.npm {
        return Ok(None);
    }
    let r = runner();
    let (manifest, name, registry) = step("Check the package", "Package", || {
        let manifest = npm::committed(git, head)?;
        let name = manifest["name"]
            .as_str()
            .ok_or("package.json has no name")?
            .to_string();
        let registry = npm::publish_registry(&manifest);
        if npm::host(&registry) == npm::host(npm::GITHUB_PACKAGES)
            && !github_packages_name(&name, &r.repository)
        {
            return Err(format!(
                "GitHub Packages takes {}'s packages as @{}/<name>, not {name}",
                r.repository,
                r.owner()
            ));
        }
        log::done(format!("Package {name}"));
        log::detail(npm::host(&registry));
        Ok((manifest, name, registry))
    })?;
    let github_packages = npm::host(&registry) == npm::host(npm::GITHUB_PACKAGES);
    let pinned = step(
        "Check node and npm",
        "node and npm unchanged since prepare",
        || {
            let pinned = crate::pin::recorded(&npm_record(config))?;
            unchanged(pinned)?;
            Ok(pinned)
        },
    )?;
    let publish_config = &manifest["publishConfig"];
    let tarball = step("Check the tarball", "Tarball matches the commit", || {
        let record = std::fs::read_to_string(tarball_record(config))
            .map_err(|_| "run prepare before publish")?;
        let tarball = PathBuf::from(record.trim());
        if !tarball.is_file() || !record.trim().ends_with(&format!("-{version}.tgz")) {
            return Err(format!(
                "run prepare before publish: {} is not the tarball of {version}",
                tarball.display()
            ));
        }
        let packed = npm::tarball_manifest(&tarball)?;
        if packed["name"] != name {
            return Err(format!(
                "npm gets no token: the tarball's name is {}, not {name}",
                packed["name"]
            ));
        }
        if packed["version"] != version.to_string() {
            return Err(format!(
                "npm gets no token: the tarball's version is {}, not {version}",
                packed["version"]
            ));
        }
        if !packed["publishConfig"].is_null() && packed["publishConfig"] != *publish_config {
            return Err(
                "npm gets no token: the tarball's publishConfig is not the committed one".into(),
            );
        }
        log::detail(tarball.display().to_string());
        Ok(tarball)
    })?;
    // A fix of an older line must not move latest back to it.
    let tags = step("Check the dist-tags", "Dist-tags", || {
        let tags = if !config.npm_tags.is_empty() {
            config.npm_tags.clone()
        } else if plan.ceiling.is_some() {
            vec![branch.to_string()]
        } else {
            publish_config["tag"]
                .as_str()
                .map(|t| vec![t.to_string()])
                .unwrap_or_default()
        };
        if let Some(tag) = tags.iter().find(|t| npm::version_range(t)) {
            return Err(format!(
                "npm takes no dist-tag that reads as a version range, like '{tag}': set npm-tag"
            ));
        }
        // The version is not on the registry until its approval, so no
        // npm dist-tag can name it; the approval sets the tag it was staged with.
        if config.npm_stage && tags.len() > 1 {
            return Err(format!(
                "a staged version takes one dist-tag, set with its approval, not {}: pass one npm-tag",
                tags.join(", ")
            ));
        }
        log::done(match tags.as_slice() {
            [] => "Dist-tag latest".to_string(),
            [one] => format!("Dist-tag {one}"),
            many => format!("Dist-tags {}", many.join(", ")),
        });
        Ok(tags)
    })?;
    let private = npm::Private::new()?;
    let token = if github_packages {
        Some(github_token.to_string())
    } else {
        config.npm_token.clone()
    };
    if token.is_some() {
        let whose = if github_packages { "GitHub" } else { "npm" };
        step("Check the token", "Token", || {
            log::done(format!("{whose} token for {}", npm::host(&registry)));
            Ok(())
        })?;
    } else {
        step("Check trusted publishing", "Trusted publishing", || {
            // npm's provenance names the repository the build ran in; a
            // manifest that names another one fails the publish, after the tag.
            let named = npm::repository(&manifest);
            if !named
                .as_deref()
                .is_some_and(|named| named.eq_ignore_ascii_case(&r.repository))
            {
                return Err(format!(
                    "trusted publishing needs repository.url in package.json naming {}, not {}",
                    r.repository,
                    named.unwrap_or_else(|| "nothing".to_string())
                ));
            }
            if tags.len() > 1 {
                npm::oidc_takes_dist_tags(pinned, &private)?;
            }
            log::done(format!("Trusted publishing from {}", r.repository));
            Ok(())
        })?;
    }
    if config.npm_stage {
        step("Check staged publishing", "Staged publishing", || {
            if github_packages {
                return Err("GitHub Packages has no stage queue: leave npm-stage out".into());
            }
            npm::takes_stage(pinned, &private)?;
            log::done("Staged for a maintainer's approval");
            Ok(())
        })?;
    }
    let mut release = NpmRelease {
        npm: pinned,
        private,
        tarball,
        name,
        tags,
        registry,
        token,
        stage: config.npm_stage,
        version: version.to_string(),
        published: npm::Published::No,
    };
    // A version on the registry cannot be published over: one from another
    // commit stops the release while nothing is tagged yet.
    let host = npm::host(&release.registry).to_string();
    release.published = step(&format!("Check {host}"), "Registry", || {
        let published = npm::published(&release.request(config.dry_run));
        let spec = format!("{}@{version}", release.name);
        match &published {
            npm::Published::No => log::done(format!("{spec} is not on {host} yet")),
            npm::Published::From(commit) if commit == head => {
                log::done(format!("{spec} is on {host}, published from this commit"))
            }
            npm::Published::From(commit) => {
                return Err(format!(
                    "{spec} is on {host} already, published from commit {commit}, not {head}"
                ));
            }
            npm::Published::Unstamped => {
                return Err(format!(
                    "{spec} is on {host} already, and not from this commit: it has no ci.commit, which prepare writes"
                ));
            }
        }
        Ok(published)
    })?;
    Ok(Some(release))
}

fn unchanged(npm: &crate::pin::Npm) -> Result<(), String> {
    match npm.changed()? {
        None => Ok(()),
        Some(file) => Err(format!("npm gets no token: {file} changed since prepare")),
    }
}

// The zips of the artifacts, downloaded before the tag: one that is missing
// stops the release while nothing is out yet.
fn artifacts(config: &Config, gh: &github::GitHub) -> Result<Vec<Asset>, String> {
    if config.artifacts.is_empty() {
        return Ok(Vec::new());
    }
    let title = format!("Release assets{}", log::dim(" · from GitHub artifacts"));
    log::section(&title, || {
        let count = log::count(config.artifacts.len(), "artifact", "artifacts");
        let assets = step("Download the artifacts", "Downloaded", || {
            log::done(format!("Downloaded {count}"));
            if config.dry_run {
                for name in &config.artifacts {
                    log::info(&format!("would download the artifact {name}"));
                }
                log::detail("dry run");
                return Ok(Vec::new());
            }
            let run_id = var("GITHUB_RUN_ID").ok_or("github-artifacts needs GITHUB_RUN_ID")?;
            let mut assets = Vec::new();
            for name in &config.artifacts {
                let file =
                    gh.download_artifact(&run_id, name, &config.pack_dir.join("artifacts"))?;
                assets.push(Asset::new(file));
            }
            Ok(assets)
        })?;
        for asset in &assets {
            log::ok(&assets::describe(asset)?);
        }
        Ok(assets)
    })
}

fn tag_release(
    config: &Config,
    plan: &Plan,
    remote: &Remote,
    git: &Git,
    tag: &str,
    head: &str,
) -> Result<Tag, String> {
    let r = runner();
    let short = short_sha(head);
    let branch = plan.branch.as_deref().unwrap_or_default();
    step(&format!("Tag {tag}"), &format!("Tagged {tag}"), || {
        let cut = plan.cut.as_ref();
        let refs = remote.refs(branch, tag, cut.map(|c| c.branch.as_str()))?;
        let cut_detail = cut.map_or(String::new(), |c| {
            format!(" · cut {} at {}", c.branch, short_sha(&c.at))
        });
        match refs.tag.as_deref() {
            // The push of the tag made the cut; a cut branch that is there has
            // moved on since, and stays as it is.
            Some(at) if at == head => {
                if let Some(c) = cut
                    && refs.cut.is_none()
                    && !config.dry_run
                {
                    remote.create_branch(&c.branch, &c.at)?;
                }
                log::detail(format!("{short} · already on {}{cut_detail}", r.repository));
                return Ok(Tag::Present);
            }
            Some(at) => {
                return Err(format!(
                    "{tag} is on {} already, at {} and not at {short}",
                    r.repository,
                    short_sha(at)
                ));
            }
            None => {}
        }
        // A checkout without every tag sees no ceiling, or a wrong one, and
        // would release a backport as the latest version.
        let last = plan.last.as_ref().map(|(_, v)| *v);
        // A backport the checkout lacks holds the tag that main would take.
        let (above, _) =
            crate::plan::outside(last, remote.releases(&config.tag_prefix)?, plan.trunk);
        if above.as_ref().map(|(_, v)| v) != plan.ceiling.as_ref().map(|(_, v)| v) {
            let name = |c: &Option<(String, Version)>| {
                c.as_ref().map_or("none".to_string(), |(t, _)| t.clone())
            };
            return Err(format!(
                "the next version above the last release is {} on {} but {} in the checkout; check out with fetch-depth: 0",
                name(&above),
                r.repository,
                name(&plan.ceiling)
            ));
        }
        // Tagging a commit that is no longer the branch's head would release
        // it beside the newer one, which the next release includes anyway.
        let moved_on = |how: &str| {
            log::notice(&format!(
                "{branch} moved on since {short}, the next release takes its commits"
            ));
            log::done(format!("Left {tag} untagged"));
            log::detail(how);
            Ok(Tag::MovedOn)
        };
        if refs.branch.as_deref() != Some(head) {
            return moved_on(&format!("{branch} moved on"));
        }
        // The plan refused a cut of a branch the checkout shows; one only the
        // remote has is a checkout without every branch.
        let cut = match (cut, refs.cut.as_deref()) {
            (Some(c), Some(tip)) if made_by_cut(git, &c.sha, &c.at, tip) => None,
            (Some(c), Some(tip)) => {
                return Err(format!(
                    "{} is on {} already, at {}, which the checkout does not show; check out with fetch-depth: 0",
                    c.branch,
                    r.repository,
                    short_sha(tip)
                ));
            }
            (cut, None) => cut.map(|c| (c.branch.as_str(), c.at.as_str())),
            (None, Some(_)) => None,
        };
        if config.dry_run {
            log::done(format!("Would tag {tag}"));
            log::detail(format!("{short}{cut_detail}"));
            return Ok(Tag::Pushed);
        }
        if !remote.push_tag(branch, tag, head, cut)? {
            return moved_on("the push lost to a newer commit");
        }
        let _ = git.tag(tag, head);
        log::detail(format!("{short}{cut_detail}"));
        Ok(Tag::Pushed)
    })
}

fn publish_npm(
    config: &Config,
    npm: &NpmRelease,
    version: Version,
    resumed: bool,
) -> Result<(), String> {
    let version = version.to_string();
    let name = &npm.name;
    unchanged(npm.npm)?;
    let host = npm::host(&npm.registry);
    let release = npm.request(config.dry_run);
    let there = matches!(npm.published, npm::Published::From(_));
    let (doing, did, would) = if npm.stage {
        ("Stage on", "Staged on", "Would stage on")
    } else {
        ("Publish to", "Published to", "Would publish to")
    };
    step(&format!("{doing} {host}"), &format!("{did} {host}"), || {
        let on = npm.dist_tags().join(", ");
        let auth = match &npm.token {
            Some(_) => "token",
            None => "trusted publishing",
        };
        if npm::publish(&release, there, resumed)? {
            if config.dry_run {
                log::done(format!("{would} {host}"));
            } else if npm.stage {
                log::notice(&format!(
                    "{name}@{version} is staged, not installable yet: a maintainer approves it on npmjs.com or with npm stage approve, which sets the dist-tag {on}"
                ));
            }
            log::detail(format!("{name}@{version} on {on} · {auth}"));
        } else {
            log::detail(format!("{name}@{version} is there already"));
        }
        Ok(())
    })
}

fn fit(notes: String) -> String {
    if notes.len() <= NOTES_LIMIT {
        return notes;
    }
    let mut end = NOTES_LIMIT;
    while !notes.is_char_boundary(end) {
        end -= 1;
    }
    let end = notes[..end].rfind('\n').unwrap_or(end);
    format!(
        "{}\n\n… the notes end here, they were too long for a GitHub release\n",
        &notes[..end]
    )
}

// With assets the release starts as a draft and goes public once they are
// up, so nobody, and no workflow on `release: published`, sees it without.
#[allow(clippy::too_many_arguments)]
fn github_release(
    config: &Config,
    plan: &Plan,
    gh: &github::GitHub,
    tag: &str,
    version: Version,
    assets: &[Asset],
    npm: Option<&NpmRelease>,
    resumed: bool,
) -> Result<(), String> {
    let download_url = format!("{}/releases/download/{tag}", runner().repo_url());
    let notes = |assets: &[Asset]| -> Result<String, String> {
        Ok(fit(match &config.release_notes {
            Some(notes) => notes.clone(),
            None => {
                let card = if config.release_notes_template.contains("{assets}") {
                    assets::card(assets, &download_url)?
                } else {
                    String::new()
                };
                plan.release_notes(config, &card).unwrap_or_default()
            }
        }))
    };
    let body = notes(assets)?;
    let title = fill(
        &config.release_title,
        &[
            ("version", &version.to_string()),
            ("tag", tag),
            ("date", &today()),
            ("repository", &runner().repository),
        ],
    );
    // With assets it is a draft until they are up.
    let name = if assets.is_empty() {
        "GitHub release"
    } else {
        "GitHub release · draft"
    };
    if config.dry_run {
        return step(name, name, || {
            log::info(&format!("would create {tag}, '{title}', with these notes:"));
            log::markdown(&body);
            log::done("Would create the GitHub release");
            log::detail(tag.to_string());
            Ok(())
        });
    }
    // A fix of an older line must not become the repository's latest release.
    let latest = if plan.ceiling.is_some() {
        "false"
    } else {
        "legacy"
    };
    let link =
        |release: &serde_json::Value| release["html_url"].as_str().unwrap_or(tag).to_string();
    let (mut release, found) = step(name, name, || {
        let (release, found) = gh.release(tag, &title, &body, !assets.is_empty(), latest)?;
        log::detail(match (found, release["draft"] == true) {
            (true, _) => format!("{tag} has one from an earlier run"),
            (false, true) => "until its assets are up".to_string(),
            (false, false) => link(&release),
        });
        Ok((release, found))
    })?;
    if !assets.is_empty() {
        let mut stored = assets.to_vec();
        step("Upload the assets", "Uploaded the assets", || {
            let mut kept = 0;
            for asset in &mut stored {
                let (uploaded, fresh) = gh.upload(&release, asset)?;
                // GitHub renames what it does not take as a file name.
                if let Some(name) = uploaded["name"].as_str() {
                    asset.name = name.to_string();
                }
                kept += usize::from(!fresh);
            }
            let files = log::count(stored.len(), "file", "files");
            log::detail(match kept {
                0 => files,
                kept => format!("{files} · {kept} there from an earlier run"),
            });
            Ok(())
        })?;
        if release["draft"] == true {
            release = step(
                "Publish the GitHub release",
                "GitHub release published",
                || {
                    let release = gh.publish_release(&release, &notes(&stored)?, latest)?;
                    log::detail(link(&release));
                    Ok(release)
                },
            )?;
        }
    }
    if config.comments {
        let npm_package = npm.map(|n| format!("{}@{version}", n.name));
        let dist_tags = npm.map(|n| {
            n.dist_tags()
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        });
        let body = fill(
            config.comment_template(npm.is_some()),
            &[
                ("version", &version.to_string()),
                ("tag", tag),
                ("date", &today()),
                ("repository", &runner().repository),
                (
                    "release_url",
                    release["html_url"].as_str().unwrap_or_default(),
                ),
                ("npm_package", npm_package.as_deref().unwrap_or_default()),
                ("dist_tags", dist_tags.as_deref().unwrap_or_default()),
            ],
        );
        step(
            "Comment on the released pull requests",
            "Commented on the released pull requests",
            || {
                // Before a first release lies all of history, not the pull
                // requests of one release.
                if plan.last.is_none() {
                    log::detail("none on a first release");
                    return Ok(());
                }
                let count = comment(gh, plan, &body, resumed || found);
                log::detail(log::count(count, "pull request", "pull requests"));
                Ok(())
            },
        )?;
    }
    Ok(())
}

fn confirm(question: &str) -> Result<(), String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "{question} Not in GitHub Actions and no terminal to ask: pass --yes"
        ));
    }
    eprint!("{question} [y/N] ");
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    match answer.trim() {
        "y" | "Y" | "yes" => Ok(()),
        _ => Err("not published".into()),
    }
}

// GitHub Packages takes a repository's packages only under its owner's scope.
fn github_packages_name(name: &str, repository: &str) -> bool {
    let owner = repository
        .split('/')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    name.split_once('/')
        .is_some_and(|(scope, _)| scope == format!("@{owner}"))
}

// The release is out by now: a comment that fails is a warning, not a failure.
// `once` looks for the comment of an earlier run first.
fn comment(gh: &github::GitHub, plan: &Plan, body: &str, once: bool) -> usize {
    let mut numbers = std::collections::BTreeSet::new();
    for (sha, _) in &plan.commits {
        match gh.pull_requests(sha) {
            Ok(found) => numbers.extend(found),
            Err(e) => log::warn(&format!("pull requests of {}: {e}", short_sha(sha))),
        }
    }
    let mut count = 0;
    let pulls = format!("{}/pull", runner().repo_url());
    for number in numbers {
        match gh.comment(number, body, once) {
            Ok(true) => {
                log::info(&format!("commented on {pulls}/{number}"));
                count += 1;
            }
            Ok(false) => log::info(&format!("{pulls}/{number} has the comment already")),
            Err(e) => log::warn(&format!("comment on #{number}: {e}")),
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_too_long_for_github_end_at_a_line() {
        assert_eq!(fit("short\n".into()), "short\n");
        let long = "- é line\n".repeat(NOTES_LIMIT / 8);
        let cut = fit(long.clone());
        assert!(cut.len() < NOTES_LIMIT + 100 && cut.len() < long.len());
        assert!(
            cut.contains("- é line\n\n… the notes end here"),
            "{}",
            &cut[cut.len() - 120..]
        );
    }

    #[test]
    fn github_packages_wants_the_owner_scope() {
        assert!(github_packages_name("@acme-io/server", "Acme-IO/server"));
        assert!(!github_packages_name("@acme/server", "Acme-IO/server"));
        assert!(!github_packages_name("server", "Acme-IO/server"));
    }
}
