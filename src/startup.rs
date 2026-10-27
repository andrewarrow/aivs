use std::{sync::OnceLock, time::Instant};

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Startup-only checkpoints: elapsed since main and since the previous checkpoint.
pub(crate) struct Timer {
    previous: Instant,
}

impl Timer {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        STARTED.get_or_init(|| now);
        Self { previous: now }
    }

    pub(crate) fn mark(&mut self, label: &str) {
        let now = Instant::now();
        println!(
            "[startup +{:.1} ms] {} ({:.1} ms)",
            now.duration_since(*STARTED.get().expect("startup timer initialized"))
                .as_secs_f64() * 1000.0,
            label,
            now.duration_since(self.previous).as_secs_f64() * 1000.0,
        );
        self.previous = now;
    }
}
