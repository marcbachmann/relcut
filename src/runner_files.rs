use std::path::PathBuf;
use std::sync::Mutex;

// The files a step writes for the runner to apply to the steps after it.
const FILES: [&str; 5] = [
    "GITHUB_ENV",
    "GITHUB_PATH",
    "GITHUB_OUTPUT",
    "GITHUB_STATE",
    "GITHUB_STEP_SUMMARY",
];

static WRITTEN: Mutex<Vec<(&'static str, String)>> = Mutex::new(Vec::new());

pub struct Snapshot {
    files: Vec<(&'static str, PathBuf, Vec<u8>)>,
}

// A script finds these files by listing their directory, whatever its
// environment says; what it wrote goes nowhere but the outputs.
pub fn take() -> Snapshot {
    let files = FILES
        .iter()
        .filter_map(|name| {
            let path = PathBuf::from(crate::var(name)?);
            let content = std::fs::read(&path).ok()?;
            Some((*name, path, content))
        })
        .collect();
    Snapshot { files }
}

impl Snapshot {
    // The file is put back as a new plain file, never through what is at the
    // path now: a symlink, a directory or a hard link a script left there
    // would take relcut's own outputs elsewhere.
    pub fn restore(self) {
        for (name, path, before) in self.files {
            let plain = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
            let now = std::fs::read(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            if !plain {
                let _ = std::fs::remove_dir_all(&path);
            }
            let _ = std::fs::write(&path, &before);
            if plain && now == before {
                continue;
            }
            let added = now.strip_prefix(before.as_slice()).unwrap_or(&now);
            WRITTEN
                .lock()
                .unwrap()
                .push((name, String::from_utf8_lossy(added).into_owned()));
        }
    }
}

pub fn keep_out(cmd: &mut std::process::Command) {
    for name in FILES {
        cmd.env_remove(name);
    }
}

// Once the scripts are done: a notice, and the env and path lines as
// outputs for a workflow that wants them.
pub fn report() -> Result<(), String> {
    let written = std::mem::take(&mut *WRITTEN.lock().unwrap());
    if written.is_empty() {
        return Ok(());
    }
    let mut names: Vec<&str> = written.iter().map(|(name, _)| *name).collect();
    names.dedup();
    crate::side_effects::found(&format!(
        "the package's scripts wrote to {}; put back",
        names.join(", ")
    ))?;
    crate::log::info("their lines are the outputs scripts-env and scripts-path");
    for (name, output) in [
        ("GITHUB_ENV", "scripts-env"),
        ("GITHUB_PATH", "scripts-path"),
    ] {
        let lines: String = written
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, text)| text.as_str())
            .collect();
        crate::output(output, lines.trim_end());
    }
    Ok(())
}
