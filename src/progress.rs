use std::time::{Duration, Instant};

pub struct Progress {
    label: String,
    started: Instant,
    last: Instant,
    total: usize,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: usize) -> Self {
        let label = label.into();
        eprintln!("{label}: started ({total} entries)");

        Self {
            label,
            started: Instant::now(),
            last: Instant::now(),
            total,
        }
    }

    pub fn update(&mut self, done: usize, detail: impl std::fmt::Display) {
        if self.last.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "{}: {done}/{} ({:.0}s) {detail}",
                self.label,
                self.total,
                self.started.elapsed().as_secs_f64()
            );
            self.last = Instant::now();
        }
    }

    pub fn finish(&self) {
        eprintln!(
            "{}: complete in {:.1}s",
            self.label,
            self.started.elapsed().as_secs_f64()
        );
    }
}
