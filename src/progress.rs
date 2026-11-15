use std::{
    fmt::Display,
    path::Path,
    sync::OnceLock,
    time::{Duration, Instant},
};

pub fn log(build: &str, operation: &str, message: impl Display) {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    let elapsed = STARTED.get_or_init(Instant::now).elapsed().as_secs_f64();
    eprintln!("[{elapsed:>7.1}s] [{build}] [{operation}] {message}");
}

pub struct Progress {
    build: String,
    operation: &'static str,
    unit: &'static str,
    started: Instant,
    last: Instant,
    total: usize,
}

impl Progress {
    pub fn for_path(
        path: &Path,
        operation: &'static str,
        total: usize,
        unit: &'static str,
    ) -> Self {
        let build = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        log(&build, operation, format!("started; {unit}={total}"));

        Self {
            build,
            operation,
            unit,
            started: Instant::now(),
            last: Instant::now(),
            total,
        }
    }

    pub fn update(&mut self, done: usize, detail: impl Display) {
        if self.last.elapsed() >= Duration::from_secs(5) {
            let message = format!(
                "{}={done}/{}; elapsed={:.0}s {detail}",
                self.unit,
                self.total,
                self.started.elapsed().as_secs_f64(),
            );
            log(&self.build, self.operation, message.trim_end());
            self.last = Instant::now();
        }
    }

    pub fn message(&self, message: impl Display) {
        log(&self.build, self.operation, message);
    }

    pub fn finish(&self) {
        log(
            &self.build,
            self.operation,
            format!("complete; {:.1}s", self.started.elapsed().as_secs_f64()),
        );
    }
}
