//! Stop a git process together with grandchildren that have left its group.
//!
//! `exec::command` starts git with `process_group(0)`. A grandchild that calls
//! `setsid` leaves that group, so a later `killpg` cannot see it. The descendant
//! list has to be taken while the parent is still alive: after it exits, the
//! child is reparented to init and walking `PPid` no longer reaches the git
//! process.

use std::collections::HashMap;

use rustix::process::{Pid, Signal};

/// How far to walk `PPid` before giving up. A cycle must not spin.
const MAX_PPID_STEPS: usize = 64;

/// Kill `pid`'s descendants, then its process group, while `pid` has not been
/// reaped yet, so the number still names that process.
///
/// Descendants are signaled first, and twice: a child can fork in between the
/// two snapshots. The parent is still alive for both, so a `setsid` grandchild
/// is not reparented to init before its pid is recorded.
pub fn reap_process_tree(pid: u32) {
    let Some(leader) = signalable(pid) else {
        return;
    };
    kill_descendants(pid);
    // A fork that lands after the first snapshot is still a child of `pid`.
    // Record it before the parent is killed, or that child is reparented to init.
    kill_descendants(pid);
    kill_group_of(leader);
}

/// Kill the process group that `pid` led, after `pid` has exited and been
/// reaped. Walking `PPid` would no longer find its descendants, and could
/// find those of an unrelated process that got the number, so only the group
/// is signaled.
pub(crate) fn kill_group(pid: u32) {
    if let Some(leader) = signalable(pid) {
        kill_group_of(leader);
    }
}

fn kill_group_of(leader: Pid) {
    // `signalable` already rejected our group. Check again at the signal:
    // this call must not kill the caller's group.
    if leader == rustix::process::getpgrp() {
        return;
    }
    // `getpgid` succeeds when this pid has been recycled and returns the new
    // process's group. That id is no longer the git group, so do not signal it.
    // ESRCH is not that case: the leader may have exited while grandchildren
    // are still in the group. A free pid makes `getpgid` return ESRCH; a
    // recycled one makes it succeed.
    if let Ok(pgid) = rustix::process::getpgid(Some(leader))
        && pgid != leader
    {
        return;
    }
    let _ = rustix::process::kill_process_group(leader, Signal::KILL);
}

/// `pid` when this process may signal it: never 0, init, this process, or
/// this process's group.
fn signalable(pid: u32) -> Option<Pid> {
    if pid <= 1 {
        return None;
    }
    let parsed = Pid::from_raw(i32::try_from(pid).ok()?)?;
    (parsed != rustix::process::getpid() && parsed != rustix::process::getpgrp()).then_some(parsed)
}

fn kill_descendants(root: u32) {
    let ppids = snapshot_ppids();
    for &pid in ppids.keys() {
        if is_descendant(pid, root, &ppids)
            && let Some(pid) = signalable(pid)
        {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }
    }
}

/// `start` is a descendant of `root` when following `PPid` reaches `root` in
/// at most [`MAX_PPID_STEPS`] hops, which also ends a cycle.
fn is_descendant(start: u32, root: u32, ppids: &HashMap<u32, u32>) -> bool {
    if start == root {
        return false;
    }
    let mut current = start;
    for _ in 0..MAX_PPID_STEPS {
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
        let Some(parent) = ppid_from_status(pid) else {
            continue;
        };
        ppids.insert(pid, parent);
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
        let Some(parent) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        ppids.insert(pid, parent);
    }
    ppids
}

/// No portable snapshot. [`reap_process_tree`] still signals the process group.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn snapshot_ppids() -> HashMap<u32, u32> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn killing_the_group_after_the_leader_has_exited_returns() {
        let mut child = std::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("true");
        let pid = child.id();
        child.wait().expect("reap true");

        let raw = i32::try_from(pid).expect("pid fits in i32");
        let parsed = rustix::process::Pid::from_raw(raw).expect("nonzero pid");
        assert_eq!(
            rustix::process::getpgid(Some(parsed)).expect_err("pid is gone"),
            rustix::io::Errno::SRCH
        );
        // ESRCH must not skip the group signal: an exited leader can still
        // have grandchildren. `kill_process_group` is a no-op when the group
        // is already empty, which is this `true` process.
        super::kill_group(pid);
    }
}
