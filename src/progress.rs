use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

#[derive(Clone)]
pub struct TransferProgress {
    bar: ProgressBar,
}

impl TransferProgress {
    pub fn new(label: impl Into<String>, total: Option<u64>, enabled: bool) -> Self {
        let visible = enabled && ENABLED.load(Ordering::Relaxed) && io::stderr().is_terminal();
        let bar = if !visible {
            ProgressBar::hidden()
        } else if let Some(total) = total {
            let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
            bar.set_style(
                ProgressStyle::with_template(
                    "{spinner:.cyan} {msg} [{bar:28.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
                )
                .expect("valid progress template")
                .progress_chars("=>-"),
            );
            bar
        } else {
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
            bar.set_style(
                ProgressStyle::with_template(
                    "{spinner:.cyan} {msg} {bytes} {bytes_per_sec} {elapsed_precise}",
                )
                .expect("valid progress template"),
            );
            bar
        };
        bar.set_message(label.into());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    pub fn inc(&self, bytes: usize) {
        self.bar.inc(bytes as u64);
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
