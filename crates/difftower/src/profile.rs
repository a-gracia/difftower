//! Frame timing for `DIFFTOWER_PROFILE=1`: how long each frame takes from
//! the workspace render to the end of its paint, summarized on stderr every
//! couple of seconds. Off by default, and then free.

use std::{
    cell::RefCell,
    sync::OnceLock,
    time::{Duration, Instant},
};

const REPORT_EVERY: Duration = Duration::from_secs(2);
/// A frame slower than this misses a 120 Hz refresh.
const BUDGET: Duration = Duration::from_millis(8);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DIFFTOWER_PROFILE").is_some())
}

#[derive(Default)]
struct Stats {
    frame_start: Option<Instant>,
    frames: Vec<Duration>,
    since: Option<Instant>,
}

thread_local! {
    static STATS: RefCell<Stats> = RefCell::default();
}

/// Marks the start of a frame: the workspace starts rendering.
pub fn frame_start() {
    if enabled() {
        STATS.with_borrow_mut(|stats| stats.frame_start = Some(Instant::now()));
    }
}

/// Marks the end of a frame: the workspace's last element painted.
pub fn frame_end() {
    if !enabled() {
        return;
    }
    STATS.with_borrow_mut(|stats| {
        let Some(start) = stats.frame_start.take() else { return };
        stats.frames.push(start.elapsed());
        let since = *stats.since.get_or_insert_with(Instant::now);
        if since.elapsed() >= REPORT_EVERY {
            report(&mut stats.frames);
            stats.since = Some(Instant::now());
        }
    });
}

fn report(frames: &mut Vec<Duration>) {
    frames.sort();
    let at = |q: f64| frames[((frames.len() - 1) as f64 * q) as usize];
    let slow = frames.iter().filter(|f| **f > BUDGET).count();
    eprintln!(
        "difftower frames: {} · median {:.1} ms · p95 {:.1} ms · worst {:.1} ms · {} over {} ms",
        frames.len(),
        ms(at(0.5)),
        ms(at(0.95)),
        ms(*frames.last().unwrap()),
        slow,
        BUDGET.as_millis(),
    );
    frames.clear();
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}
