use crate::commits::{Change, NotesContext, analyze, notes};
use crate::config::{Config, glob};
use crate::git::Git;
use crate::runner::runner;
use crate::version::{Bump, Constraint, Version};
use crate::{fill, short_sha, today};

// A branch a `Release-Cut:` footer creates with this release, at the parent
// of its commit: it keeps the line this release leaves.
pub struct Cut {
    pub branch: String,
    pub sha: String,
    pub at: String,
    // The highest bump of the commits between the last release and `at`,
    // which the branch releases on its own.
    pub carries: Option<Bump>,
}

pub struct Plan {
    pub branch: Option<String>,
    // The branch whose rules apply: the pushed one, or the one a pull
    // request merges into.
    pub target: Option<String>,
    pub target_releases: bool,
    pub releasing: bool,
    // The target is the repository's default branch.
    pub trunk: bool,
    // The lowest version above the last release that exists outside this
    // branch's history: the branch releases below it, as a backport.
    pub ceiling: Option<(String, Version)>,
    pub constraint: Option<Constraint>,
    pub cut: Option<Cut>,
    // With releases: explicit, a commit asked for this release.
    pub requested: bool,
    // Footers relcut does not follow, by commit, and why.
    pub ignored: Vec<(String, String)>,
    // Footers below the first line of the body, which relcut does not read.
    pub misplaced: Vec<(String, String)>,
    pub last: Option<(String, Version)>,
    pub commits: Vec<(String, String)>,
    pub next: Option<(Bump, Version)>,
    // The next version's tag is on this commit already, from a run that
    // failed after it: this run finishes that release.
    pub resumed: bool,
}

fn bump(commits: &[(String, String)]) -> Option<Bump> {
    commits
        .iter()
        .filter_map(|(sha, msg)| analyze(sha, msg).bump)
        .max()
}

// The releases outside the history above the last one. The lowest is the
// ceiling the branch releases below. On the trunk a patch of its last release
// is a backport of its line instead (true): the trunk releases above that line.
pub fn outside(
    last: Option<Version>,
    tags: impl IntoIterator<Item = (String, Version)>,
    trunk: bool,
) -> (Option<(String, Version)>, bool) {
    let mut backported = false;
    let ceiling = tags
        .into_iter()
        .filter(|(_, v)| last.is_none_or(|l| *v > l))
        .filter(|(_, v)| {
            let backport = trunk && last.is_some_and(|l| (v.major, v.minor) == (l.major, l.minor));
            backported |= backport;
            !backport
        })
        .min_by_key(|(_, v)| *v);
    (ceiling, backported)
}

// A branch that exists is the one a cut made when it holds `at` but not the
// commit that cuts it: a rerun finds it so, maybe with fixes on it.
pub fn made_by_cut(git: &Git, cut_sha: &str, at: &str, tip: &str) -> bool {
    git.is_ancestor(at, tip) && !git.is_ancestor(cut_sha, tip)
}

struct Outcome {
    ceiling: Option<(String, Version)>,
    cut: Option<Cut>,
    requested: bool,
    ignored: Vec<(String, String)>,
    misplaced: Vec<(String, String)>,
    next: Option<(Bump, Version)>,
}

// What the commits since `last` release, and what their footers ask for.
fn evaluate(
    config: &Config,
    git: &Git,
    // Footers apply where the target releases.
    target: Option<&str>,
    trunk: bool,
    foreign: &[(String, Version)],
    last: &Option<(String, Version)>,
    commits: &[(String, String)],
) -> Result<Outcome, String> {
    let last_version = last.as_ref().map(|(_, v)| *v);
    let (ceiling, backported) = outside(last_version, foreign.iter().cloned(), trunk);
    let name_of_target = target.unwrap_or_default();
    let (mut cut, mut requested, mut ignored) = (None::<Cut>, false, Vec::new());
    let mut misplaced = Vec::new();
    let footers = if target.is_some() { commits } else { &[] };
    let cuts = footers
        .iter()
        .any(|(sha, msg)| analyze(sha, msg).cut.is_some());
    // A pull request's merge ref has the base first: the line is the head's.
    let line = match runner().base {
        Some(_)
            if cuts
                && git
                    .run(&["rev-parse", "--verify", "--quiet", "HEAD^2"])
                    .is_ok() =>
        {
            "HEAD^2"
        }
        _ => "HEAD",
    };
    let on_line = match last {
        Some((tag, _)) if cuts => git.first_parents(Some(tag), line)?,
        _ => Vec::new(),
    };
    for (sha, message) in footers.iter().rev() {
        let change = analyze(sha, message);
        if let Some(line) = change.misplaced {
            misplaced.push((
                sha.clone(),
                format!("{line} is a footer on the first line of the body only"),
            ));
        }
        if change.requests && !config.explicit {
            ignored.push((
                sha.clone(),
                "release: asks for a release, which only releases: explicit waits for".into(),
            ));
        }
        requested |= change.requests && config.explicit;
        let Some(name) = change.cut else { continue };
        let at = git
            .run(&["rev-parse", "--verify", "--quiet", &format!("{sha}^1")])
            .map(|at| at.trim().to_string());
        let why = if name.is_empty() {
            Some("Release-Cut names no branch".to_string())
        } else if git
            .run(&["check-ref-format", &format!("refs/heads/{name}")])
            .is_err()
        {
            Some(format!("Release-Cut: {name} is no branch name git takes"))
        } else if !config.branches.iter().any(|p| glob(p, name)) {
            Some(format!("Release-Cut: {name} is no branch of branches"))
        } else if name == name_of_target {
            Some(format!("Release-Cut: {name} is the branch that releases"))
        } else if last.is_some() && !on_line.contains(sha) {
            // Its parent is the merged branch's, not a commit of the target.
            Some(format!(
                "Release-Cut: {name} came in through a merge, and only a commit of {name_of_target} itself cuts"
            ))
        } else if ceiling.is_some() {
            Some(format!("{name_of_target} is a backport and cuts no {name}"))
        } else if let Some(first) = &cut {
            Some(format!(
                "{} cuts {} already, one cut per release",
                short_sha(&first.sha),
                first.branch
            ))
        } else if let (Ok(at), Some(tip)) = (&at, git.branch_tip(name))
            && !made_by_cut(git, sha, at, &tip)
        {
            Some(format!(
                "Release-Cut: {name} exists already, at {}",
                short_sha(&tip)
            ))
        } else {
            None
        };
        match (why, at, last) {
            (Some(why), _, _) => ignored.push((sha.clone(), why)),
            (None, Ok(at), Some((tag, _))) => {
                let carries = bump(&git.commits_between(Some(tag), &at)?);
                cut = Some(Cut {
                    branch: name.to_string(),
                    sha: sha.clone(),
                    at,
                    carries,
                });
            }
            _ => ignored.push((
                sha.clone(),
                format!("{name} has no release to keep: cut it after the first release"),
            )),
        }
    }
    requested |= config.explicit && cut.is_some();
    let floor = [
        config.min_bump,
        requested.then_some(Bump::Patch),
        cut.as_ref().map(|_| Bump::Minor),
    ]
    .into_iter()
    .flatten()
    .max();
    // A backport of the trunk's line raises a release, it makes none.
    // Without a commit there is nothing to release, whatever min-bump asks.
    let raised = bump(commits)
        .max(floor)
        .filter(|_| !commits.is_empty())
        .map(|b| if backported { b.max(Bump::Minor) } else { b });
    let next = raised.map(|bump| {
        let version = last_version.map_or(Version::FIRST, |v| v.bump(bump));
        // Above the line the cut branch keeps, with what it carries.
        let above = cut.as_ref().zip(last_version).map(|(cut, v)| {
            cut.carries
                .map_or(v, |carried| v.bump(carried))
                .bump(Bump::Minor)
        });
        (bump, version.max(above.unwrap_or(version)))
    });
    Ok(Outcome {
        ceiling,
        cut,
        requested,
        ignored,
        misplaced,
        next,
    })
}

impl Plan {
    pub fn read(config: &Config, git: &Git) -> Result<Plan, String> {
        let branch = runner().branch.clone();
        let target = branch.clone().or_else(|| runner().base.clone());
        let target_releases = target
            .as_deref()
            .is_some_and(|b| config.branches.iter().any(|p| glob(p, b)));
        let releasing = target_releases && branch.is_some();
        let trunk = target.is_some() && target == runner().default_branch;
        let tag_of = |release: &Option<(String, Version)>| release.as_ref().map(|(t, _)| t.clone());
        let foreign = git.foreign_releases(&config.tag_prefix)?;
        let releases_on = target.as_deref().filter(|_| target_releases);
        let evaluate = |last: &Option<(String, Version)>, commits: &[(String, String)]| {
            evaluate(config, git, releases_on, trunk, &foreign, last, commits)
        };
        let mut releases = git.releases(&config.tag_prefix)?;
        let mut last = releases.pop();
        let mut commits = git.commits_since(tag_of(&last).as_deref())?;
        let mut outcome = evaluate(&last, &commits)?;
        let mut resumed = false;
        if releasing && let Some((tag, version)) = &last {
            let at_head = git.tags_at_head()?;
            if at_head.contains(tag) {
                releases.retain(|(t, _)| !at_head.contains(t));
                let before = releases.pop();
                let made = git.commits_since(tag_of(&before).as_deref())?;
                let earlier = evaluate(&before, &made)?;
                if earlier.next.is_some_and(|(_, v)| v == *version) {
                    (last, commits, outcome, resumed) = (before, made, earlier, true);
                }
            }
        }
        Ok(Plan {
            branch,
            target,
            target_releases,
            releasing,
            trunk,
            ceiling: outcome.ceiling,
            constraint: config.constraint,
            cut: outcome.cut,
            requested: outcome.requested,
            ignored: outcome.ignored,
            misplaced: outcome.misplaced,
            last,
            commits,
            next: outcome.next,
            resumed,
        })
    }

    pub fn changes(&self) -> Vec<Change<'_>> {
        self.commits
            .iter()
            .map(|(sha, msg)| analyze(sha, msg))
            .collect()
    }

    pub fn tag(&self, config: &Config) -> Option<String> {
        self.next.map(|(_, v)| format!("{}{v}", config.tag_prefix))
    }

    pub fn release_notes(&self, config: &Config, card: &str) -> Option<String> {
        let (_, version) = self.next?;
        let tag = self.tag(config)?;
        let repo_url = runner().repo_url();
        let previous_tag = self.last.as_ref().map(|(t, _)| t.as_str());
        let notes = notes(
            &self.changes(),
            &NotesContext {
                repo_url: &repo_url,
                previous_tag,
                tag: &tag,
            },
        );
        let compare_url = previous_tag
            .map(|p| format!("{repo_url}/compare/{p}...{tag}"))
            .unwrap_or_default();
        Some(
            fill(
                &config.release_notes_template,
                &[
                    ("notes", notes.trim_end()),
                    ("version", &version.to_string()),
                    ("tag", &tag),
                    ("previous_tag", previous_tag.unwrap_or_default()),
                    ("date", &today()),
                    ("repository", &runner().repository),
                    ("compare_url", &compare_url),
                    ("assets", card),
                ],
            )
            .trim_end()
            .to_string()
                + "\n",
        )
    }
}

// Why the next version must not be released, if it must not.
pub fn violation(config: &Config, plan: &Plan) -> Option<String> {
    let (_, version) = plan.next?;
    let tag = format!("{}{version}", config.tag_prefix);
    match (plan.constraint, &plan.ceiling) {
        (Some(c), _) if !c.allows(version) => Some(format!("{tag} is outside {c}")),
        (_, Some((above, at))) if version >= *at => Some(format!(
            "{tag} is not below {above}, which exists already: {} releases below it",
            plan.target.as_deref().unwrap_or_default()
        )),
        _ => None,
    }
}

// With releases: explicit, the version waits for a commit that asks for it.
pub fn waiting(config: &Config, plan: &Plan) -> bool {
    config.explicit && !plan.requested
}

pub fn releasable(config: &Config, plan: &Plan) -> Option<Version> {
    let (_, version) = plan.next?;
    (plan.releasing && !waiting(config, plan) && violation(config, plan).is_none())
        .then_some(version)
}
