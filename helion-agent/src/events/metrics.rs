//! Sustained pressure: CPU, memory and swap on the host, a container at its
//! memory limit, and a disk that will fill soon at the rate it's filling.
//!
//! Each level is judged on the average over a window rather than one sample,
//! so a burst never pages and a hot hour always does, and clears on a lower
//! average over a shorter window, so it doesn't flap at the line. Pure: the
//! task feeds it samples and the time. Windows live in memory, so after a
//! restart nothing opens or clears until a window has filled again.

use super::{ human_bytes, EventsHandle, ProblemSpec, Report, Resolution };
use crate::sample::{ docker::ContainerSnapshot, host::{ unix_now, HostSnapshot, HostVitals } };
use helion_common::types::{ ContainerInfo, DiskInfo, EventCategory, Problem, Severity };
use std::{ collections::{ HashMap, VecDeque }, sync::Arc, time::Duration };
use tokio::sync::watch;

pub const EVERY: Duration = Duration::from_secs(15);

/// How the averages are judged; the three thresholds come from config.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Host CPU %. 0 turns the check off.
    pub cpu_pct: u8,
    /// Host memory %. 0 turns the check off.
    pub memory_pct: u8,
    /// Days until a disk is full. 0 turns the forecast off.
    pub disk_full_days: u32,
}

/// One level of one metric.
struct Level {
    kind: &'static str,
    severity: Severity,
    open_at: f64,
    open_over: i64,
    clear_below: f64,
    clear_over: i64,
}

const MIN: i64 = 60;

fn levels(limits: Limits) -> Vec<(Metric, Level)> {
    let mut out = Vec::new();
    if limits.cpu_pct > 0 {
        let at = f64::from(limits.cpu_pct);
        out.push((Metric::Cpu, Level {
            kind: "cpu",
            severity: Severity::Warning,
            open_at: at,
            open_over: 10 * MIN,
            clear_below: at - 10.0,
            clear_over: 2 * MIN,
        }));
    }
    if limits.memory_pct > 0 {
        let at = f64::from(limits.memory_pct);
        out.push((Metric::Memory, Level {
            kind: "memory",
            severity: Severity::Warning,
            open_at: at,
            open_over: 5 * MIN,
            clear_below: at - 5.0,
            clear_over: 2 * MIN,
        }));
        out.push((Metric::Memory, Level {
            kind: "memory_critical",
            severity: Severity::Error,
            open_at: 97.0_f64.max(at),
            open_over: 2 * MIN,
            clear_below: 92.0_f64.max(at - 5.0),
            clear_over: 2 * MIN,
        }));
    }
    // macOS sizes its swap files to what it uses, so the percentage reads
    // near full on an idle Mac. It only means pressure on Linux.
    if cfg!(target_os = "linux") {
        out.push((Metric::Swap, Level {
            kind: "swap",
            severity: Severity::Warning,
            open_at: 50.0,
            open_over: 10 * MIN,
            clear_below: 40.0,
            clear_over: 2 * MIN,
        }));
    }
    out
}

const CONTAINER_LIMIT: Level = Level {
    kind: "memory_limit",
    severity: Severity::Warning,
    open_at: 95.0,
    open_over: 5 * MIN,
    clear_below: 90.0,
    clear_over: 2 * MIN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Metric {
    Cpu,
    Memory,
    Swap,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Swap => "Swap",
        }
    }
}

/// Percent samples over the longest window any level uses.
#[derive(Debug, Default)]
struct Window(VecDeque<(i64, f64)>);

impl Window {
    const SPAN: i64 = 10 * MIN;

    fn push(&mut self, at: i64, v: f64) {
        self.0.push_back((at, v));
        while self.0.front().is_some_and(|(t, _)| *t <= at - Self::SPAN) {
            self.0.pop_front();
        }
    }

    /// The average over the last `over` seconds, once samples cover it.
    fn mean(&self, now: i64, over: i64) -> Option<f64> {
        let first = self.0.front()?.0;
        // A tick of slack: samples land every 15 s, not on the second.
        if first > now - over + EVERY.as_secs() as i64 {
            return None;
        }
        let (sum, n) = self.0
            .iter()
            .filter(|(t, _)| *t > now - over)
            .fold((0.0, 0u32), |(s, n), (_, v)| (s + v, n + 1));
        (n > 0).then(|| sum / f64::from(n))
    }
}

/// Used bytes every [`Forecast::EVERY`], for a day, per disk.
#[derive(Debug, Default)]
struct Forecast(VecDeque<(i64, f64)>);

impl Forecast {
    const EVERY: i64 = 5 * MIN;
    const SPAN: i64 = 24 * 60 * MIN;
    /// Less than this and a backup or a download reads as a trend.
    const NEEDS: i64 = 6 * 60 * MIN;

    fn push(&mut self, at: i64, used: u64) {
        if self.0.back().is_some_and(|(t, _)| at - t < Self::EVERY) {
            return;
        }
        self.0.push_back((at, used as f64));
        while self.0.front().is_some_and(|(t, _)| *t < at - Self::SPAN) {
            self.0.pop_front();
        }
    }

    /// Bytes a second by least squares, once there's enough history.
    fn slope(&self) -> Option<f64> {
        let (first, last) = (self.0.front()?.0, self.0.back()?.0);
        if last - first < Self::NEEDS {
            return None;
        }
        let n = self.0.len() as f64;
        let mean_t = self.0.iter().map(|(t, _)| (t - first) as f64).sum::<f64>() / n;
        let mean_v = self.0.iter().map(|(_, v)| v).sum::<f64>() / n;
        let (mut num, mut den) = (0.0, 0.0);
        for (t, v) in &self.0 {
            let dt = ((t - first) as f64) - mean_t;
            num += dt * (v - mean_v);
            den += dt * dt;
        }
        (den > 0.0).then(|| num / den)
    }
}

#[derive(Debug, Default)]
pub struct Pressure {
    host: HashMap<Metric, Window>,
    containers: HashMap<String, Window>,
    disks: HashMap<String, Forecast>,
}

fn pct(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64) * 100.0 / (total as f64))
}

fn key(kind: &str) -> String {
    format!("host:{kind}")
}

impl Pressure {
    pub fn observe(
        &mut self,
        vitals: HostVitals,
        containers: &[ContainerInfo],
        disks: &[DiskInfo],
        open: &[Problem],
        limits: Limits,
        now: i64
    ) -> Vec<Report> {
        let mut out = Vec::new();
        self.host_levels(vitals, open, limits, now, &mut out);
        self.container_limits(containers, open, now, &mut out);
        if limits.disk_full_days > 0 {
            self.forecast(disks, open, limits.disk_full_days, now, &mut out);
        } else {
            for p in open.iter().filter(|p| p.kind == "disk_filling") {
                out.push(
                    Report::Resolve(Resolution {
                        key: p.key.clone(),
                        title: format!("{} is no longer forecast", p.subject),
                        detail: None,
                    })
                );
            }
        }
        out
    }

    fn host_levels(&mut self, v: HostVitals, open: &[Problem], limits: Limits, now: i64, out: &mut Vec<Report>) {
        let samples = [
            (Metric::Cpu, Some(f64::from(v.cpu_pct))),
            (Metric::Memory, pct(v.mem_used, v.mem_total)),
            (Metric::Swap, pct(v.swap_used, v.swap_total)),
        ];
        for (m, sample) in samples {
            if let Some(s) = sample {
                self.host.entry(m).or_default().push(now, s);
            }
        }

        for (m, level) in levels(limits) {
            let key = key(level.kind);
            let is_open = open.iter().any(|p| p.key == key);
            let Some(window) = self.host.get(&m) else {
                continue;
            };
            if !is_open {
                if let Some(avg) = window.mean(now, level.open_over).filter(|a| *a >= level.open_at) {
                    let (title, detail) = match m {
                        Metric::Cpu => (
                            format!("CPU has averaged {avg:.0}% for {} minutes", level.open_over / MIN),
                            "Top containers on the Monitoring page shows what's using it.".to_string(),
                        ),
                        Metric::Memory => (
                            format!("Memory is {avg:.0}% used"),
                            format!(
                                "{} of {} in use for {} minutes.",
                                human_bytes(v.mem_used),
                                human_bytes(v.mem_total),
                                level.open_over / MIN
                            ),
                        ),
                        Metric::Swap => (
                            format!("Swap is {avg:.0}% used"),
                            "The host is short of memory and paging to disk.".to_string(),
                        ),
                    };
                    out.push(
                        Report::Open(ProblemSpec {
                            key,
                            category: EventCategory::Host,
                            kind: level.kind,
                            severity: level.severity,
                            subject: m.label().to_string(),
                            title,
                            detail: Some(detail),
                            service: None,
                            depends_on: None,
                        })
                    );
                }
            } else if let Some(avg) = window.mean(now, level.clear_over).filter(|a| *a < level.clear_below) {
                out.push(
                    Report::Resolve(Resolution {
                        key,
                        title: format!("{} is back to {avg:.0}%", m.label()),
                        detail: None,
                    })
                );
            }
        }
        // A level turned off in config: close what it left open.
        let on: Vec<String> = levels(limits)
            .iter()
            .map(|(_, l)| key(l.kind))
            .collect();
        for p in open.iter().filter(|p| p.category == EventCategory::Host && !on.contains(&p.key)) {
            out.push(
                Report::Resolve(Resolution {
                    key: p.key.clone(),
                    title: format!("{} is no longer checked", p.subject),
                    detail: None,
                })
            );
        }
    }

    fn container_limits(&mut self, containers: &[ContainerInfo], open: &[Problem], now: i64, out: &mut Vec<Report>) {
        let level = &CONTAINER_LIMIT;
        for c in containers.iter().filter(|c| c.state == "running" && c.mem_limit_bytes > 0) {
            let Some(p) = pct(c.mem_used_bytes, c.mem_limit_bytes) else {
                continue;
            };
            let window = self.containers.entry(c.name.clone()).or_default();
            window.push(now, p);
            let key = format!("container:{}:{}", c.name, level.kind);
            let is_open = open.iter().any(|p| p.key == key);
            if !is_open {
                if let Some(avg) = window.mean(now, level.open_over).filter(|a| *a >= level.open_at) {
                    out.push(
                        Report::Open(ProblemSpec {
                            key,
                            category: EventCategory::Container,
                            kind: level.kind,
                            severity: level.severity,
                            subject: c.name.clone(),
                            title: format!("{} is at {avg:.0}% of its memory limit", c.name),
                            detail: Some(
                                format!(
                                    "{} of {}. At the limit it is killed.",
                                    human_bytes(c.mem_used_bytes),
                                    human_bytes(c.mem_limit_bytes)
                                )
                            ),
                            service: c.helion_service.clone(),
                            depends_on: None,
                        })
                    );
                }
            } else if let Some(avg) = window.mean(now, level.clear_over).filter(|a| *a < level.clear_below) {
                out.push(
                    Report::Resolve(Resolution {
                        key,
                        title: format!("{} is down to {avg:.0}% of its memory limit", c.name),
                        detail: None,
                    })
                );
            }
        }
        // Stopped containers keep nothing; a removed one's problem is closed
        // by the container tracker, which owns removals.
        self.containers.retain(|name, _| {
            containers.iter().any(|c| &c.name == name && c.state == "running" && c.mem_limit_bytes > 0)
        });
        for p in open.iter().filter(|p| p.kind == level.kind && p.category == EventCategory::Container) {
            let title = match containers.iter().find(|c| c.name == p.subject) {
                Some(c) if c.state != "running" => format!("{} stopped", p.subject),
                Some(c) if c.mem_limit_bytes == 0 => format!("{} no longer has a memory limit", p.subject),
                _ => {
                    continue;
                }
            };
            out.push(Report::Resolve(Resolution { key: p.key.clone(), title, detail: None }));
        }
    }

    fn forecast(&mut self, disks: &[DiskInfo], open: &[Problem], days: u32, now: i64, out: &mut Vec<Report>) {
        const KIND: &str = "disk_filling";
        let open_within = f64::from(days) * 86_400.0;
        let clear_beyond = open_within * 1.5;
        for d in disks.iter().filter(|d| d.total_bytes > 0) {
            let f = self.disks.entry(d.label.clone()).or_default();
            f.push(now, d.used_bytes);
            let key = format!("disk:{}:{KIND}", d.label);
            let is_open = open.iter().any(|p| p.key == key);
            let Some(slope) = f.slope() else {
                continue;
            };
            let free = d.total_bytes.saturating_sub(d.used_bytes) as f64;
            let left = if slope > 0.0 { free / slope } else { f64::INFINITY };
            if !is_open && left < open_within {
                let days_left = (left / 86_400.0).max(0.0);
                out.push(
                    Report::Open(ProblemSpec {
                        key,
                        category: EventCategory::Disk,
                        kind: KIND,
                        severity: Severity::Warning,
                        subject: d.label.clone(),
                        title: if days_left < 1.0 {
                            format!("{} will be full within a day", d.label)
                        } else {
                            format!("{} will be full in about {days_left:.0} days", d.label)
                        },
                        detail: Some(
                            format!(
                                "It has grown by {} a day over the last {} hours; {} is free.",
                                human_bytes((slope * 86_400.0) as u64),
                                (f.0.back().map_or(0, |b| b.0) - f.0.front().map_or(0, |a| a.0)) / 3_600,
                                human_bytes(free as u64)
                            )
                        ),
                        service: None,
                        depends_on: None,
                    })
                );
            } else if is_open && left > clear_beyond {
                out.push(
                    Report::Resolve(Resolution {
                        key,
                        title: format!("{} has stopped filling quickly", d.label),
                        detail: None,
                    })
                );
            }
        }
        self.disks.retain(|label, _| disks.iter().any(|d| &d.label == label));
        for p in open.iter().filter(|p| p.kind == KIND) {
            if !disks.iter().any(|d| d.label == p.subject) {
                out.push(
                    Report::Resolve(Resolution {
                        key: p.key.clone(),
                        title: format!("{} is no longer mounted", p.subject),
                        detail: None,
                    })
                );
            }
        }
    }
}

pub fn spawn(
    events: EventsHandle,
    host: watch::Receiver<Arc<HostSnapshot>>,
    containers: watch::Receiver<Arc<ContainerSnapshot>>,
    limits: Limits
) {
    tokio::spawn(async move {
        let mut pressure = Pressure::default();
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let (vitals, disks) = {
                let h = host.borrow();
                (h.vitals, h.disks.clone())
            };
            let list = containers.borrow().containers.clone();
            let reports = pressure.observe(vitals, &list, &disks, &events.open_problems(), limits, unix_now());
            events.report_all(reports);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use helion_common::types::DiskKind;

    const LIMITS: Limits = Limits { cpu_pct: 90, memory_pct: 90, disk_full_days: 7 };
    const GB: u64 = 1_000_000_000;

    fn vitals(cpu: f32, mem_pct: u64) -> HostVitals {
        HostVitals { cpu_pct: cpu, mem_used: mem_pct * GB, mem_total: 100 * GB, swap_used: 0, swap_total: 0 }
    }

    fn problem(key: &str, kind: &str, category: EventCategory, subject: &str) -> Problem {
        Problem {
            key: key.into(),
            category,
            kind: kind.into(),
            severity: Severity::Warning,
            subject: subject.into(),
            title: String::new(),
            detail: None,
            service: None,
            opened_at: 0,
            event_id: 1,
            depends_on: None,
        }
    }

    fn keys(out: &[Report]) -> Vec<String> {
        out.iter()
            .map(|r| {
                match r {
                    Report::Open(p) => format!("open {}", p.key),
                    Report::Resolve(r) => format!("resolve {}", r.key),
                    Report::Event(e) => format!("event {}", e.kind),
                }
            })
            .collect()
    }

    /// Feeds the same reading every 15 s from `from` to `to`, returning the
    /// last tick's reports.
    fn run(p: &mut Pressure, v: HostVitals, open: &[Problem], from: i64, to: i64) -> Vec<Report> {
        let mut last = Vec::new();
        let mut t = from;
        while t <= to {
            last = p.observe(v, &[], &[], open, LIMITS, t);
            t += 15;
        }
        last
    }

    #[test]
    fn a_burst_does_not_open_but_a_sustained_load_does() {
        let mut p = Pressure::default();
        assert!(run(&mut p, vitals(99.0, 10), &[], 0, 5 * MIN).is_empty(), "five hot minutes");
        let out = run(&mut p, vitals(99.0, 10), &[], 5 * MIN + 15, 10 * MIN);
        assert_eq!(keys(&out), ["open host:cpu"]);
        let Report::Open(spec) = &out[0] else { unreachable!() };
        assert_eq!(spec.title, "CPU has averaged 99% for 10 minutes");
    }

    #[test]
    fn it_clears_only_well_below_the_line() {
        let open = [problem("host:cpu", "cpu", EventCategory::Host, "CPU")];
        let mut p = Pressure::default();
        assert!(run(&mut p, vitals(85.0, 10), &open, 0, 10 * MIN).is_empty(), "below 90, not below 80");
        let out = run(&mut p, vitals(20.0, 10), &open, 10 * MIN + 15, 13 * MIN);
        assert_eq!(keys(&out), ["resolve host:cpu"]);
    }

    #[test]
    fn memory_has_a_warning_and_a_critical_level() {
        let mut p = Pressure::default();
        let out = run(&mut p, vitals(1.0, 98), &[], 0, 5 * MIN);
        assert_eq!(keys(&out), ["open host:memory", "open host:memory_critical"]);
    }

    #[test]
    fn a_turned_off_level_closes_what_it_left_open() {
        let open = [problem("host:cpu", "cpu", EventCategory::Host, "CPU")];
        let mut p = Pressure::default();
        let limits = Limits { cpu_pct: 0, ..LIMITS };
        let out = p.observe(vitals(99.0, 10), &[], &[], &open, limits, 0);
        assert_eq!(keys(&out), ["resolve host:cpu"]);
    }

    fn container(mem: u64, limit: u64) -> ContainerInfo {
        ContainerInfo {
            id: "id".into(),
            name: "immich".into(),
            image: "img".into(),
            status: String::new(),
            state: "running".into(),
            health: None,
            ports: vec![],
            started_at: None,
            created_unix: 0,
            restart_count: 0,
            compose_project: None,
            compose_service: None,
            helion_service: Some("immich".into()),
            helion_service_description: None,
            helion_service_url: None,
            icon: None,
            update_labels: Default::default(),
            cpu_pct: 0.0,
            mem_used_bytes: mem,
            mem_limit_bytes: limit,
            net_rx_bps: None,
            net_tx_bps: None,
            blk_read_bps: None,
            blk_write_bps: None,
        }
    }

    #[test]
    fn a_container_near_its_limit_opens_and_clears() {
        let mut p = Pressure::default();
        let mut out = Vec::new();
        for t in (0..=5 * MIN).step_by(15) {
            out = p.observe(vitals(1.0, 10), &[container(97, 100)], &[], &[], LIMITS, t);
        }
        assert_eq!(keys(&out), ["open container:immich:memory_limit"]);

        let open = [problem("container:immich:memory_limit", "memory_limit", EventCategory::Container, "immich")];
        for t in (5 * MIN..=8 * MIN).step_by(15) {
            out = p.observe(vitals(1.0, 10), &[container(50, 100)], &[], &open, LIMITS, t);
        }
        assert_eq!(keys(&out), ["resolve container:immich:memory_limit"]);
    }

    fn disk(used: u64) -> DiskInfo {
        DiskInfo {
            mount: "/".into(),
            label: "/".into(),
            used_bytes: used,
            total_bytes: 1_000 * GB,
            read_bps: 0.0,
            write_bps: 0.0,
            io_available: true,
            kind: DiskKind::Ssd,
        }
    }

    #[test]
    fn a_disk_filling_fast_is_forecast_once_there_is_enough_history() {
        let mut p = Pressure::default();
        // 50 GB an hour with 500 GB free: full in 10 hours.
        let mut out = Vec::new();
        for t in (0..=6 * 3_600).step_by(300) {
            let used = 500 * GB + (t as u64) * 50 * GB / 3_600;
            out = p.observe(vitals(1.0, 10), &[], &[disk(used)], &[], LIMITS, t);
            if t < 6 * 3_600 {
                assert!(out.is_empty(), "too little history at {t}");
            }
        }
        assert_eq!(keys(&out), ["open disk:/:disk_filling"]);
        let Report::Open(spec) = &out[0] else { unreachable!() };
        assert_eq!(spec.title, "/ will be full within a day");
    }

    #[test]
    fn a_steady_disk_is_not_forecast_and_an_open_forecast_clears() {
        let open = [problem("disk:/:disk_filling", "disk_filling", EventCategory::Disk, "/")];
        let mut p = Pressure::default();
        let mut out = Vec::new();
        for t in (0..=6 * 3_600).step_by(300) {
            out = p.observe(vitals(1.0, 10), &[], &[disk(500 * GB)], &open, LIMITS, t);
        }
        assert_eq!(keys(&out), ["resolve disk:/:disk_filling"]);
    }
}
