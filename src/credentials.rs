use std::collections::BTreeMap;
use std::ffi::{CStr, c_char};
use std::process::Command;
use std::sync::OnceLock;

const MARKERS: [&str; 4] = ["TOKEN", "SECRET", "PASSWORD", "_AUTH"];

static PASS: OnceLock<Vec<String>> = OnceLock::new();
static HELD: OnceLock<BTreeMap<String, String>> = OnceLock::new();

pub fn pass(patterns: Vec<String>) {
    let _ = PASS.set(patterns);
}

pub fn is_credential(name: &str) -> bool {
    let name = name.to_uppercase();
    MARKERS.iter().any(|m| name.contains(m))
}

// Moves every credential-like variable out of the environment into memory.
// ps and /proc/<pid>/environ show the environment to all the user runs, a
// dependency's scripts included, so the values are overwritten in place.
pub fn seal() {
    let mut held = BTreeMap::new();
    let credentials: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| is_credential(&key.to_string_lossy()))
        .collect();
    unsafe { scrub() };
    for (key, value) in credentials {
        unsafe { std::env::remove_var(&key) };
        held.insert(
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        );
    }
    let _ = HELD.set(held);
    undumpable();
}

unsafe fn scrub() {
    unsafe extern "C" {
        #[cfg(target_os = "macos")]
        fn _NSGetEnviron() -> *mut *mut *mut c_char;
        #[cfg(not(target_os = "macos"))]
        static mut environ: *mut *mut c_char;
    }
    unsafe {
        #[cfg(target_os = "macos")]
        let mut entry = *_NSGetEnviron();
        #[cfg(not(target_os = "macos"))]
        let mut entry = environ;
        while !entry.is_null() && !(*entry).is_null() {
            let bytes = CStr::from_ptr(*entry).to_bytes();
            if let Some(eq) = bytes.iter().position(|b| *b == b'=')
                && is_credential(&String::from_utf8_lossy(&bytes[..eq]))
            {
                std::ptr::write_bytes((*entry).add(eq + 1), 0, bytes.len() - eq - 1);
            }
            entry = entry.add(1);
        }
    }
}

#[cfg(target_os = "linux")]
mod prctl {
    use std::ffi::{c_int, c_ulong};

    const SET_DUMPABLE: c_int = 4;
    const SET_NO_NEW_PRIVS: c_int = 38;

    unsafe extern "C" {
        fn prctl(option: c_int, ...) -> c_int;
    }

    pub fn undumpable() {
        unsafe {
            prctl(
                SET_DUMPABLE,
                0 as c_ulong,
                0 as c_ulong,
                0 as c_ulong,
                0 as c_ulong,
            )
        };
    }

    pub fn no_new_privs() -> std::io::Result<()> {
        let zero = 0 as c_ulong;
        match unsafe { prctl(SET_NO_NEW_PRIVS, 1 as c_ulong, zero, zero, zero) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

// The user relcut and its commands run as, from the account database rather
// than $USER: runner · uid 501. Once: release shows it in prepare only.
pub fn user() -> Option<String> {
    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    use std::ffi::{CStr, c_char};
    // pw_name comes first in struct passwd on Linux and macOS alike.
    #[repr(C)]
    struct Passwd {
        name: *const c_char,
    }
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn getpwuid(uid: u32) -> *const Passwd;
    }
    let uid = unsafe { geteuid() };
    let entry = unsafe { getpwuid(uid) };
    let name = if entry.is_null() || unsafe { (*entry).name }.is_null() {
        "an unnamed user".to_string()
    } else {
        unsafe { CStr::from_ptr((*entry).name) }
            .to_string_lossy()
            .into_owned()
    };
    Some(format!("{name} · uid {uid}"))
}

// No process of the same user reads this one's memory or attaches to it.
fn undumpable() {
    #[cfg(target_os = "linux")]
    prctl::undumpable();
}

// What `setpriv --no-new-privs` does: the child and all it starts never gain
// privileges, so no script gets to root through sudo and past the guards.
pub fn confine(cmd: &mut Command) {
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(prctl::no_new_privs);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cmd;
}

// A variable of the environment as it was before seal().
pub fn var(key: &str) -> Option<String> {
    HELD.get()
        .and_then(|held| held.get(key).cloned())
        .or_else(|| std::env::var(key).ok())
}

fn passes(key: &str) -> bool {
    let pass = PASS.get().map(Vec::as_slice).unwrap_or_default();
    pass.iter().any(|p| crate::config::glob(p, key))
}

// No child sees a variable that looks like a credential, unless --pass-env
// names it. What a child needs on purpose, the caller sets after this.
pub fn strip(cmd: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if is_credential(&key) && !passes(&key) {
            cmd.env_remove(&*key);
        }
    }
    for (key, value) in HELD.get().into_iter().flatten() {
        if passes(key) {
            cmd.env(key, value);
        }
    }
}

// For a child that is meant to see these, whatever --pass-env says.
pub fn restore(cmd: &mut Command, keys: &[&str]) {
    for key in keys {
        if let Some(value) = var(key) {
            cmd.env(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_by_name() {
        for name in [
            "GITHUB_TOKEN",
            "relcut_npm_token",
            "npm_config_//registry.npmjs.org/:_authToken",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "NPM_CONFIG__AUTH",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
        ] {
            assert!(is_credential(name), "{name}");
        }
        for name in ["PATH", "HOME", "NODE_OPTIONS", "AUTHOR"] {
            assert!(!is_credential(name), "{name}");
        }
    }
}
