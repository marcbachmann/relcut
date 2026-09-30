use crate::version::Bump;
use git_conventional::Commit;

pub struct Change<'a> {
    pub sha: &'a str,
    pub bump: Option<Bump>,
    pub kind: Option<&'static str>,
    pub scope: Option<&'a str>,
    pub subject: &'a str,
    pub breaking: Option<&'a str>,
    pub conventional: bool,
    // A `Release-Cut:` footer: the branch to cut, maybe empty.
    pub cut: Option<&'a str>,
    // A `release:` commit or a `Release:` footer.
    pub requests: bool,
    // A footer below the first line of the body, which relcut does not read.
    pub misplaced: Option<&'a str>,
}

const SECTIONS: [(&str, &str); 4] = [
    ("feat", "✨ Features"),
    ("fix", "🐛 Bug fixes"),
    ("perf", "⚡ Performance"),
    ("revert", "⏪ Reverts"),
];

// The angular types, `feature`, which our history uses for feat, and
// `release`, which asks for one.
pub const TYPES: [&str; 13] = [
    "feat", "feature", "fix", "perf", "revert", "docs", "refactor", "style", "test", "chore",
    "build", "ci", "release",
];

enum Footer<'a> {
    Cut(&'a str),
    Release,
}

// `Release-Cut: <branch>`, `RELEASE CUT: <branch>` or `Release: ...`, in any
// case and with `-` or a space.
fn footer(line: &str) -> Option<Footer<'_>> {
    let (token, value) = line.split_once(':')?;
    match token.trim().to_uppercase().replace('-', " ").as_str() {
        "RELEASE CUT" => Some(Footer::Cut(value.trim())),
        "RELEASE" => Some(Footer::Release),
        _ => None,
    }
}

// Only the first line of the body holds a footer: one further down is prose,
// trailers or a squashed commit's, and is reported instead.
fn footers(message: &str) -> (Option<&str>, bool, Option<&str>) {
    let body = message
        .split_once("\n\n")
        .map_or("", |(_, body)| body.trim_start_matches('\n'));
    let mut lines = body.lines();
    let (cut, requests) = match lines.next().and_then(footer) {
        Some(Footer::Cut(name)) => (Some(name), false),
        Some(Footer::Release) => (None, true),
        None => (None, false),
    };
    let misplaced = lines.map(str::trim).find(|l| footer(l).is_some());
    (cut, requests, misplaced)
}

// A breaking change is major, feat minor, fix, perf and revert patch.
pub fn analyze<'a>(sha: &'a str, message: &'a str) -> Change<'a> {
    let message = message.trim();
    let header = message.lines().next().unwrap_or_default();
    let (cut, requests, misplaced) = footers(message);
    if let Some(reverted) = header
        .strip_prefix("Revert \"")
        .and_then(|h| h.strip_suffix('"'))
    {
        return Change {
            sha,
            bump: Some(Bump::Patch),
            kind: Some("revert"),
            scope: None,
            subject: reverted,
            breaking: None,
            conventional: true,
            cut,
            requests,
            misplaced,
        };
    }
    let parsed = Commit::parse(message)
        .ok()
        .filter(|c| TYPES.iter().any(|t| c.type_() == *t));
    let Some(commit) = parsed else {
        return Change {
            sha,
            bump: None,
            kind: None,
            scope: None,
            subject: header,
            breaking: None,
            conventional: false,
            cut,
            requests,
            misplaced,
        };
    };
    let t = commit.type_();
    let kind = if t == "feat" || t == "feature" {
        Some("feat")
    } else {
        ["fix", "perf", "revert"].into_iter().find(|k| t == *k)
    };
    let bump = if commit.breaking() {
        Some(Bump::Major)
    } else {
        match kind {
            Some("feat") => Some(Bump::Minor),
            Some(_) => Some(Bump::Patch),
            None => None,
        }
    };
    Change {
        sha,
        bump,
        kind,
        scope: commit.scope().map(|s| s.as_str()),
        subject: commit.description(),
        breaking: commit.breaking().then(|| {
            commit
                .breaking_description()
                .unwrap_or(commit.description())
        }),
        conventional: true,
        cut,
        requests: requests || t == "release",
        misplaced,
    }
}

pub struct NotesContext<'a> {
    pub repo_url: &'a str,
    pub previous_tag: Option<&'a str>,
    pub tag: &'a str,
}

pub fn notes(changes: &[Change], cx: &NotesContext) -> String {
    let line = |c: &Change, text: &str| {
        let scope = c.scope.map(|s| format!("**{s}:** ")).unwrap_or_default();
        let short = crate::short_sha(c.sha);
        format!(
            "- {scope}{text} ([`{short}`]({}/commit/{}))",
            cx.repo_url, c.sha
        )
    };
    // A no-break space after the emoji keeps a gap HTML does not collapse.
    let section = |heading: &str, lines: Vec<String>| {
        let heading = heading.replacen(' ', "\u{a0} ", 1);
        (!lines.is_empty()).then(|| format!("### {heading}\n\n{}", lines.join("\n")))
    };
    let mut parts = Vec::new();
    parts.extend(section(
        "⚠️ Breaking changes",
        changes
            .iter()
            .filter_map(|c| c.breaking.map(|b| line(c, b)))
            .collect(),
    ));
    for (kind, heading) in SECTIONS {
        parts.extend(section(
            heading,
            changes
                .iter()
                .filter(|c| c.kind == Some(kind) && c.breaking != Some(c.subject))
                .map(|c| line(c, c.subject))
                .collect(),
        ));
    }
    if let Some(prev) = cx.previous_tag {
        parts.push(format!(
            "**Full changelog**: [`{prev}...{}`]({}/compare/{prev}...{})",
            cx.tag, cx.repo_url, cx.tag
        ));
    }
    parts.join("\n\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breaking_commit_without_text_of_its_own_is_listed_once() {
        let changes = [analyze("aaaaaaaaaa", "feat!: drop the old api")];
        let cx = NotesContext {
            repo_url: "https://github.com/o/r",
            previous_tag: None,
            tag: "v2.0.0",
        };
        let notes = notes(&changes, &cx);
        assert_eq!(notes.matches("drop the old api").count(), 1, "{notes}");
        assert!(
            notes.starts_with("### ⚠️\u{a0} Breaking changes\n"),
            "{notes}"
        );
    }

    fn bump(message: &str) -> Option<Bump> {
        analyze("0123456789", message).bump
    }

    #[test]
    fn cut_and_release_footers_in_any_spelling() {
        for message in [
            "chore: x\n\nRelease-Cut: release-2026-09",
            "chore: x\n\nRELEASE CUT: release-2026-09",
            "chore: x\n\n\nrelease-cut:  release-2026-09 ",
            "Update renovate.json\n\nRELEASE-CUT: release-2026-09",
            "chore: x (#12)\n\nRelease-Cut: release-2026-09\n\nCo-authored-by: A <a@x>",
        ] {
            let change = analyze("a", message);
            assert_eq!(change.cut, Some("release-2026-09"), "{message}");
            assert_eq!(change.misplaced, None, "{message}");
        }
        assert_eq!(analyze("a", "chore: x\n\nRelease-Cut:").cut, Some(""));
        // Only the first line of the body holds a footer, never the header.
        for message in ["chore: x", "Release-Cut: release-1"] {
            assert_eq!(analyze("a", message).cut, None, "{message}");
        }
        for (message, line) in [
            (
                "chore: x\n\nbody\n\nrelease-cut: release-1",
                "release-cut: release-1",
            ),
            (
                "chore: x\n\nNotes for the next\nrelease: update the docs",
                "release: update the docs",
            ),
            ("chore: x\n\nRelease: now\nRelease: again", "Release: again"),
        ] {
            let change = analyze("a", message);
            assert_eq!(change.cut, None, "{message}");
            assert_eq!(change.misplaced, Some(line), "{message}");
        }
        for message in [
            "release: now",
            "chore: x\n\nRelease: now",
            "chore: x\n\nRELEASE: yes",
        ] {
            let change = analyze("a", message);
            assert!(change.requests && change.conventional, "{message}");
            assert_eq!(change.bump, None, "{message}");
        }
        assert!(!analyze("a", "chore: x\n\nbody\nRelease: later").requests);
        assert!(!analyze("a", "chore: release notes").requests);
    }

    #[test]
    fn bumps_like_the_commit_analyzer() {
        assert_eq!(bump("feat: a"), Some(Bump::Minor));
        assert_eq!(bump("feature(api): a"), Some(Bump::Minor));
        assert_eq!(bump("Feat: a"), Some(Bump::Minor));
        assert_eq!(bump("fix: a"), Some(Bump::Patch));
        assert_eq!(bump("perf: a"), Some(Bump::Patch));
        assert_eq!(bump("revert: a"), Some(Bump::Patch));
        assert_eq!(
            bump("Revert \"feat: a\"\n\nThis reverts commit abc."),
            Some(Bump::Patch)
        );
        assert_eq!(bump("fix: a\n\nBREAKING CHANGE: gone"), Some(Bump::Major));
        assert_eq!(bump("feat!: a"), Some(Bump::Major));
        assert_eq!(bump("chore: a"), None);
        assert_eq!(bump("docs(readme): a"), None);
        assert_eq!(bump("Merge the thing"), None);
        assert!(!analyze("x", "Merge the thing").conventional);
        assert!(!analyze("x", "wip: stuff").conventional);
        assert_eq!(bump("wip: stuff"), None);
        assert!(analyze("x", "chore(deps): bump").conventional);
    }

    #[test]
    fn notes_group_by_section_with_breaking_changes_first() {
        let changes = [
            analyze("aaaaaaaaaa", "feat(api): add a thing"),
            analyze(
                "bbbbbbbbbb",
                "fix: repair it\n\nBREAKING CHANGE: the old thing is gone",
            ),
            analyze("cccccccccc", "chore: tidy"),
        ];
        let cx = NotesContext {
            repo_url: "https://github.com/o/r",
            previous_tag: Some("v1.0.0"),
            tag: "v2.0.0",
        };
        assert_eq!(
            notes(&changes, &cx),
            "### ⚠️\u{a0} Breaking changes\n\n\
             - the old thing is gone ([`bbbbbbb`](https://github.com/o/r/commit/bbbbbbbbbb))\n\n\
             ### ✨\u{a0} Features\n\n\
             - **api:** add a thing ([`aaaaaaa`](https://github.com/o/r/commit/aaaaaaaaaa))\n\n\
             ### 🐛\u{a0} Bug fixes\n\n\
             - repair it ([`bbbbbbb`](https://github.com/o/r/commit/bbbbbbbbbb))\n\n\
             **Full changelog**: [`v1.0.0...v2.0.0`](https://github.com/o/r/compare/v1.0.0...v2.0.0)\n"
        );
    }
}
