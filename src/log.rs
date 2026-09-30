use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, IsTerminal, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// The log is a flat list of sections, a line per step, and what the steps
// printed below them, collapsed in Actions; then a footer with the result:
//
// ── Prepare ─────────────────────────────────
//    No stored git credentials · 0.0s
//    Installed dependencies · package-lock.json · 0.2s
// ▸ Install dependencies
//
// ──────────────────────────────────────────
//    Released v1.1.0 · npm, github · 1.0s
// ──────────────────────────────────────────

static ACTIONS: OnceLock<bool> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();
static LAST_BLANK: AtomicBool = AtomicBool::new(true);
static STEPS: AtomicUsize = AtomicUsize::new(0);
// Steps that passed without a line of their own, counted for one line.
static QUIET: AtomicBool = AtomicBool::new(false);
static PASSED: AtomicUsize = AtomicUsize::new(0);
// A section's title, printed with its first line: an empty one prints nothing.
static TITLE: Mutex<Option<String>> = Mutex::new(None);
static OPEN: AtomicBool = AtomicBool::new(false);
// A command's result ends its section, or the log as the footer when nothing
// follows it.
static RESULT: Mutex<Option<String>> = Mutex::new(None);
static FACTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
// What the section's steps printed, by step, and whether the step failed.
static LOGS: Mutex<Vec<(String, Vec<String>, bool)>> = Mutex::new(Vec::new());
// Listed once more above the line a command ends with.
static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

thread_local! {
    static STEP: Cell<bool> = const { Cell::new(false) };
    static PRINTED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static DETAIL: RefCell<Option<String>> = const { RefCell::new(None) };
    static DONE: RefCell<Option<String>> = const { RefCell::new(None) };
    static RETRIES: Cell<usize> = const { Cell::new(0) };
}

fn in_actions() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v.trim() == "true")
}

fn actions() -> bool {
    *ACTIONS.get_or_init(in_actions)
}

fn started() -> Instant {
    *STARTED.get_or_init(Instant::now)
}

pub fn init(style: Option<&str>) -> Result<(), String> {
    started();
    let actions = match style.unwrap_or("auto") {
        "auto" => in_actions(),
        "github-actions" => true,
        "plain" => false,
        other => {
            return Err(format!(
                "log-style takes auto, github-actions or plain, not '{other}'"
            ));
        }
    };
    let _ = ACTIONS.set(actions);
    Ok(())
}

fn color() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none() && (actions() || std::io::stdout().is_terminal())
    })
}

fn paint(code: &str, text: &str) -> String {
    if color() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn red(t: &str) -> String {
    paint("31", t)
}
pub fn yellow(t: &str) -> String {
    paint("33", t)
}
pub fn dim(t: &str) -> String {
    paint("2", t)
}

fn separator() -> String {
    format!("{} ", dim(" ·"))
}

// 1 commit, 3 commits.
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// Paths under the working directory or the runner's temp directory, as the
// part below it: relcut/x.tgz, not /home/runner/work/_temp/relcut/x.tgz.
pub fn short(text: &str) -> String {
    let mut text = text.to_string();
    let roots = [
        std::env::current_dir().ok(),
        std::env::var_os("RUNNER_TEMP").map(Into::into),
        Some(std::env::temp_dir()),
    ];
    for root in roots.into_iter().flatten() {
        let root = root.to_string_lossy().trim_end_matches('/').to_string();
        if root.len() > 1 {
            text = text.replace(&format!("{root}/"), "");
        }
    }
    text
}

// The runner reads a line as a workflow command when it starts with `::`
// after its whitespace, or holds `##[` anywhere, and a lone `\r` ends a line
// for it. The name it looks up starts right after either and ends at a space,
// so a space there leaves an empty name, which no command has, however its
// culture-aware matching treats invisible characters.
fn defuse(line: &str) -> Vec<String> {
    line.split(['\r', '\n'])
        .map(|part| part.replace("::", ":: ").replace("##[", "##[ "))
        .collect()
}

fn emit(line: &str) {
    LAST_BLANK.store(line.trim().is_empty(), Ordering::Relaxed);
    if line.trim().is_empty() {
        println!();
    } else if actions() {
        for part in defuse(line) {
            println!("{part}");
        }
    } else {
        println!("{line}");
    }
}

// relcut's own workflow commands, the only lines that may be one.
fn emit_command(line: &str) {
    LAST_BLANK.store(false, Ordering::Relaxed);
    println!("{line}");
}

fn emit_err(line: &str) {
    LAST_BLANK.store(false, Ordering::Relaxed);
    eprintln!("{line}");
}

// At most one blank line in a row, and none at the very start.
fn gap() {
    if !LAST_BLANK.load(Ordering::Relaxed) {
        LAST_BLANK.store(true, Ordering::Relaxed);
        println!();
    }
}

// The columns: a mark, then text that lines up whether it has one or not, then
// what a command prints.
fn marked(mark: &str, text: &str) -> String {
    format!("{mark}  {text}")
}
fn plain(text: &str) -> String {
    format!("   {text}")
}
fn quoted(text: &str) -> String {
    format!("     {text}")
}

fn width(text: &str) -> usize {
    let mut n = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|c| *c == 'm');
        } else {
            n += 1;
        }
    }
    n
}

fn rule(width: usize) -> String {
    dim(&"─".repeat(width))
}

fn end_section() {
    if !OPEN.swap(false, Ordering::Relaxed) {
        return;
    }
    if printed() && !actions() {
        gap();
    }
    if let Some(result) = RESULT.lock().unwrap().take() {
        emit(&result);
    }
    gap();
}

// What the section's steps printed, below their lines: a collapsed group per
// step in Actions, unless it failed. Whether there was any.
fn printed() -> bool {
    let logs = std::mem::take(&mut *LOGS.lock().unwrap());
    let any = !logs.is_empty();
    if !actions() && any {
        gap();
    }
    for (title, lines, failed) in logs {
        // Each ends with a blank line, inside its group.
        if actions() && !failed {
            emit_command(&format!("::group::{}", title.replace(['\r', '\n'], " ")));
            lines.iter().for_each(|l| emit(l));
            emit("");
            emit_command("::endgroup::");
        } else {
            emit(&dim(&format!("▸ {title}")));
            lines.iter().for_each(|l| emit(l));
            emit("");
        }
    }
    any
}

// The section's title, once something goes into it.
fn begin() {
    let Some(title) = TITLE.lock().unwrap().take() else {
        return;
    };
    end_section();
    if let Some(result) = RESULT.lock().unwrap().take() {
        emit(&result);
        gap();
    }
    gap();
    let head = format!("── {title} ");
    emit(&dim(&format!(
        "{head}{}",
        "─".repeat(48usize.saturating_sub(width(&head)))
    )));
    OPEN.store(true, Ordering::Relaxed);
}

// A line a step prints goes below the section's lines, any other in place;
// `depth` 1 is what a command printed.
fn put(depth: usize, line: &str) {
    if STEP.with(Cell::get) {
        let line = format!("{}{line}", "  ".repeat(depth + 1));
        PRINTED.with(|p| p.borrow_mut().push(line));
    } else {
        begin();
        emit(&if depth == 0 {
            plain(line)
        } else {
            quoted(line)
        });
    }
}

// Between a command and its output, and before the next command.
fn blank() {
    PRINTED.with(|p| {
        let mut p = p.borrow_mut();
        if STEP.with(Cell::get) && p.last().is_some_and(|l| !l.trim().is_empty()) {
            p.push(String::new());
        }
    });
}

// A section: its steps and lines, and the result that ends it.
pub fn section<T>(title: &str, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    *TITLE.lock().unwrap() = Some(title.to_string());
    run()
}

pub fn info(message: &str) {
    for line in message.lines() {
        put(0, line);
    }
}

// `packages/x $ npm ci` for a command that runs outside the working directory.
pub fn command(dir: Option<&Path>, line: &str) {
    let at = dir
        .and_then(elsewhere)
        .map_or(String::new(), |d| format!("{d} "));
    blank();
    info(&dim(&format!("{at}$ {}", short(line))));
    blank();
}

// The directory, when it is not the working directory.
pub fn elsewhere(dir: &Path) -> Option<String> {
    let here = std::env::current_dir().ok()?;
    let there = dir.canonicalize().ok()?;
    (there != here.canonicalize().ok()?).then(|| short(&dir.display().to_string()))
}

// Lines below the section's own, collapsed in Actions like a step's output.
pub fn collapsed(title: &str, lines: &[String]) {
    begin();
    let lines = lines.iter().map(|l| format!("  {l}")).collect();
    LOGS.lock().unwrap().push((title.to_string(), lines, false));
}

// Below the line that introduces it and a blank one, like a command's output.
pub fn markdown(text: &str) {
    blank();
    for line in text.trim_end().lines() {
        put(1, line);
    }
}

pub fn retry(message: &str) {
    RETRIES.with(|r| r.set(r.get() + 1));
    info(message);
}

// What the status line of the current step shows after its `·`.
pub fn detail(text: impl Into<String>) {
    DETAIL.with(|d| *d.borrow_mut() = Some(short(&text.into())));
}

// The status line of the current step, in place of its `done`.
pub fn done(text: impl Into<String>) {
    DONE.with(|d| *d.borrow_mut() = Some(text.into()));
}

// A line of the footer below the result: `npm   @acme/x@1.1.0 on latest`.
pub fn fact(label: &str, value: &str) {
    FACTS
        .lock()
        .unwrap()
        .push((label.to_string(), short(value)));
}

// Shown in place within a step, and in the summary at the end either way.
pub fn warn(message: &str) {
    WARNINGS.lock().unwrap().push(message.to_string());
    if STEP.with(Cell::get) && !actions() {
        put(0, &marked(&yellow("▲"), message));
    }
}

pub fn error(message: &str) {
    ERRORS.lock().unwrap().push(message.to_string());
}

pub fn notice(message: &str) {
    begin();
    if actions() {
        annotate("notice", message);
    } else {
        emit(&marked(&dim("•"), message));
    }
}

pub fn annotate(level: &str, message: &str) {
    emit_command(&format!(
        "::{level}::{}",
        message
            .replace('%', "%25")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    ));
}

// The warnings and errors once more, above the result.
fn summary() {
    let warnings = std::mem::take(&mut *WARNINGS.lock().unwrap());
    let errors = std::mem::take(&mut *ERRORS.lock().unwrap());
    if warnings.is_empty() && errors.is_empty() {
        return;
    }
    begin();
    // A message's first line takes the mark, the rest line up below it.
    let lines = |mark: &str, message: &str| -> Vec<String> {
        let mut lines = message.lines();
        let first = marked(mark, lines.next().unwrap_or_default());
        std::iter::once(first).chain(lines.map(plain)).collect()
    };
    for warning in warnings {
        if actions() {
            annotate("warning", &warning);
        } else {
            lines(&yellow("▲"), &warning).iter().for_each(|l| emit(l));
        }
    }
    for error in errors {
        if actions() {
            annotate("error", &error);
        } else {
            lines(&red("●"), &error).iter().for_each(|l| emit_err(l));
        }
    }
}

pub enum Outcome {
    Ok,
    Warn,
    Fail,
}

// The line a command ends with: in its section when another one follows, or
// the footer.
//     v1.1.0 · 3 commits since v1.0.0 · 0.0s
pub fn result(outcome: Outcome, head: &str, started: Instant, parts: &[String]) {
    summary();
    let rest: String = parts
        .iter()
        .map(|p| format!("{}{p}", separator()))
        .collect();
    let text = format!(
        "{head}{rest}{}{}",
        separator(),
        dim(&seconds(started.elapsed()))
    );
    let line = match outcome {
        Outcome::Ok => plain(&text),
        Outcome::Warn => marked(&yellow("▲"), &text),
        Outcome::Fail => marked(&red("●"), &text),
    };
    let mut held = RESULT.lock().unwrap();
    if let Some(earlier) = held.replace(line) {
        emit(&earlier);
    }
}

fn footer(line: &str, to_stderr: bool) {
    end_section();
    let facts = std::mem::take(&mut *FACTS.lock().unwrap());
    let label = facts.iter().map(|(l, _)| width(l)).max().unwrap_or(0);
    let mut body = vec![String::new(), line.to_string()];
    if !facts.is_empty() {
        body.push(String::new());
        let mut last = "";
        for (name, value) in &facts {
            let shown = if name == last { "" } else { name.as_str() };
            body.push(plain(&format!("{shown:<label$}   {value}")));
            last = name;
        }
    }
    body.push(String::new());
    let wide = body.iter().map(|l| width(l)).max().unwrap_or(0).max(47) + 1;
    gap();
    let print = |l: &str| if to_stderr { emit_err(l) } else { emit(l) };
    print(&rule(wide));
    for line in &body {
        print(line);
    }
    print(&rule(wide));
}

// Closes the last section and prints the result that ended the command.
pub fn finish() {
    summary();
    let line = RESULT.lock().unwrap().take();
    if let Some(line) = line {
        footer(&line, false);
    } else {
        end_section();
    }
}

// The footer of a command that stopped, in place of its result.
pub fn fail(head: &str, error: &str) {
    summary();
    if actions() {
        annotate("error", &format!("{head}: {error}"));
    }
    RESULT.lock().unwrap().take();
    let line = marked(
        &red("●"),
        &format!(
            "{head}{}{error}{}{}",
            separator(),
            separator(),
            dim(&seconds(started().elapsed()))
        ),
    );
    footer(&line, !actions());
}

pub fn steps() -> usize {
    STEPS.load(Ordering::Relaxed)
}

fn seconds(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        format!("{}m {}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

// The child's stdout and stderr, line by line, below the command.
pub fn run(cmd: &mut Command, scripts: bool) -> std::io::Result<ExitStatus> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let forward = |stream: Box<dyn Read + Send>, tx: std::sync::mpsc::Sender<String>| {
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                let _ = tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
                line.clear();
            }
        })
    };
    let out = forward(Box::new(child.stdout.take().unwrap()), tx.clone());
    let err = forward(Box::new(child.stderr.take().unwrap()), tx);
    for line in rx {
        if !line.trim().is_empty() {
            put(1, &short(&line));
        }
    }
    let _ = (out.join(), err.join());
    let status = child.wait();
    if scripts {
        match crate::leftover::sweep(child.id()) {
            Ok(true) => {
                crate::side_effects::found("the package's scripts left process(es) running, killed")
                    .map_err(std::io::Error::other)?
            }
            Ok(false) => {}
            Err(e) => return Err(std::io::Error::other(e)),
        }
    }
    status
}

// A line of a section that is no step:    relcut-linux.tgz · 2.1 MB
pub fn ok(text: &str) {
    if QUIET.load(Ordering::Relaxed) {
        PASSED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    begin();
    emit(&plain(&short(text)));
}

// Steps that show only when they fail or print, then one line for all:
//     Ready to tag v1.1.0 · 6 checks passed · 0.1s
pub fn quietly<T>(done: &str, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let started = Instant::now();
    PASSED.store(0, Ordering::Relaxed);
    QUIET.store(true, Ordering::Relaxed);
    let result = run();
    QUIET.store(false, Ordering::Relaxed);
    if result.is_ok() {
        let passed = count(PASSED.load(Ordering::Relaxed), "check", "checks");
        begin();
        emit(&plain(&format!(
            "{done}{}{passed} passed{}{}",
            separator(),
            separator(),
            dim(&seconds(started.elapsed()))
        )));
    }
    result
}

// One line for a step, its detail, retries and duration after the `·`; what
// it prints goes below the section's lines:
//     Tagged v1.1.0 · 106ae4f · 1 retry · 0.0s
pub fn step<T>(
    doing: &str,
    done: &str,
    run: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let started = Instant::now();
    STEPS.fetch_add(1, Ordering::Relaxed);
    DETAIL.with(|d| d.borrow_mut().take());
    DONE.with(|d| d.borrow_mut().take());
    RETRIES.with(|r| r.set(0));
    STEP.with(|s| s.set(true));
    PRINTED.with(|p| p.borrow_mut().clear());
    let result = run();
    STEP.with(|s| s.set(false));
    let mut printed = PRINTED.with(|p| std::mem::take(&mut *p.borrow_mut()));
    while printed.last().is_some_and(|l| l.trim().is_empty()) {
        printed.pop();
    }
    if !printed.is_empty() {
        LOGS.lock()
            .unwrap()
            .push((doing.to_string(), printed, result.is_err()));
    }
    let detail = match &result {
        Ok(_) => DETAIL.with(|d| d.borrow_mut().take()),
        Err(e) => Some(short(e.lines().next().unwrap_or_default())),
    };
    if result.is_ok() && QUIET.load(Ordering::Relaxed) {
        PASSED.fetch_add(1, Ordering::Relaxed);
        return result;
    }
    let retries = RETRIES.with(Cell::get);
    let trail: Vec<String> = detail
        .into_iter()
        .chain((retries > 0).then(|| count(retries, "retry", "retries")))
        .chain([dim(&seconds(started.elapsed()))])
        .collect();
    let line = match &result {
        Ok(_) => plain(
            &DONE
                .with(|d| d.borrow_mut().take())
                .unwrap_or_else(|| done.to_string()),
        ),
        Err(_) => marked(&red("●"), &format!("{doing} failed")),
    };
    begin();
    emit(&format!(
        "{line}{}{}",
        separator(),
        trail.join(&separator())
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_below_the_temp_directory() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("relcut/origin-demo-1.1.0.tgz");
        assert_eq!(
            short(&format!("Packed {}", path.display())),
            "Packed relcut/origin-demo-1.1.0.tgz"
        );
        assert_eq!(seconds(Duration::from_millis(1234)), "1.2s");
        assert_eq!(seconds(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn widths_leave_colors_out() {
        assert_eq!(width("\x1b[32m✔\x1b[0m  done"), 7);
    }
}
