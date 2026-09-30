use std::ffi::c_int;
use std::process::{Command, Stdio};

unsafe extern "C" {
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
}

const SIGKILL: c_int = 9;
const WNOHANG: c_int = 1;
const ROUNDS: usize = 200;

// A script's process that outlives its npm reparents to relcut rather than
// to init, so relcut can find it and end it. Linux only.
pub fn adopt_orphans() {
    #[cfg(target_os = "linux")]
    {
        const SET_CHILD_SUBREAPER: c_int = 36;
        unsafe extern "C" {
            fn prctl(option: c_int, ...) -> c_int;
        }
        let zero = 0 as std::ffi::c_ulong;
        unsafe {
            prctl(
                SET_CHILD_SUBREAPER,
                1 as std::ffi::c_ulong,
                zero,
                zero,
                zero,
            )
        };
    }
}

// The child leads a process group of its own, so what it started is one
// signal away once it returns. Away from the terminal's group it must not
// read the terminal, and no script of a package should.
pub fn in_own_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0).stdin(Stdio::null());
}

// Once a child has returned: everything still in its process group, and on
// Linux everything descended from relcut, round after round until a round
// finds nothing. Ok(true) when there was anything; Err when something is
// still there after the last round, which must not meet a token.
pub fn sweep(child: u32) -> Result<bool, String> {
    let group = -(child as c_int);
    let mut found = false;
    for _ in 0..ROUNDS {
        let mut alive = unsafe { kill(group, 0) } == 0;
        if alive {
            unsafe { kill(group, SIGKILL) };
        }
        for pid in descendants() {
            unsafe { kill(pid as c_int, SIGKILL) };
            alive = true;
        }
        reap();
        if !alive {
            return Ok(found);
        }
        found = true;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Err("a process the package's scripts started could not be ended".into())
}

// Every process whose parent chain reaches relcut; a script's process that
// forks on keeps its parent alive, so a scan for direct orphans is not enough.
#[cfg(target_os = "linux")]
fn descendants() -> Vec<u32> {
    let me = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let parents: std::collections::HashMap<u32, u32> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // `pid (comm) state ppid …`; comm may hold spaces and parentheses.
            let ppid = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, ppid))
        })
        .collect();
    parents
        .keys()
        .copied()
        .filter(|pid| {
            let mut at = *pid;
            while let Some(parent) = parents.get(&at) {
                if *parent == me {
                    return true;
                }
                if *parent <= 1 || *parent == at {
                    break;
                }
                at = *parent;
            }
            false
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn descendants() -> Vec<u32> {
    Vec::new()
}

fn reap() {
    let mut status = 0;
    while unsafe { waitpid(-1, &mut status, WNOHANG) } > 0 {}
}
