use std::sync::OnceLock;

static REJECT: OnceLock<bool> = OnceLock::new();

pub fn set(reject: bool) {
    let _ = REJECT.set(reject);
}

// A side effect of the package's scripts that relcut has undone: a process
// left running, a line in the runner's files. With
// `side-effects: reject` the release stops there; with `warn` it goes on.
pub fn found(what: &str) -> Result<(), String> {
    if REJECT.get().copied().unwrap_or(true) {
        return Err(format!("{what}; side-effects is reject, warn would go on"));
    }
    crate::log::notice(what);
    Ok(())
}
