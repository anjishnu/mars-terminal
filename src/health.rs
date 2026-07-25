//! The SPACES health line: session uptime + host load / disk / memory, plus an
//! optional GPU-memory reading on machines that have `nvidia-smi`.
//!
//! Cheap probes (load, disk, memory — via the `sys::health` PAL) are sampled inline
//! on an interval; host memory is smoothed with an exponential moving average so it
//! reads as a "last few minutes" figure rather than a jittery instant. GPU is polled
//! OFF-THREAD with `nvidia-smi` (a subprocess would otherwise hitch the UI) and the
//! segment is simply omitted when the tool is absent — the "are we on a GPU box?"
//! gate for free.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

/// Memory-EMA time constant: how many seconds of history the smoothing reflects.
const MEM_SMOOTH_SECS: f64 = 180.0;

pub struct Health {
    start: Instant,
    interval_secs: u64,
    last_sample: Option<Instant>,
    /// Smoothed host-memory-used %, EMA over ~`MEM_SMOOTH_SECS`.
    mem_ema: Option<f64>,
    load1: Option<f64>,
    disk_free_pct: Option<u8>,
    gpu_pct: Option<u8>,
    gpu_inflight: bool,
    gpu_tx: Sender<Option<u8>>,
    gpu_rx: Receiver<Option<u8>>,
}

impl Health {
    pub fn new(interval_secs: u64) -> Self {
        let (gpu_tx, gpu_rx) = channel();
        Health {
            start: Instant::now(),
            interval_secs: interval_secs.max(1),
            last_sample: None,
            mem_ema: None,
            load1: None,
            disk_free_pct: None,
            gpu_pct: None,
            gpu_inflight: false,
            gpu_tx,
            gpu_rx,
        }
    }

    fn alpha(&self) -> f64 {
        let i = self.interval_secs as f64;
        (i / (MEM_SMOOTH_SECS + i)).clamp(0.0, 1.0)
    }

    /// Refresh the cheap probes if the interval has elapsed, drain any GPU result,
    /// and (when `gpu` is set) kick off a background `nvidia-smi` poll. Returns true
    /// if any displayed value changed, so the caller can repaint when the panel is up.
    pub fn maybe_sample(&mut self, cwd: &Path, gpu: bool) -> bool {
        let mut changed = false;
        while let Ok(v) = self.gpu_rx.try_recv() {
            if self.gpu_pct != v { changed = true; }
            self.gpu_pct = v;
            self.gpu_inflight = false;
        }
        let now = Instant::now();
        let due = self
            .last_sample
            .map(|t| now.duration_since(t).as_secs() >= self.interval_secs)
            .unwrap_or(true);
        if !due {
            return changed;
        }
        self.last_sample = Some(now);
        self.load1 = crate::sys::health::load_avg1();
        self.disk_free_pct = crate::sys::health::disk_free_pct(cwd);
        if let Some(m) = crate::sys::health::mem_used_pct() {
            let s = m as f64;
            self.mem_ema = Some(match self.mem_ema {
                Some(e) => e + self.alpha() * (s - e),
                None => s,
            });
        }
        if gpu && !self.gpu_inflight {
            self.gpu_inflight = true;
            let tx = self.gpu_tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(query_gpu());
            });
        }
        true
    }

    /// The one-line status string, e.g. `up 2h 13m · load 1.40 · mem 63% · disk 82% free · gpu 40%`.
    /// Any probe that returned `None` is silently dropped, so the line self-trims.
    pub fn line(&self) -> String {
        let mut parts = vec![format!("up {}", fmt_uptime(self.start.elapsed().as_secs()))];
        if let Some(l) = self.load1 {
            parts.push(format!("load {l:.2}"));
        }
        if let Some(m) = self.mem_ema {
            parts.push(format!("mem {}%", m.round() as u8));
        }
        if let Some(d) = self.disk_free_pct {
            parts.push(format!("disk {d}% free"));
        }
        if let Some(g) = self.gpu_pct {
            parts.push(format!("gpu {g}%"));
        }
        parts.join(" · ")
    }
}

/// `45s` / `13m` / `2h 13m` — compact uptime.
fn fmt_uptime(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Ask `nvidia-smi` for GPU-memory used/total and reduce to a percent. `None` when
/// the tool is missing, errors, or reports nothing — i.e. "no GPU here".
fn query_gpu() -> Option<u8> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used,memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Average across GPUs if there are several.
    let (mut used, mut total) = (0f64, 0f64);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.split(',');
        let u: f64 = it.next()?.trim().parse().ok()?;
        let t: f64 = it.next()?.trim().parse().ok()?;
        used += u;
        total += t;
    }
    if total <= 0.0 {
        return None;
    }
    Some(((used * 100.0 / total).round() as u8).min(100))
}
