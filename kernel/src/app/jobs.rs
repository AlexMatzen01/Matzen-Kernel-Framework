//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Background jobs: cooperative MFKE/script execution with PIDs.
//!
//! There is no preemption (single ring-0 kernel stack, no scheduler), so
//! jobs are resumable interpreter states ([`MfkeVm`] / [`ScriptJob`]) that
//! the shell loop time-slices via [`poll`]: one small quantum per runnable
//! job per pass. `SLEEP` never blocks here; it parks the job until its
//! wake time. `kill` sets a flag that [`poll`] (or `kill` itself) turns
//! into an immediate reap at the next quantum boundary.
//!
//! All state is `no_std` + `alloc` compatible (`spin::Mutex` + atomics).
//! The global lock is never held while job code runs: [`poll`] removes a
//! job from the table, steps it unlocked, then reinserts or reaps it, so
//! nested `jobs` / `kill` / `run --bg` from inside a script line cannot
//! deadlock.

use super::interpreter::{ScriptJob, ScriptStep};
use super::loader::{MfkeVm, StepOut, VmMode};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// Maximum live background jobs (bounds heap usage).
pub const MAX_JOBS: usize = 8;
/// First background PID (0 idle, 1 shell, 2 net, 3 fs are fixed `ps` rows).
pub const FIRST_PID: u64 = 4;
/// Job-name storage cap so overlong paths cannot bloat the table.
const NAME_MAX: usize = 64;
/// Max display width for job names in the `ps` table.
const PS_NAME_WIDTH: usize = 14;

/// Commands a background script may not invoke.
///
/// Blocking commands would stall the owner's quantum, interactive ones
/// steal the screen/keyboard, destructive ones format disks, and the rest
/// mutate global shell state (cwd, drives, colors) out from under the
/// foreground shell.
const BG_DENY: &[&str] = &[
    "ping",
    "top",
    "nano",
    "edit",
    "mfkedit",
    "wget",
    "speedtest",
    "speedtest-server",
    "tcpconnect",
    "tcpsend",
    "tcpclose",
    "run",
    "exec",
    "java",
    "javac",
    "jar",
    "jversions",
    "jsdk",
    "halt",
    "reboot",
    "shutdown",
    "clear",
    "cls",
    "color",
    "cd",
    "mount",
    "mkfs",
    "install",
];

/// Runnable or parked until `wake_at_ms`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Sleeping,
}

/// What a job executes.
pub enum JobKind {
    Mfke(MfkeVm),
    Script(ScriptJob),
}

/// One background job.
pub struct Job {
    pid: u64,
    name: String,
    kind: JobKind,
    state: JobState,
    wake_at_ms: u64,
    started_ms: u64,
    kill_requested: bool,
}

static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());
static NEXT_PID: AtomicU64 = AtomicU64::new(FIRST_PID);

/// `true` once `now_ms` has reached `until_ms` (wrapping-safe for any
/// sleep shorter than 2^63 ms, i.e. all of them).
fn wake_due(now_ms: u64, until_ms: u64) -> bool {
    now_ms.wrapping_sub(until_ms) < (1u64 << 63)
}

/// Store at most [`NAME_MAX`] bytes, keeping the tail (char-boundary safe).
fn truncate_name(path: &str) -> String {
    let mut s = String::from(path);
    while s.len() > NAME_MAX {
        s.remove(0);
    }
    s
}

/// Last up to [`PS_NAME_WIDTH`] chars, char-boundary safe, for tables.
pub fn display_name(name: &str) -> &str {
    if name.len() <= PS_NAME_WIDTH {
        return name;
    }
    let mut start = name.len() - PS_NAME_WIDTH;
    while start < name.len() && !name.is_char_boundary(start) {
        start += 1;
    }
    &name[start..]
}

/// First whitespace-separated token of a command line (`""` when blank).
fn first_token(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// If `line` (already expanded) may not run in the background, returns the
/// offending command for the error message.
pub fn bg_blocked(line: &str) -> Option<&str> {
    let cmd = first_token(line);
    if cmd.is_empty() || cmd.starts_with('#') || cmd == "exit" {
        return None;
    }
    if BG_DENY.contains(&cmd) {
        return Some(cmd);
    }
    // `meminfo` is fine except the blocking live view.
    if cmd == "meminfo"
        && line
            .split_whitespace()
            .skip(1)
            .any(|t| t == "--watch" || t == "-w")
    {
        return Some("meminfo --watch");
    }
    None
}

/// Spawn `path` as a background job. MFKE bytecode and text scripts only;
/// Java/ELF/unknown formats stay foreground-only.
///
/// Prints `[job <pid>] started ...` and returns the PID.
pub fn spawn_background(path: &str, args: &[&str]) -> Result<u64, &'static str> {
    if !crate::shell::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    if JOBS.lock().len() >= MAX_JOBS {
        return Err("Too many background jobs (max 8; use 'jobs' + 'kill')");
    }
    let data = crate::shell::read_file_contents(path)
        .ok_or("Failed to read file (not found or not mounted)")?;
    if data.is_empty() {
        return Err("Empty file (nothing to run in the background)");
    }
    // PIDs are monotonic and never reused; a failed spawn below may skip
    // one, which is harmless.
    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let started_ms = crate::shell::get_tick_count();

    // Type detection mirrors `app::run`; Java/ELF stay foreground-only.
    if crate::java::class::is_class_file(&data) {
        return Err("Java apps can't run in the background (foreground only)");
    }
    if data.len() >= 4 && data[0..4] == [0x7F, b'E', b'L', b'F'] {
        return Err("ELF not yet supported (see mkapp)");
    }
    if data.len() >= 4 {
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic == super::loader::MFKE_MAGIC {
            let vm = MfkeVm::new(&data, VmMode::Background { pid })?;
            JOBS.lock().push(Job {
                pid,
                name: truncate_name(path),
                kind: JobKind::Mfke(vm),
                state: JobState::Running,
                wake_at_ms: 0,
                started_ms,
                kill_requested: false,
            });
            crate::println!("[job {}] started '{}' (mfke, background)", pid, path);
            return Ok(pid);
        }
    }
    if !crate::app::is_probably_text(&data) {
        return Err("Unknown binary format (only MFKE + scripts run in background)");
    }
    let script = ScriptJob::new(&data, path, args)?;
    // Spawn-time denylist scan over the final (expanded) command text, so a
    // blocking/interactive line can never stall the shell from background.
    for line in script.lines() {
        if let Some(bad) = bg_blocked(line) {
            crate::println!(
                "[job] '{}' uses '{}': not allowed in the background",
                path,
                bad
            );
            return Err("Script uses a blocking/interactive command (see 'run --help')");
        }
    }
    let total = script.line_total();
    JOBS.lock().push(Job {
        pid,
        name: truncate_name(path),
        kind: JobKind::Script(script),
        state: JobState::Running,
        wake_at_ms: 0,
        started_ms,
        kill_requested: false,
    });
    crate::println!(
        "[job {}] started '{}' (script, {} lines, background)",
        pid,
        path,
        total
    );
    Ok(pid)
}

/// Request termination of `pid`. The job is reaped at its next quantum
/// boundary (or immediately if the caller pumps [`poll`]); `false` means
/// no such job exists.
pub fn kill(pid: u64) -> bool {
    let mut jobs = JOBS.lock();
    match jobs.iter_mut().find(|j| j.pid == pid) {
        Some(job) => {
            job.kill_requested = true;
            true
        }
        None => false,
    }
}

/// Give every runnable job one quantum. Called from the shell idle loop,
/// foreground sleeps, and live views (`top`, `meminfo --watch`).
///
/// Each job is removed from the table before stepping and reinserted (or
/// reaped) afterwards, so nested `jobs` / `kill` / `run --bg` from inside
/// a script line cannot deadlock on the table lock.
pub fn poll() {
    let mut idx = 0;
    loop {
        let mut job = {
            let mut jobs = JOBS.lock();
            if idx >= jobs.len() {
                break;
            }
            jobs.remove(idx)
        };
        let now_ms = crate::shell::get_tick_count();
        if job.kill_requested {
            reap_killed(job, now_ms);
            continue;
        }
        if job.state == JobState::Sleeping && !wake_due(now_ms, job.wake_at_ms) {
            JOBS.lock().insert(idx, job);
            idx += 1;
            continue;
        }
        job.state = JobState::Running;
        let fate: Option<Result<i32, &'static str>> = match &mut job.kind {
            JobKind::Mfke(vm) => match vm.step(super::loader::BG_FUEL) {
                StepOut::More => None,
                StepOut::Sleep(ms) => {
                    job.state = JobState::Sleeping;
                    job.wake_at_ms = now_ms.wrapping_add(ms);
                    None
                }
                StepOut::Done(code) => Some(Ok(code)),
                StepOut::Failed(e) => Some(Err(e)),
            },
            JobKind::Script(script) => {
                match script.step(super::interpreter::BG_LINES, Some(job.pid)) {
                    ScriptStep::More => None,
                    ScriptStep::Done(code) => Some(Ok(code)),
                    ScriptStep::Failed(e) => Some(Err(e)),
                }
            }
        };
        match fate {
            None => {
                JOBS.lock().insert(idx, job);
                idx += 1;
            }
            Some(Ok(code)) => reap_exit(job, now_ms, code),
            Some(Err(e)) => reap_fail(job, now_ms, e),
        }
    }
}

fn reap_exit(job: Job, now_ms: u64, code: i32) {
    let elapsed = now_ms.wrapping_sub(job.started_ms);
    crate::sysinfo::record_app_exit(&job.name, code, elapsed);
    crate::println!(
        "[job {}] '{}' exited code {} in {} ms",
        job.pid,
        job.name,
        code,
        elapsed
    );
}

fn reap_fail(job: Job, now_ms: u64, err: &'static str) {
    let elapsed = now_ms.wrapping_sub(job.started_ms);
    crate::sysinfo::record_app_exit(&job.name, -1, elapsed);
    crate::println!(
        "[job {}] '{}' failed: {} ({} ms)",
        job.pid,
        job.name,
        err,
        elapsed
    );
}

fn reap_killed(job: Job, now_ms: u64) {
    let elapsed = now_ms.wrapping_sub(job.started_ms);
    crate::sysinfo::record_app_exit(&job.name, -1, elapsed);
    crate::println!(
        "[job {}] '{}' killed after {} ms",
        job.pid,
        job.name,
        elapsed
    );
}

/// Printable snapshot of live jobs (cloned out from under the lock).
pub struct JobInfo {
    pub pid: u64,
    pub name: String,
    /// `"running"` / `"sleeping"` (lowercase, matches the `ps` table).
    pub state: &'static str,
    pub elapsed_ms: u64,
    pub detail: String,
}

/// Clone the live job table for `jobs` / `ps` rendering.
pub fn snapshot() -> Vec<JobInfo> {
    let now_ms = crate::shell::get_tick_count();
    let jobs = JOBS.lock();
    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs.iter() {
        let state = match job.state {
            JobState::Running => "running",
            JobState::Sleeping => "sleeping",
        };
        let base = match &job.kind {
            JobKind::Mfke(vm) => alloc::format!("mfke pc {}/{}", vm.pc(), vm.code_len()),
            JobKind::Script(s) => {
                alloc::format!("script line {}/{}", job_line_shown(s), s.line_total())
            }
        };
        let detail = if job.state == JobState::Sleeping {
            if wake_due(now_ms, job.wake_at_ms) {
                alloc::format!("{} (wake now)", base)
            } else {
                alloc::format!(
                    "{} (wake in {} ms)",
                    base,
                    job.wake_at_ms.wrapping_sub(now_ms)
                )
            }
        } else {
            base
        };
        out.push(JobInfo {
            pid: job.pid,
            name: job.name.clone(),
            state,
            elapsed_ms: now_ms.wrapping_sub(job.started_ms),
            detail,
        });
    }
    out
}

/// 1-based "current line" display for a script job (clamped to the total).
fn job_line_shown(s: &ScriptJob) -> usize {
    core::cmp::min(s.line_next() + 1, core::cmp::max(s.line_total(), 1))
}
