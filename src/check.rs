use crate::commits::analyze;
use crate::config::Config;
use crate::git::Git;
use crate::plan::{Plan, violation, waiting};
use crate::runner::runner;
use crate::version::Bump;
use crate::{github, log, output, short_sha};
use std::path::PathBuf;

// The job summary of a check step has the notes already, so a publish step
// of the same job adds its headline alone. RUNNER_TEMP is the job's.
fn summary_record(config: &Config) -> PathBuf {
    config.pack_dir.join("check-summary")
}

pub fn summarized(config: &Config) {
    let _ = std::fs::create_dir_all(&config.pack_dir)
        .and_then(|_| std::fs::write(summary_record(config), ""));
}

pub fn summary_written(config: &Config) -> bool {
    summary_record(config).exists()
}

// Whether the commits pass, and the job summary of what they make.
pub fn check(config: &Config, plan: &Plan, git: &Git) -> Result<(bool, String), String> {
    let started = std::time::Instant::now();
    let changes = plan.changes();
    let since = match &plan.last {
        Some((tag, _)) => format!("since {tag}"),
        None => "without a release before".into(),
    };
    let commits = log::count(plan.commits.len(), "commit", "commits");
    let listed: Vec<String> = changes
        .iter()
        .zip(&plan.commits)
        .map(|(c, (sha, message))| {
            let effect = match c.bump {
                Some(bump) => format!("{:<6}", bump.as_str()),
                None if !analyze(sha, message).conventional => log::yellow(&format!("{:<6}", "▲")),
                None => log::dim(&format!("{:<6}", "·")),
            };
            let header = message.lines().next().unwrap_or_default();
            format!("{effect} {}  {header}", short_sha(c.sha))
        })
        .collect();
    let (failed, mut parts, summary) = log::section("Commits", || {
        log::collapsed(&format!("{commits} {since}"), &listed);
        // A pull request answers for its own commits only; what is already on
        // the branch is not its author's to fix, and never blocks a release.
        let pull_request = runner().base.as_deref();
        let own = match pull_request {
            Some(base) => git.commits_beyond(base)?,
            None => plan.commits.clone(),
        };
        let unconventional: Vec<String> = own
            .iter()
            .filter(|(sha, message)| !analyze(sha, message).conventional)
            .map(|(sha, message)| {
                format!(
                    "{} · not a conventional commit: {}",
                    short_sha(sha),
                    message.lines().next().unwrap_or_default()
                )
            })
            .collect();
        let refuse_commits = pull_request.is_some() && config.enforce_conventional;
        let not_conventional = match unconventional.len() {
            1 => "1 commit is not conventional".to_string(),
            n => format!("{n} commits are not conventional"),
        };
        // What to do about it: which commits, and the name they need.
        let advice = {
            let offending: Vec<String> = own
                .iter()
                .filter(|(sha, message)| !analyze(sha, message).conventional)
                .map(|(sha, message)| {
                    format!(
                        "{}  {}",
                        short_sha(sha),
                        message.lines().next().unwrap_or_default()
                    )
                })
                .collect();
            let them = if offending.len() == 1 { "it" } else { "them" };
            format!(
                "{not_conventional}\n{}\nName {them} like feat: add a flag, or fix(api): handle a timeout",
                offending.join("\n")
            )
        };
        match (unconventional.is_empty(), refuse_commits) {
            (true, _) if !own.is_empty() => log::ok(&log::count(
                own.len(),
                "conventional commit",
                "conventional commits",
            )),
            (true, _) => {}
            (false, true) => log::error(&advice),
            (false, false) => log::warn(&advice),
        }
        // A footer is the author's to fix on the pull request; on the branch it
        // never blocks a release, which would take its commit along every time.
        let mut refused_footers = 0;
        for (sha, why) in &plan.ignored {
            let line = format!("{} · {why}", short_sha(sha));
            if pull_request.is_some() && own.iter().any(|(own, _)| own == sha) {
                log::error(&line);
                refused_footers += 1;
            } else {
                log::warn(&format!("{line}; ignored"));
            }
        }
        for (sha, why) in &plan.misplaced {
            log::warn(&format!("{} · {why}; ignored", short_sha(sha)));
        }

        let tag = plan.tag(config).unwrap_or_default();
        let outside = violation(config, plan);
        let waits = waiting(config, plan);
        let releases = plan.releasing && plan.next.is_some() && outside.is_none() && !waits;
        let branch = match (&plan.branch, &plan.target, plan.target_releases) {
            (Some(b), _, true) => format!("{b} is a release branch"),
            (Some(b), _, false) => format!("{b} does not release"),
            (None, Some(t), true) => format!("merging into {t}, a release branch"),
            (None, Some(t), false) => format!("{t} does not release"),
            (None, None, _) => "nothing releases here".into(),
        };
        let failed_commits = refuse_commits && !unconventional.is_empty();
        // `minor on main`, `minor on next release`, or `minor` where nothing releases.
        let on = |bump: Bump| match (waits, plan.target.as_deref()) {
            (true, _) => format!("{} on next release", bump.as_str()),
            (false, Some(target)) if plan.target_releases => {
                format!("{} on {target}", bump.as_str())
            }
            _ => bump.as_str().to_string(),
        };
        // Only what is not the usual: a branch that releases what it says.
        let mut parts = match plan.next {
            Some(_) => vec![tag.clone()],
            None => vec!["no release".into()],
        };
        parts.push(format!("{commits} {since}"));
        if let Some((bump, _)) = plan.next.filter(|_| waits) {
            parts.push(on(bump));
        }
        if plan.branch.is_none() || !plan.target_releases {
            parts.push(branch.clone());
        }
        let below = plan
            .ceiling
            .as_ref()
            .map(|(above, _)| format!("a backport below {above}"));
        parts.extend(below.clone());
        let cuts = plan
            .cut
            .as_ref()
            .map(|cut| format!("cuts {} at {}", cut.branch, short_sha(&cut.at)));
        parts.extend(cuts.clone());
        let unfinished = format!("{tag} is tagged here already, its release gets finished");
        if plan.resumed {
            parts.push(unfinished.clone());
        }
        if let Some(why) = &outside {
            log::error(why);
            parts.push(why.clone());
        }
        let footers = log::count(refused_footers, "footer", "footers");
        if refused_footers > 0 {
            parts.push(format!("{footers} to fix"));
        }
        let failed = outside.is_some() || failed_commits || refused_footers > 0;

        let make = if plan.commits.len() == 1 {
            "makes"
        } else {
            "make"
        };
        let mut text = match plan.next {
            Some((bump, _)) => format!("{commits} {since} {make} a `{}` release.", bump.as_str()),
            None => format!("{commits} {since} {make} no release."),
        };
        if !plan.target_releases || (plan.branch.is_none() && plan.target.is_none()) {
            let mut initial = branch.chars();
            let capital: String = initial
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .collect();
            text.push_str(&format!(" {capital}{}.", initial.as_str()));
        }
        if let Some(below) = &below {
            text.push_str(&format!(" It is {below}."));
        }
        if let Some(cuts) = &cuts {
            text.push_str(&format!(" It {cuts}."));
        }
        if waits && plan.next.is_some() {
            text.push_str(" It waits for a `release:` commit.");
        }
        if plan.resumed {
            text.push_str(&format!("\n\n{unfinished}."));
        }
        if let Some(why) = &outside {
            text.push_str(&format!("\n\n{why}."));
        }
        if !unconventional.is_empty() {
            let kind = if refuse_commits { "Fix" } else { "Mind" };
            text.push_str(&format!(
            "\n\n{kind} these commits, named like `feat: add a flag` or `fix(api): handle a timeout`:\n\n{}",
            unconventional
                .iter()
                .map(|l| format!("- {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        }
        output(
            "version",
            &plan.next.map(|(_, v)| v.to_string()).unwrap_or_default(),
        );
        output("tag", &tag);
        output(
            "type",
            plan.next.map(|(b, _)| b.as_str()).unwrap_or_default(),
        );
        output("release", if releases { "true" } else { "false" });
        output(
            "cut",
            plan.cut
                .as_ref()
                .filter(|_| releases)
                .map_or("", |cut| cut.branch.as_str()),
        );
        let notes = plan.release_notes(config, "").unwrap_or_default();
        output("notes", &notes);
        if outside.is_none() {
            let mut line = branch.clone();
            for part in below.iter().chain(&cuts) {
                line.push_str(&format!(" · {part}"));
            }
            if waits && plan.next.is_some() {
                line.push_str(" · waits for a release: commit");
            }
            log::ok(&line);
        }
        let title = match (&outside, failed_commits, plan.next) {
            (Some(why), _, _) => why.clone(),
            (None, true, _) => not_conventional.clone(),
            (None, false, _) if refused_footers > 0 => format!("{footers} to fix"),
            (None, false, Some((bump, _))) => match &cuts {
                Some(cuts) => format!("{tag} · {} · {cuts}", on(bump)),
                None => format!("{tag} · {}", on(bump)),
            },
            (None, false, None) => "no release".into(),
        };
        let body = if notes.is_empty() {
            text.clone()
        } else {
            format!("{text}\n\n{notes}")
        };
        let summary = if plan.next.is_some() || failed {
            format!("<p></p>\n\n## {title}\n\n{}\n\n<p></p>", body.trim_end())
        } else {
            body.clone()
        };

        if let Some(id) = &config.check_run_id {
            let r = runner();
            match config.github_token.as_deref() {
                Some(token) => {
                    let gh = github::GitHub::new(&r.api_url, &r.repository, token);
                    match gh.update_check_run(id, &title, &body) {
                        Ok(()) => log::ok(&format!("Check run: {title}")),
                        Err(e) => log::warn(&format!("updating the check run: {e}")),
                    }
                }
                None => log::info("No github-token, so the check run keeps its title"),
            }
        }
        Ok((failed, parts, summary))
    })?;
    let head = parts.remove(0);
    log::result(
        if failed {
            log::Outcome::Fail
        } else {
            log::Outcome::Ok
        },
        &head,
        started,
        &parts,
    );
    Ok((!failed, summary))
}
