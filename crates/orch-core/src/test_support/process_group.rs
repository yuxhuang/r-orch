//! Reading a process group out of `/proc`, for the group-kill tests.
//!
//! Two tests assert that a group kill left nothing behind, and both used to do
//! it with `kill(-pgid, 0)`. That call **succeeds for a zombie as well as a
//! live process**, so both were really waiting on init to reap the orphaned
//! grandchildren — something neither test controls — and both flaked at about
//! 1 in 10 full-workspace runs, where process pressure is highest. Worse,
//! neither could say *which* had happened: a surviving child and an unreaped
//! zombie produced the same bare message.
//!
//! Distinguishing them is the whole point. A zombie means the kill worked and
//! the parent has not been reaped yet, which is not ORCH's business; a **live**
//! member means the kill path is broken, which is a product bug and must still
//! fail the test.
//!
//! Linux-only, like the group-kill behaviour it inspects.

use std::time::Duration;

/// A process in a process group, as `/proc/<pid>/stat` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub pid: u32,
    /// `R`, `S`, `D`, `Z`, `T`, … — field 3 of `/proc/<pid>/stat`.
    pub state: char,
    pub ppid: i32,
    pub pgid: i32,
}

impl GroupMember {
    /// Exited but not yet reaped. Holds the pid, so the *group* still answers
    /// `kill(-pgid, 0)`, but it is not running and cannot be killed again.
    pub fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }
}

/// Every process currently in `pgid`.
///
/// Scans `/proc`, so a process that exits mid-scan is simply absent — which is
/// the right answer. Anything unreadable is skipped rather than failing the
/// caller: a pid belonging to another user is not this group's business.
pub fn process_group_members(pgid: u32) -> Vec<GroupMember> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut members = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some(member) = parse_stat(pid, &stat).filter(|m| m.pgid == pgid as i32) {
            members.push(member);
        }
    }
    members.sort_by_key(|member| member.pid);
    members
}

/// The members of `pgid` that are not zombies.
pub fn live_group_members(pgid: u32) -> Vec<GroupMember> {
    process_group_members(pgid)
        .into_iter()
        .filter(|member| !member.is_zombie())
        .collect()
}

/// Poll until `pgid` has no live members, up to `budget`.
///
/// `Err` carries whatever was still running, so the caller can say what
/// survived instead of only that something did.
pub async fn wait_for_no_live_members(
    pgid: u32,
    budget: Duration,
) -> std::result::Result<(), Vec<GroupMember>> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let live = live_group_members(pgid);
        if live.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(live);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// `pid state ppid pgid` per member, for a failure message.
pub fn describe(members: &[GroupMember]) -> String {
    if members.is_empty() {
        return "<none>".to_owned();
    }
    members
        .iter()
        .map(|m| {
            format!(
                "pid {} state {} ppid {} pgid {}",
                m.pid, m.state, m.ppid, m.pgid
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Parse the fields of `/proc/<pid>/stat` that identify a group member.
///
/// The second field is the executable name in parentheses and may itself
/// contain spaces and parentheses — `(sh)` but also `((weird) name)` — so the
/// fields after it are found from the **last** `')'`, not by splitting the
/// whole line.
fn parse_stat(pid: u32, stat: &str) -> Option<GroupMember> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_comm.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    let pgid = fields.next()?.parse().ok()?;
    Some(GroupMember {
        pid,
        state,
        ppid,
        pgid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fields_after_the_executable_name() {
        let member = parse_stat(42, "42 (sh) S 7 99 99 0 -1 4194304 0 0").expect("parsed");
        assert_eq!(
            member,
            GroupMember {
                pid: 42,
                state: 'S',
                ppid: 7,
                pgid: 99
            }
        );
    }

    #[test]
    fn an_executable_name_containing_spaces_and_parens_does_not_shift_the_fields() {
        // The reason this parses from the last ')' rather than splitting: a
        // comm of `(my (odd) prog)` would otherwise move every field.
        let member = parse_stat(42, "42 (my (odd) prog) Z 1 500 500 0").expect("parsed");
        assert_eq!(member.state, 'Z');
        assert_eq!(member.ppid, 1);
        assert_eq!(member.pgid, 500);
        assert!(member.is_zombie());
    }

    #[test]
    fn a_malformed_line_is_skipped_rather_than_panicking() {
        assert!(parse_stat(1, "").is_none());
        assert!(parse_stat(1, "1 (sh)").is_none());
        assert!(parse_stat(1, "1 (sh) S").is_none());
        assert!(parse_stat(1, "no parens here").is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn this_process_is_in_its_own_group() {
        // A live check that the /proc scan and the parse agree with reality.
        let pgid = own_pgid();
        let members = process_group_members(pgid);
        assert!(
            members.iter().any(|m| m.pid == std::process::id()),
            "this process is missing from its own group: {}",
            describe(&members)
        );
        // And it is not a zombie, so it shows up as live too.
        assert!(
            live_group_members(pgid)
                .iter()
                .any(|m| m.pid == std::process::id())
        );
    }

    /// This process's group, read from its own stat line — no libc needed in
    /// orch-core for one value.
    #[cfg(target_os = "linux")]
    fn own_pgid() -> u32 {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("read /proc/self/stat");
        parse_stat(std::process::id(), &stat).expect("parse").pgid as u32
    }

    #[test]
    fn describe_names_every_survivor() {
        let text = describe(&[
            GroupMember {
                pid: 7,
                state: 'S',
                ppid: 1,
                pgid: 7,
            },
            GroupMember {
                pid: 8,
                state: 'Z',
                ppid: 7,
                pgid: 7,
            },
        ]);
        assert!(text.contains("pid 7 state S ppid 1 pgid 7"), "{text}");
        assert!(text.contains("pid 8 state Z ppid 7 pgid 7"), "{text}");
        assert_eq!(describe(&[]), "<none>");
    }
}
