//! Stop a git process together with grandchildren that have left its group.
//!
//! `exec::command` starts git with `process_group(0)`. A grandchild that calls
//! `setsid` leaves that group, so a later `killpg` cannot see it. The descendant
//! list has to be taken while the parent is still alive: after it exits, the
//! child is reparented to init and walking `PPid` no longer reaches the git
//! process.

#[cfg(unix)]
use std::collections::HashMap;

/// How far to walk `PPid` before giving up. A cycle must not spin.
#[cfg(unix)]
const MAX_PPID_STEPS: usize = 64;

/// Kill `pid`'s descendants, then its process group.
///
/// Descendants are signaled first, and twice: a child can fork in between the
/// two snapshots. The parent is still alive for both, so a `setsid` grandchild
/// is not reparented to init before its pid is recorded.
pub fn reap_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        reap_unix(pid);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[cfg(unix)]
fn reap_unix(pid: u32) {
    // pid 0, init, this process, and this process's group are never signaled.
    let Some(leader) = group_leader(pid) else {
        return;
    };

    kill_descendants(pid);
    // A fork that lands after the first snapshot is still a child of `pid`.
    // Record it before the parent is killed, or that child is reparented to init.
    kill_descendants(pid);

    // `group_leader` already rejected our group. Check again at the signal:
    // this call must not kill the caller's group.
    if leader == rustix::process::getpgrp() {
        return;
    }
    let _ = rustix::process::kill_process_group(leader, rustix::process::Signal::KILL);
}

#[cfg(unix)]
fn group_leader(pid: u32) -> Option<rustix::process::Pid> {
    if pid == 0 || pid == 1 {
        return None;
    }
    let raw = i32::try_from(pid).ok()?;
    let parsed = rustix::process::Pid::from_raw(raw)?;
    if parsed == rustix::process::getpid() || parsed == rustix::process::getpgrp() {
        return None;
    }
    Some(parsed)
}

#[cfg(unix)]
fn kill_descendants(root: u32) {
    let self_pid = rustix::process::getpid();
    for pid in descendant_pids(root) {
        if pid <= 1 {
            continue;
        }
        let Ok(raw) = i32::try_from(pid) else {
            continue;
        };
        let Some(parsed) = rustix::process::Pid::from_raw(raw) else {
            continue;
        };
        if parsed == self_pid {
            continue;
        }
        let _ = rustix::process::kill_process(parsed, rustix::process::Signal::KILL);
    }
}

#[cfg(unix)]
fn descendant_pids(root: u32) -> Vec<u32> {
    let ppids = snapshot_ppids();
    let pids: Vec<u32> = ppids.keys().copied().collect();
    pids.into_iter()
        .filter(|&pid| is_descendant(pid, root, &ppids))
        .collect()
}

/// `start` is a descendant of `root` when following `PPid` reaches `root` in
/// at most [`MAX_PPID_STEPS`] hops. A repeated pid is a cycle and stops the walk.
#[cfg(unix)]
fn is_descendant(start: u32, root: u32, ppids: &HashMap<u32, u32>) -> bool {
    if start == root {
        return false;
    }
    let mut current = start;
    let mut seen = Vec::with_capacity(MAX_PPID_STEPS);
    for _ in 0..MAX_PPID_STEPS {
        if seen.contains(&current) {
            return false;
        }
        seen.push(current);
        let Some(&parent) = ppids.get(&current) else {
            return false;
        };
        if parent == root {
            return true;
        }
        current = parent;
    }
    false
}

/// `comm` in `/proc/<pid>/stat` can contain parentheses, so the parent pid is
/// the `PPid:` line of `status` instead.
#[cfg(target_os = "linux")]
fn snapshot_ppids() -> HashMap<u32, u32> {
    let mut ppids = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return ppids;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Some(ppid) = ppid_from_status(pid) else {
            continue;
        };
        ppids.insert(pid, ppid);
    }
    ppids
}

#[cfg(target_os = "linux")]
fn ppid_from_status(pid: u32) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .and_then(|rest| rest.trim().parse().ok())
    })
}

/// `proc_bsdinfo`'s layout is not stable across OS releases, and this crate
/// cannot take a new dependency to decode it. `/bin/ps` is absolute so `PATH`
/// cannot select a different binary.
#[cfg(target_os = "macos")]
fn snapshot_ppids() -> HashMap<u32, u32> {
    let mut ppids = HashMap::new();
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,ppid="])
        .output()
    else {
        return ppids;
    };
    if !output.status.success() {
        return ppids;
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return ppids;
    };
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        ppids.insert(pid, ppid);
    }
    ppids
}

/// No portable snapshot. [`reap_unix`] still signals the process group.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn snapshot_ppids() -> HashMap<u32, u32> {
    HashMap::new()
}
