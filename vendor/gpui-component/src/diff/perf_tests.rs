// Difftower: how long a frame takes with a whole large file in the diff.
// Run with `--ignored --nocapture`; release mode gives realistic numbers.

use std::time::Instant;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    Window, div, px,
};

use super::{Diff, DiffFile, DiffMode, DiffState};

struct Harness(Entity<DiffState>);

/// The same window with nothing in it, for the platform's own cost per frame.
struct Empty;

impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1000.)).h(px(800.))
    }
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1000.)).h(px(800.)).flex().flex_col().child(Diff::new(&self.0))
    }
}

fn whole_file(lines: usize) -> DiffFile {
    let mut body = String::new();
    for i in 1..=lines {
        if i % 50 == 0 {
            body.push_str(&format!("-    let value_{i} = compute({i});\n+    let value_{i} = compute_fast({i}, cache);\n"));
        } else {
            body.push_str(&format!("     let value_{i} = compute({i}); // unchanged line\n"));
        }
    }
    let patch = format!("diff --git a/big.rs b/big.rs\n--- a/big.rs\n+++ b/big.rs\n@@ -1,{lines} +1,{lines} @@\n{body}");
    DiffFile::parse(&patch).unwrap().remove(0)
}

#[gpui::test]
#[ignore]
fn frame_time_with_a_large_whole_file(cx: &mut TestAppContext) {
    cx.update(crate::init);
    // Each set turns parts off through `DT_ABL`, to see what a frame spends on.
    {
        let (_, cx) = cx.add_window_view(|_, _| Empty);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let start = Instant::now();
        for _ in 0..30 {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx)
            });
        }
        println!("empty window: {:.2} ms per frame", start.elapsed().as_secs_f64() * 1000. / 30.);
    }
    let sets = [""];
    for mode in [DiffMode::Unified, DiffMode::Split] {
        for set in sets {
            super::probe::set_off(set);
            let inline = if super::probe::off("inline") { None } else { Some(super::DiffInlineUnit::Word) };
            let state = cx.new(|cx| {
                DiffState::new([whole_file(3000)], cx).with_context_lines(None).with_mode(mode).with_inline_unit(inline)
            });
            let (_, cx) = cx.add_window_view({
                let state = state.clone();
                move |_, _| Harness(state)
            });
            for _ in 0..5 {
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.run_until_parked();
            }
            super::probe::take_row_nanos();
            let profiler = (set.is_empty() && std::env::var_os("DT_PPROF").is_some())
                .then(|| pprof::ProfilerGuard::new(2000).unwrap());
            let frames = if profiler.is_some() { 300 } else { 30 };
            let start = Instant::now();
            for _ in 0..frames {
                cx.update(|window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx)
                });
            }
            let total = start.elapsed().as_secs_f64() * 1000. / frames as f64;
            let rows = super::probe::take_row_nanos() as f64 / 1e6 / frames as f64;
            println!("{mode:?} off=[{set}]: {total:.2} ms per frame, {rows:.2} ms building rows");
            if let Some(profiler) = profiler {
                print_hotspots(&format!("{mode:?}"), &profiler.report().build().unwrap());
            }
        }
    }
    super::probe::set_off("");
}

/// Prints the functions with the most samples, by self time and by total
/// time, skipping the standard library and the test harness.
fn print_hotspots(label: &str, report: &pprof::Report) {
    use std::collections::HashMap;
    let mut own: HashMap<String, usize> = HashMap::new();
    let mut total: HashMap<String, usize> = HashMap::new();
    let mut samples = 0;
    for (frames, count) in &report.data {
        let count = *count as usize;
        samples += count;
        let names: Vec<String> = frames
            .frames
            .iter()
            .flat_map(|frame| frame.iter().map(|symbol| symbol.name()))
            .collect();
        if let Some(first) = names.first() {
            *own.entry(first.clone()).or_default() += count;
        }
        let mut seen = std::collections::HashSet::new();
        for name in names {
            if seen.insert(name.clone()) {
                *total.entry(name).or_default() += count;
            }
        }
    }
    let top = |map: &HashMap<String, usize>, n: usize| {
        let mut rows: Vec<_> = map.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        rows.into_iter()
            .filter(|(name, _)| !name.contains("test::run_test") && !name.starts_with("std::rt") && !name.starts_with("core::ops::function"))
            .take(n)
            .map(|(name, count)| format!("  {:5.1}%  {}", 100. * *count as f64 / samples as f64, shorten(name)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    println!("--- {label}: {samples} samples, by self time\n{}", top(&own, 25));
    println!("--- {label}: by total time\n{}", top(&total, 140));
}

fn shorten(name: &str) -> String {
    let name = name.split("::h").next().unwrap_or(name);
    if name.len() > 150 { format!("{}…", &name[..150]) } else { name.to_owned() }
}
