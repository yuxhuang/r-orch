//! Measurement observations, never process ownership or mutation authority.
use super::steps::Runner;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub(super) struct Sampler {
    next: Instant,
    pub(super) foreign: &'static str,
    pub(super) max_load: Option<f64>,
    pub(super) max_foreign: usize,
    pub(super) thermal: String,
    pub(super) samples: usize,
    pub(super) log: String,
    unavailable: bool,
}
impl Sampler {
    pub(super) fn new() -> Self {
        Self {
            next: Instant::now(),
            foreign: "unavailable",
            max_load: None,
            max_foreign: 0,
            thermal: "unavailable: not queried".into(),
            samples: 0,
            log: String::new(),
            unavailable: false,
        }
    }
    pub(super) fn due(&self) -> bool {
        Instant::now() >= self.next
    }
    pub(super) fn record(&mut self, result: Result<Observation>) {
        self.next = Instant::now() + Duration::from_secs(15);
        self.samples += 1;
        match result {
            Ok(o) => {
                self.max_foreign = self.max_foreign.max(o.foreign);
                if o.foreign > 0 {
                    self.foreign = "yes";
                } else if self.foreign != "yes" && !self.unavailable {
                    self.foreign = "no";
                }
                self.max_load = Some(self.max_load.map_or(o.load, |n| n.max(o.load)));
                self.thermal = o.thermal;
                self.log.push_str(&format!(
                    "sample={} foreign={} load={:.3} thermal={}\n",
                    self.samples, o.foreign, o.load, self.thermal
                ));
            }
            Err(e) => {
                self.unavailable = true;
                if self.foreign != "yes" {
                    self.foreign = "unavailable";
                }
                self.log.push_str(&format!(
                    "sample={} unavailable={}\n",
                    self.samples,
                    super::report::diagnostic(format!("{e:#}").as_bytes())
                ));
            }
        }
    }
    pub(super) fn load(&self) -> String {
        if self.unavailable {
            "unavailable".into()
        } else {
            self.max_load
                .map_or("unavailable".into(), |n| format!("{n:.3}"))
        }
    }
}
pub(super) struct Observation {
    foreign: usize,
    load: f64,
    thermal: String,
}
fn foreign(bytes: &[u8], root: u32) -> Result<usize> {
    let rows = std::str::from_utf8(bytes)?
        .lines()
        .map(|line| {
            let f: Vec<_> = line.split_whitespace().collect();
            ensure!(f.len() >= 3, "ps record");
            Ok((f[0].parse::<u32>()?, f[1].parse::<u32>()?, f[2].to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut ours = BTreeSet::from([root]);
    loop {
        let old = ours.len();
        for (pid, parent, _) in &rows {
            if ours.contains(parent) {
                ours.insert(*pid);
            }
        }
        if old == ours.len() {
            break;
        }
    }
    Ok(rows
        .iter()
        .filter(|(pid, _, cmd)| {
            !ours.contains(pid)
                && [
                    "cargo",
                    "rustc",
                    "rustdoc",
                    "clippy-driver",
                    "cargo-nextest",
                ]
                .contains(
                    &Path::new(cmd)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                )
        })
        .count())
}
fn load(text: &str) -> Result<f64> {
    let n = text
        .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .next()
        .context("load empty")?
        .parse::<f64>()?;
    ensure!(n.is_finite() && n >= 0., "load invalid");
    Ok(n)
}
pub(super) fn query(runner: &mut Runner<'_, '_>) -> Result<Observation> {
    let ps = runner.capture_raw(
        Command::new("ps").args(["-axo", "pid=,ppid=,comm="]),
        1024 * 1024,
    )?;
    ensure!(ps.exit.success(), "ps query failed");
    let foreign = foreign(&ps.stdout, std::process::id())?;
    #[cfg(target_os = "macos")]
    let text = {
        let out = runner.capture_raw(Command::new("sysctl").args(["-n", "vm.loadavg"]), 4096)?;
        ensure!(out.exit.success(), "load query failed");
        String::from_utf8(out.stdout)?
    };
    #[cfg(target_os = "linux")]
    let text = std::fs::read_to_string("/proc/loadavg")?;
    let load = load(&text)?;
    #[cfg(target_os = "macos")]
    let thermal = if Path::new("/usr/bin/notifyutil").is_file() {
        match runner.capture_raw(
            Command::new("/usr/bin/notifyutil")
                .args(["-g", "com.apple.system.thermalpressurelevel"]),
            4096,
        ) {
            Ok(out) if out.exit.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            _ => "unavailable: thermal query failed".into(),
        }
    } else {
        "unavailable: notifyutil missing".into()
    };
    #[cfg(target_os = "linux")]
    let thermal = "unsupported: no portable Linux thermal pressure query".into();
    Ok(Observation {
        foreign,
        load,
        thermal,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descendants_are_excluded_regardless_of_row_order_and_unknown_rows_refuse() {
        assert_eq!(
            foreign(
                b"30 20 rustc\n100 1 /bin/cargo\n20 10 cargo\n10 1 gate\n40 1 /bin/rustdoc\n",
                10
            )
            .unwrap(),
            2
        );
        assert!(foreign(b"broken\n", 10).is_err());
        assert!(foreign(b"10 nope cargo\n", 10).is_err());
    }
    #[test]
    fn unavailable_samples_are_sticky_and_never_erase_an_observed_foreign_builder() {
        let mut s = Sampler::new();
        assert!(s.due());
        s.record(Ok(Observation {
            foreign: 0,
            load: 1.,
            thermal: "unavailable".into(),
        }));
        assert!(!s.due());
        assert_eq!(s.foreign, "no");
        s.record(Err(anyhow::anyhow!("load failed")));
        assert_eq!(s.foreign, "unavailable");
        s.record(Ok(Observation {
            foreign: 0,
            load: 2.,
            thermal: "unavailable".into(),
        }));
        assert_eq!(s.foreign, "unavailable");
        s.record(Ok(Observation {
            foreign: 1,
            load: 1.,
            thermal: "unavailable".into(),
        }));
        assert_eq!(s.foreign, "yes");
        assert_eq!(s.load(), "unavailable");
        for text in ["", "NaN", "-1", "infinity"] {
            assert!(load(text).is_err());
        }
        assert_eq!(load("{ 2.00 1.00 0.50 }").unwrap(), 2.);
    }
}
