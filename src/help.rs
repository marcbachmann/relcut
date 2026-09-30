const CHECK: u8 = 1;
const PREPARE: u8 = 2;
const PUBLISH: u8 = 4;
const ALL: u8 = CHECK | PREPARE | PUBLISH;

const COMMANDS: [(&str, u8, &str, &str); 4] = [
    (
        "release",
        ALL,
        "check, prepare and publish in one go, when nothing needs to run between.",
        "Runs check, then prepare and publish. Stops after check when the branch does\nnot release or there is nothing to release, so it can run on every push, and\nfails before anything is packed or tagged when the version is outside\n--constraint. Takes the options of all three.",
    ),
    (
        "check",
        CHECK,
        "Work out the next version and its release notes. Changes nothing.",
        "Reads the conventional commits since the last version tag and works out the\nnext version and its release notes. On a pull request it applies the rules of\nthe branch it merges into, and fails on a version outside them or on a commit\nthat is not conventional. Changes nothing, so it can run on every push and\npull request.",
    ),
    (
        "prepare",
        PREPARE,
        "npm ci, set the version and npm pack, without any credentials.",
        "For npm and GitHub Packages: npm ci --ignore-scripts, npm rebuild when a\ndependency has install scripts, the version and a ci block (repository, directory,\ndate, commit, buildUrl, branch, tag) into package.json and npm pack.\nNo credential reaches any of them; --npm-install-token only npm ci.",
    ),
    (
        "publish",
        PUBLISH,
        "Tag and push, publish to the registries, create the GitHub release.",
        "Tags and pushes the release first, then publishes the packed tarball to the\nregistries, creates the GitHub release with its assets and comments on the\nreleased pull requests. What it can check, it checks before the tag. Run again\non the same commit, it finishes a release that failed after its tag.\nOutside of Actions it asks before it tags.",
    ),
];

const OPTIONS: [(&str, &str, u8); 30] = [
    (
        "--branches <patterns>",
        "Branches that release, e.g. main,release-*",
        ALL,
    ),
    (
        "--releases <mode>",
        "auto releases on every push; explicit waits for a
release: commit or a Release: footer [default: auto]",
        ALL,
    ),
    (
        "--min-bump <bump>",
        "Release at least patch, minor or major, whatever the\ncommits make [default: the commits decide]",
        ALL,
    ),
    (
        "--conventional-commits <mode>",
        "enforce fails a pull request with a commit that is not\nconventional, warn only warns [default: enforce]",
        CHECK,
    ),
    (
        "--side-effects <mode>",
        "reject fails prepare when the package's scripts leave a\nprocess running or write the runner's files; warn undoes\nit and goes on [default: reject]",
        PREPARE,
    ),
    (
        "--check-run-id <id>",
        "Write the version into this check, e.g.\n${{ job.check_run_id }}, and the release once it is\nout; needs checks: write",
        CHECK | PUBLISH,
    ),
    (
        "--constraint <vN[.N]>",
        "Refuse a version outside it, e.g. v312.0",
        ALL,
    ),
    (
        "--tag-prefix <text>",
        "Before the version in tags [default: v]",
        ALL,
    ),
    (
        "--publish <targets>",
        "npm (to publishConfig.registry); the GitHub release is always made",
        PREPARE | PUBLISH,
    ),
    (
        "--npm-install-token <token>",
        "Read token for npm ci",
        PREPARE,
    ),
    (
        "--npm-tag <tags>",
        "dist-tags [default: publishConfig.tag]",
        PUBLISH,
    ),
    (
        "--npm-stage",
        "Stage the version on npmjs for a maintainer to approve",
        PUBLISH,
    ),
    (
        "--npm-token <token>",
        "Without it npm uses trusted publishing",
        PUBLISH,
    ),
    (
        "--github-token <token>",
        "Outside of Actions: gh auth token",
        CHECK | PUBLISH,
    ),
    (
        "--github-assets <paths|json>",
        "Files for the GitHub release, or JSON with metadata:\n[{\"file\", \"name\", \"label\", \"content_type\"}]",
        PUBLISH,
    ),
    (
        "--github-artifacts <names>",
        "Artifacts of this workflow run to attach",
        PUBLISH,
    ),
    (
        "--github-comments <bool>",
        "Comment on released pull requests [default: true]",
        PUBLISH,
    ),
    (
        "--release-notes <markdown>",
        "Instead of the generated notes",
        PUBLISH,
    ),
    (
        "--release-notes-template <text>",
        "{notes} {assets} {version} {tag} {previous_tag}\n{date} {repository} {compare_url}\n[default: {notes}]",
        CHECK | PUBLISH,
    ),
    (
        "--release-title <text>",
        "{version} {tag} {date} {repository}\n[default: Version {tag}]",
        PUBLISH,
    ),
    (
        "--comment-template <text>",
        "{version} {tag} {date} {repository} {release_url}\n{npm_package} {dist_tags}, empty without npm\n[default: 🚀 Released in [`{tag}`]({release_url}),\nwith npm · `{npm_package}` on {dist_tags}]",
        PUBLISH,
    ),
    ("--working-directory <dir>", "[default: .]", ALL),
    (
        "--pack-dir <dir>",
        "[default: $RUNNER_TEMP/relcut]",
        PREPARE | PUBLISH,
    ),
    ("--keep-tarball", "Keep the tarball after publish", PUBLISH),
    (
        "--pass-env <patterns>",
        "Credential-like variables npm and git may see",
        PREPARE | PUBLISH,
    ),
    (
        "--log-style <style>",
        "auto, github-actions, plain [default: auto]",
        ALL,
    ),
    ("--dry-run", "Publish nothing", PUBLISH),
    (
        "--yes",
        "Publish without asking, outside of Actions",
        PUBLISH,
    ),
    ("-h, --help", "Print this help", ALL),
    ("-V, --version", "Print the version", ALL),
];

const FOOTER: &str =
    "Every option is also read from RELCUT_<NAME>, e.g. RELCUT_NPM_TAG; a flag wins
over its variable. Lists take a flag per value, or values separated by commas,
newlines or spaces; assets and artifacts only by commas and newlines.
Pass tokens as variables: other processes can read a command's arguments.
Each command takes every option, so one configuration serves all of them, and
ignores the ones it has no use for.";

fn options(mask: u8, all: bool) -> String {
    let width = OPTIONS
        .iter()
        .map(|(flag, _, _)| flag.len())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = String::new();
    for (flag, text, _) in OPTIONS
        .iter()
        .filter(|(_, _, on)| if all { *on == ALL } else { on & mask != 0 })
    {
        let mut lines = text.lines();
        out.push_str(&format!(
            "  {flag:<width$}{}\n",
            lines.next().unwrap_or_default()
        ));
        for line in lines {
            out.push_str(&format!("  {:<width$}{line}\n", ""));
        }
    }
    out
}

pub fn general() -> String {
    let commands: String = COMMANDS
        .iter()
        .map(|(name, _, short, _)| format!("  {name:<10}{short}\n"))
        .collect();
    format!(
        "relcut · releases from conventional commits

Usage: relcut <command> [options]

Reads the conventional commits since the last version tag, works out the next
version and releases it: a git tag, npm and GitHub Packages, a GitHub release
with notes. relcut release does it all; check, prepare and publish are its
steps, for workflows that need to run something between them.

Commands:
{commands}
Options for every command:
{}
relcut <command> --help shows the options of that command.

{FOOTER}",
        options(ALL, true)
    )
}

pub fn command(name: &str) -> Option<String> {
    let (name, mask, _, long) = COMMANDS.iter().find(|(n, ..)| *n == name)?;
    Some(format!(
        "Usage: relcut {name} [options]\n\n{long}\n\nOptions:\n{}\n{FOOTER}",
        options(*mask, false)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_lists_its_own_options() {
        let release = command("release").unwrap();
        assert!(
            release.contains("--npm-install-token")
                && release.contains("--dry-run")
                && release.contains("--release-notes-template")
        );
        let check = command("check").unwrap();
        assert!(check.contains("--constraint") && check.contains("--release-notes-template"));
        assert!(!check.contains("--npm-token") && !check.contains("--publish <"));
        let prepare = command("prepare").unwrap();
        assert!(prepare.contains("--npm-install-token") && !prepare.contains("--dry-run"));
        let publish = command("publish").unwrap();
        assert!(
            publish.contains("--dry-run")
                && publish.contains("--npm-token")
                && !publish.contains("--npm-install-token")
        );
        let general = general();
        assert!(general.contains("--branches") && !general.contains("--npm-token"));
        assert!(command("nope").is_none());
    }

    #[test]
    fn every_setting_has_a_help_line() {
        for key in crate::config::SETTINGS {
            let flag = format!("--{} ", key.to_lowercase().replace('_', "-"));
            let alone = format!("--{}", key.to_lowercase().replace('_', "-"));
            assert!(
                OPTIONS
                    .iter()
                    .any(|(f, ..)| f.starts_with(&flag) || *f == alone),
                "{key} has no help line"
            );
        }
    }
}
