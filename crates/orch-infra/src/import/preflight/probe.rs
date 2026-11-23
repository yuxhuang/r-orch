//! R119: has the work of a recorded 1.0 run possibly not ended? The importer asks two
//! questions with the existence test that delivers nothing, and no other.
use nix::errno::Errno;
use nix::sys::signal::{kill, killpg};
use nix::unistd::Pid;

/// The two questions, behind a seam so a test can prove which calls are made.
pub trait ProcessProbe: Send + Sync {
    /// Does a process with this pid exist (signal number 0)?
    fn process_exists(&self, pid: i32) -> bool;
    /// Does the process group with this id still have a member (signal number 0)?
    fn group_has_member(&self, pgid: i32) -> bool;
}

/// The operating system's answer. `EPERM` means the target exists. An error that is
/// not `ESRCH` is read as "exists": the check refuses on doubt and never on silence.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

fn exists(result: nix::Result<()>) -> bool {
    !matches!(result, Err(Errno::ESRCH))
}

impl ProcessProbe for SystemProbe {
    fn process_exists(&self, pid: i32) -> bool {
        exists(kill(Pid::from_raw(pid), None))
    }
    fn group_has_member(&self, pgid: i32) -> bool {
        exists(killpg(Pid::from_raw(pgid), None))
    }
}

/// What the two questions found for one recorded pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) process: bool,
    pub(super) group: bool,
}

impl Found {
    pub(super) fn any(self) -> bool {
        self.process || self.group
    }
    pub(super) fn describe(self) -> &'static str {
        match (self.process, self.group) {
            (true, true) => "the process and its process group were found",
            (true, false) => "the process was found",
            (false, true) => "only the process group was found",
            (false, false) => "neither the process nor its group was found",
        }
    }
}

/// A recorded pid of 0 or above 2,147,483,647 names nothing and reaches no system
/// call: 0 would address the caller's own group and a wrapped value would address
/// every process. Pid 1 is asked about as a process and never as a group, where
/// the number would mean every process the caller may signal.
pub(super) fn ask(probe: &dyn ProcessProbe, pid: u32) -> Found {
    let Ok(pid) = i32::try_from(pid) else {
        return Found {
            process: false,
            group: false,
        };
    };
    if pid == 0 {
        return Found {
            process: false,
            group: false,
        };
    }
    Found {
        process: probe.process_exists(pid),
        group: pid != 1 && probe.group_has_member(pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Table(Mutex<Vec<(&'static str, i32)>>);
    impl ProcessProbe for Table {
        fn process_exists(&self, pid: i32) -> bool {
            self.0.lock().unwrap().push(("process", pid));
            true
        }
        fn group_has_member(&self, pgid: i32) -> bool {
            self.0.lock().unwrap().push(("group", pgid));
            true
        }
    }

    #[test]
    fn a_pid_of_0_or_1_or_over_the_range_is_never_passed_to_a_group_test() {
        let table = Table::default();
        for pid in [0, u32::MAX, 2_147_483_648] {
            assert!(!ask(&table, pid).any(), "{pid}");
        }
        assert!(table.0.lock().unwrap().is_empty());
        assert_eq!(ask(&table, 1).describe(), "the process was found");
        assert!(ask(&table, 2_147_483_647).process);
        assert_eq!(
            *table.0.lock().unwrap(),
            [("process", 1), ("process", i32::MAX), ("group", i32::MAX)]
        );
    }

    #[test]
    fn the_system_probe_distinguishes_a_missing_target_from_a_present_one() {
        let me = i32::try_from(std::process::id()).unwrap();
        assert!(SystemProbe.process_exists(me));
        // The pid space ends at 2^22 on Linux, so this pid names nothing.
        assert!(!SystemProbe.process_exists(i32::MAX));
        assert!(!SystemProbe.group_has_member(i32::MAX));
    }
}
