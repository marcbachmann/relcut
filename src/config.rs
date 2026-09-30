use crate::assets;
use crate::var;
use crate::version::{Bump, Constraint};
use std::path::PathBuf;

const RELEASE_NOTES_TEMPLATE: &str = "{notes}";
// GitHub drops a leading space, not a no-break one.
const COMMENT_TEMPLATE: &str = "\u{a0}🚀\u{a0} Released in [`{tag}`]({release_url})";
const NPM_COMMENT_TEMPLATE: &str =
    "\u{a0}🚀\u{a0} Released in [`{tag}`]({release_url}) · `{npm_package}` on {dist_tags}";
const RELEASE_TITLE: &str = "Version {tag}";

pub const SETTINGS: [&str; 28] = [
    "PUBLISH",
    "BRANCHES",
    "RELEASES",
    "MIN_BUMP",
    "CONVENTIONAL_COMMITS",
    "SIDE_EFFECTS",
    "CHECK_RUN_ID",
    "CONSTRAINT",
    "NPM_TAG",
    "NPM_STAGE",
    "NPM_TOKEN",
    "NPM_INSTALL_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_ASSETS",
    "GITHUB_ARTIFACTS",
    "GITHUB_COMMENTS",
    "RELEASE_NOTES",
    "RELEASE_NOTES_TEMPLATE",
    "RELEASE_TITLE",
    "COMMENT_TEMPLATE",
    "TAG_PREFIX",
    "WORKING_DIRECTORY",
    "PACK_DIR",
    "KEEP_TARBALL",
    "PASS_ENV",
    "LOG_STYLE",
    "DRY_RUN",
    "YES",
];
const SWITCHES: [&str; 5] = [
    "DRY_RUN",
    "GITHUB_COMMENTS",
    "KEEP_TARBALL",
    "NPM_STAGE",
    "YES",
];

// Settings from flags, falling back to the environment variable of the same
// name only when the flag is not given at all.
#[derive(Debug)]
pub struct Settings(std::collections::BTreeMap<&'static str, Vec<String>>);

impl Settings {
    pub fn parse(args: &[String]) -> Result<Settings, String> {
        let mut flags = std::collections::BTreeMap::<&'static str, Vec<String>>::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let flag = arg
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument '{arg}', see relcut --help"))?;
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (flag, None),
            };
            let key = name.to_uppercase().replace('-', "_");
            let key = SETTINGS
                .iter()
                .find(|k| **k == key)
                .ok_or_else(|| format!("unknown flag --{name}, see relcut --help"))?;
            let value = match inline {
                Some(value) => value,
                None if SWITCHES.contains(key) => "true".into(),
                None => args
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value"))?
                    .clone(),
            };
            flags.entry(key).or_default().push(value);
        }
        Ok(Settings(flags))
    }

    pub fn values(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(values) => values.clone(),
            None => crate::credentials::var(&env_name(key))
                .into_iter()
                .collect(),
        }
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        let values = self.values(key);
        // Paths and artifact names may hold a space; branch names, targets,
        // dist-tags and variable names cannot.
        let spaces = !matches!(key, "GITHUB_ASSETS" | "GITHUB_ARTIFACTS");
        values
            .iter()
            .flat_map(|v| {
                v.split(move |c: char| {
                    c == ',' || c == '\n' || c == '\r' || (spaces && c.is_whitespace())
                })
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn value(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(values) => values.last().cloned().filter(|v| !v.trim().is_empty()),
            None => var(&env_name(key)),
        }
    }
}

fn env_name(key: &str) -> String {
    format!("RELCUT_{key}")
}

pub struct Config {
    pub npm: bool,
    pub branches: Vec<String>,
    pub explicit: bool,
    pub min_bump: Option<Bump>,
    pub enforce_conventional: bool,
    pub reject_side_effects: bool,
    pub check_run_id: Option<String>,
    pub constraint: Option<Constraint>,
    pub npm_tags: Vec<String>,
    pub npm_stage: bool,
    pub npm_token: Option<String>,
    pub npm_install_token: Option<String>,
    pub github_token: Option<String>,
    pub assets: Vec<assets::Asset>,
    pub artifacts: Vec<String>,
    pub comments: bool,
    pub release_notes_template: String,
    pub release_notes: Option<String>,
    pub release_title: String,
    pub comment_template: Option<String>,
    pub tag_prefix: String,
    pub dir: PathBuf,
    pub pack_dir: PathBuf,
    pub keep_tarball: bool,
    pub dry_run: bool,
    pub yes: bool,
}

impl Config {
    pub fn comment_template(&self, npm: bool) -> &str {
        match &self.comment_template {
            Some(template) => template,
            None if npm => NPM_COMMENT_TEMPLATE,
            None => COMMENT_TEMPLATE,
        }
    }

    pub fn read(settings: &Settings) -> Result<Config, String> {
        let list = |k: &str| settings.list(k);
        let value = |k: &str| settings.value(k);
        // Anything but true or false is refused: a dry run asked for with
        // `1` must not publish.
        let switch = |k: &str, default: bool| match value(k).map(|v| v.to_lowercase()).as_deref() {
            None => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(other) => Err(format!(
                "{} takes true or false, not '{other}'",
                k.to_lowercase().replace('_', "-")
            )),
        };
        let publish = list("PUBLISH");
        if publish.iter().any(|p| p == "github-packages") {
            return Err(format!(
                "publish takes npm for GitHub Packages too: set publishConfig.registry in package.json to {}",
                crate::npm::GITHUB_PACKAGES
            ));
        }
        if let Some(bad) = publish
            .iter()
            .find(|p| !["npm", "github"].contains(&p.as_str()))
        {
            return Err(format!("publish takes npm and github, not '{bad}'"));
        }
        Ok(Config {
            npm: publish.iter().any(|p| p == "npm"),
            branches: list("BRANCHES"),
            explicit: match value("RELEASES").as_deref() {
                None | Some("auto") => false,
                Some("explicit") => true,
                Some(other) => {
                    return Err(format!("releases takes auto or explicit, not '{other}'"));
                }
            },
            min_bump: value("MIN_BUMP").map(|b| Bump::parse(&b)).transpose()?,
            enforce_conventional: match value("CONVENTIONAL_COMMITS").as_deref() {
                None | Some("enforce") => true,
                Some("warn") => false,
                Some(other) => {
                    return Err(format!(
                        "conventional-commits takes enforce or warn, not '{other}'"
                    ));
                }
            },
            reject_side_effects: match value("SIDE_EFFECTS").as_deref() {
                None | Some("reject") => true,
                Some("warn") => false,
                Some(other) => {
                    return Err(format!("side-effects takes reject or warn, not '{other}'"));
                }
            },
            check_run_id: value("CHECK_RUN_ID"),
            constraint: value("CONSTRAINT")
                .map(|c| Constraint::parse(&c))
                .transpose()?,
            npm_tags: list("NPM_TAG"),
            npm_stage: switch("NPM_STAGE", false)?,
            npm_token: value("NPM_TOKEN"),
            npm_install_token: value("NPM_INSTALL_TOKEN"),
            github_token: value("GITHUB_TOKEN"),
            assets: assets::parse(&settings.values("GITHUB_ASSETS"))?,
            artifacts: list("GITHUB_ARTIFACTS"),
            comments: switch("GITHUB_COMMENTS", true)?,
            release_notes_template: value("RELEASE_NOTES_TEMPLATE")
                .unwrap_or_else(|| RELEASE_NOTES_TEMPLATE.into()),
            release_notes: value("RELEASE_NOTES"),
            release_title: value("RELEASE_TITLE").unwrap_or_else(|| RELEASE_TITLE.into()),
            comment_template: value("COMMENT_TEMPLATE"),
            tag_prefix: value("TAG_PREFIX").unwrap_or_else(|| "v".into()),
            dir: PathBuf::from(value("WORKING_DIRECTORY").unwrap_or_else(|| ".".into())),
            pack_dir: value("PACK_DIR").map(PathBuf::from).unwrap_or_else(|| {
                var("RUNNER_TEMP")
                    .map_or_else(std::env::temp_dir, PathBuf::from)
                    .join("relcut")
            }),
            keep_tarball: switch("KEEP_TARBALL", false)?,
            dry_run: switch("DRY_RUN", false)?,
            yes: switch("YES", false)?,
        })
    }
}

// `release-*` matches release-2026-09; without a `*` the name must match exactly.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_patterns() {
        assert!(glob("main", "main"));
        assert!(!glob("main", "main2"));
        assert!(glob("release-*", "release-2026-09"));
        assert!(!glob("release-*", "main"));
        assert!(glob("v*", "v1"));
        assert!(glob("release-*-09", "release-2026-09"));
        assert!(!glob("release-*-09", "release-2026-10"));
    }

    #[test]
    fn only_the_default_comment_names_npm() {
        let config = |a: &[&str]| {
            let args: Vec<String> = a.iter().map(|s| s.to_string()).collect();
            Config::read(&Settings::parse(&args).unwrap()).unwrap()
        };
        let default = config(&[]);
        assert_eq!(default.comment_template(false), COMMENT_TEMPLATE);
        assert_eq!(default.comment_template(true), NPM_COMMENT_TEMPLATE);
        assert!(COMMENT_TEMPLATE.starts_with("\u{a0}🚀\u{a0} Released in"));
        let own = config(&["--comment-template", "Out in {tag}"]);
        assert_eq!(own.comment_template(true), "Out in {tag}");
    }

    #[test]
    fn flags_win_over_the_environment() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let s = Settings::parse(&args(&[
            "--branches",
            "main",
            "--branches=release-*,v1",
            "--dry-run",
            "--tag-prefix",
            "r",
        ]))
        .unwrap();
        assert_eq!(s.list("BRANCHES"), ["main", "release-*", "v1"]);
        let lists = Settings::parse(&args(&[
            "--branches",
            "main\nrelease-*-*\r\n v1 v2",
            "--github-assets",
            "dist/a b.tgz\ndist/c.zip",
        ]))
        .unwrap();
        assert_eq!(lists.list("BRANCHES"), ["main", "release-*-*", "v1", "v2"]);
        assert_eq!(lists.list("GITHUB_ASSETS"), ["dist/a b.tgz", "dist/c.zip"]);
        assert_eq!(s.value("DRY_RUN").as_deref(), Some("true"));
        assert_eq!(s.value("TAG_PREFIX").as_deref(), Some("r"));
        assert_eq!(s.value("CONSTRAINT"), var("RELCUT_CONSTRAINT"));
        assert!(
            Settings::parse(&args(&["--nope", "x"]))
                .unwrap_err()
                .starts_with("unknown flag --nope")
        );
        assert!(
            Settings::parse(&args(&["--branches"]))
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            Settings::parse(&args(&["main"]))
                .unwrap_err()
                .starts_with("unexpected argument 'main'")
        );
    }
}
