// CPU / memory sampling. A "process" here is the whole tree under the pid we
// spawned, since the interesting bit is usually a grandchild (sh -> npm -> node).

use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub const EVERY: Duration = Duration::from_secs(1);

pub struct Sampler {
    sys: Option<System>,
    last: Option<Instant>,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Usage {
    pub cpu: f32,
    pub mem: u64,
    pub procs: usize,
}

impl Sampler {
    pub fn new() -> Self {
        Self { sys: System::new().ok(), last: None }
    }

    pub fn due(&self) -> bool {
        self.last.is_none_or(|t| t.elapsed() >= EVERY)
    }

    /// Usage for each root pid, summed over all its descendants.
    pub fn sample(&mut self, roots: &[u32]) -> HashMap<u32, Usage> {
        self.last = Some(Instant::now());
        let mut out = HashMap::new();
        let Some(sys) = self.sys.as_mut() else { return out };
        if roots.is_empty() {
            return out;
        }
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_cpu().with_memory());

        let mut kids: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, p) in sys.processes() {
            if let Some(parent) = p.parent() {
                kids.entry(parent).or_default().push(*pid);
            }
        }
        for &root in roots {
            let mut u = Usage::default();
            let mut stack = vec![Pid::from_u32(root)];
            let mut seen = 0;
            while let Some(pid) = stack.pop() {
                seen += 1;
                if seen > 4096 {
                    break; // just in case of a weird pid loop
                }
                if let Some(p) = sys.process(pid) {
                    u.cpu += p.cpu_usage();
                    u.mem += p.memory();
                    u.procs += 1;
                }
                if let Some(k) = kids.get(&pid) {
                    stack.extend(k);
                }
            }
            out.insert(root, u);
        }
        out
    }
}

pub fn fmt_bytes(b: f64) -> String {
    if b >= 1024.0 {
        format!("{:.2} GB", b / 1024.0)
    } else {
        format!("{b:.1} MB")
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        3600..86400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_duration(Duration::from_secs(125)), "2m 5s");
        assert_eq!(fmt_duration(Duration::from_secs(3 * 3600 + 120)), "3h 2m");
    }

    #[test]
    fn samples_self() {
        let mut s = Sampler::new();
        let me = std::process::id();
        let u = s.sample(&[me]);
        assert!(u[&me].mem > 0);
    }
}
