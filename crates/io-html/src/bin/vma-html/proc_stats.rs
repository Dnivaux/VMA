//! CPU time and memory of this process and of its Chromium sub-processes.
//!
//! Linux reads `/proc`: CPU time from `stat`, memory as PSS (`smaps_rollup`), which shares the
//! pages common to several processes instead of counting them once per process. Not measured
//! elsewhere for this trial (`None`).

use std::time::Duration;

#[derive(Debug, Clone, Copy, Default)]
pub struct TreeSample {
    pub processes: usize,
    pub cpu: Duration,
    pub pss_kib: u64,
}

#[cfg(target_os = "linux")]
pub fn sample() -> Option<TreeSample> {
    use std::collections::HashMap;
    use std::fs;

    // Clock ticks per second of `stat`: 100 on every Linux target we build for.
    const CLK_TCK: u64 = 100;

    let root = std::process::id();
    let mut parent_of = HashMap::new();
    let mut cpu_ticks = HashMap::new();
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid … utime stime`: comm may hold spaces, split after its `)`.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let field = |i: usize| fields.get(i).and_then(|f| f.parse::<u64>().ok());
        if let (Some(ppid), Some(utime), Some(stime)) = (field(1), field(11), field(12)) {
            parent_of.insert(pid, u32::try_from(ppid).unwrap_or(0));
            cpu_ticks.insert(pid, utime + stime);
        }
    }

    let in_tree = |mut pid: u32| {
        while pid != 0 {
            if pid == root {
                return true;
            }
            pid = parent_of.get(&pid).copied().unwrap_or(0);
        }
        false
    };

    let mut out = TreeSample::default();
    for (&pid, &ticks) in &cpu_ticks {
        if !in_tree(pid) {
            continue;
        }
        out.processes += 1;
        out.cpu += Duration::from_millis(ticks * 1000 / CLK_TCK);
        out.pss_kib += fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("Pss:"))
                    .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            })
            .unwrap_or(0);
    }
    Some(out)
}

#[cfg(not(target_os = "linux"))]
pub fn sample() -> Option<TreeSample> {
    None
}
