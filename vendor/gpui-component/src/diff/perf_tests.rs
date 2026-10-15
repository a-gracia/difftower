// Difftower: how long a frame takes with a whole large file in the diff.
// Run with `--ignored --nocapture`; release mode gives realistic numbers.

use std::time::Instant;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    Window, div, px,
};

use super::{Diff, DiffFile, DiffMode, DiffState};

struct Harness(Entity<DiffState>);

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
    for (lines, mode) in [(40, DiffMode::Unified), (3000, DiffMode::Unified), (40, DiffMode::Split), (3000, DiffMode::Split)] {
        let state = cx.new(|cx| DiffState::new([whole_file(lines)], cx).with_context_lines(None).with_mode(mode));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| Harness(state)
        });
        for _ in 0..5 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }
        let frames = 30;
        let start = Instant::now();
        for _ in 0..frames {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx)
            });
        }
        println!("{lines} lines, {mode:?}: {:.2} ms per frame", start.elapsed().as_secs_f64() * 1000. / frames as f64);
    }
}
