use crate::config::Config;
use crate::git::Git;
use crate::plan::{Plan, releasable};
use crate::runner::runner;
use crate::{log, npm, output, pin};
use log::step;
use std::path::PathBuf;

pub fn tarball_record(config: &Config) -> PathBuf {
    config.pack_dir.join("npm-tarball")
}

pub fn npm_record(config: &Config) -> PathBuf {
    config.pack_dir.join("npm-fingerprint")
}

pub fn git_record(config: &Config) -> PathBuf {
    config.pack_dir.join("git-fingerprint")
}

pub fn prepare(config: &Config, plan: &Plan, git: &Git) -> Result<(), String> {
    let started = std::time::Instant::now();
    let Some(version) = releasable(config, plan) else {
        log::result(log::Outcome::Ok, "Nothing to prepare", started, &[]);
        return Ok(());
    };
    let steps = log::steps();
    log::section("Prepare", || {
        // Fingerprinted before the Check section printed them; held against
        // them by a publish of its own.
        let pinned = if config.npm { Some(pin::pin()?) } else { None };
        let git_pin = pin::git()?;
        git_pin.unchanged()?;
        git_pin.record(&git_record(config))?;
        let Some(pinned) = pinned else {
            return Ok(());
        };
        step(
            "Check the checkout for stored credentials",
            "No stored git credentials",
            || {
                let stored = git.stored_credentials()?;
                if stored.is_empty() {
                    return Ok(());
                }
                Err(format!(
                    "the checkout's git config holds credentials every npm script could read ({}); check out with persist-credentials: false",
                    stored.join(", ")
                ))
            },
        )?;
        let scripted = step(
            "Check the dependencies for install scripts",
            "No install scripts to run",
            || {
                let (lockfile, scripted) = npm::install_scripts(&config.dir)?;
                if scripted > 0 {
                    log::done(format!(
                        "Install scripts of {} run after the install, without a token",
                        log::count(scripted, "dependency", "dependencies")
                    ));
                }
                Ok((lockfile, scripted > 0))
            },
        )?;
        let (lockfile, scripted) = scripted;
        step("Install dependencies", "Installed dependencies", || {
            npm::install(
                pinned,
                &config.dir,
                config.npm_install_token.as_deref(),
                &config.pack_dir,
                scripted,
            )?;
            log::detail(format!("from {lockfile}"));
            Ok(())
        })?;
        let tarball = step("Pack the tarball", "Packed the tarball", || {
            let r = runner();
            let prefix = git.run(&["rev-parse", "--show-prefix"])?;
            let directory = prefix.trim().trim_end_matches('/');
            let mut ci = serde_json::json!({
                "repository": r.repo_url(),
                "date": crate::now(),
                "commit": git.head()?,
                "branch": plan.branch,
                "tag": plan.tag(config),
            });
            // As repository.directory names it: a package below the root.
            if !directory.is_empty() {
                ci["directory"] = directory.into();
            }
            if let Some(run) = crate::var("GITHUB_RUN_ID") {
                ci["buildUrl"] = format!("{}/actions/runs/{run}", r.repo_url()).into();
            }
            npm::stamp(&config.dir, &version.to_string(), ci)?;
            let [shrinkwrap, lock] = npm::LOCKFILES;
            let files = ["package.json", shrinkwrap, lock];
            let before = files.map(|f| std::fs::read(config.dir.join(f)).ok());
            let tarball = npm::pack(pinned, &config.dir, &config.pack_dir)?;
            for (file, before) in files.iter().zip(before) {
                if std::fs::read(config.dir.join(file)).ok() != before {
                    log::notice(&format!(
                        "npm pack changed {file}; the tarball holds the changed one"
                    ));
                }
            }
            if let Some(file) = pinned.changed()? {
                return Err(format!(
                    "npm gets no token: {file} changed while the package's scripts ran"
                ));
            }
            let name = npm::read(&config.dir)?["name"].as_str().map(str::to_string);
            let bytes = std::fs::metadata(&tarball).map_or(0, |m| m.len());
            log::done(format!("Packed {}@{version}", name.unwrap_or_default()));
            log::detail(format!(
                "{} · {}",
                tarball.display(),
                crate::assets::size(bytes)
            ));
            Ok(tarball)
        })?;
        std::fs::write(tarball_record(config), tarball.to_string_lossy().as_bytes())
            .map_err(|e| e.to_string())?;
        pinned.record(&npm_record(config))?;
        crate::runner_files::report()?;
        output("npm-tarball", &tarball.to_string_lossy());
        Ok(())
    })?;
    log::result(
        log::Outcome::Ok,
        &format!("Prepared {}{version}", config.tag_prefix),
        started,
        &[log::count(log::steps() - steps, "step", "steps")],
    );
    Ok(())
}
