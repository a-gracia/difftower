// Difftower: each side of a split diff scrolls sideways on its own.

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, VisualTestContext, Window, div,
    point, px,
};

use super::{Diff, DiffFile, DiffMode, DiffState};

const WIDTH: f32 = 600.;

struct Harness {
    state: Entity<DiffState>,
    soft_wrap: bool,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(WIDTH))
            .h(px(300.))
            .flex()
            .flex_col()
            .child(Diff::new(&self.state).header_visible(false).soft_wrap(self.soft_wrap))
    }
}

fn long_line(i: usize, value: &str) -> String {
    format!("let config_{i} = Config {{ host: \"server-{i}.example.com\", port: {}, timeout: {value}, retry: Retry::exponential({i}) }};\n", 8000 + i)
}

fn harness(cx: &mut TestAppContext, mode: DiffMode, soft_wrap: bool) -> (Entity<DiffState>, &mut VisualTestContext) {
    cx.update(crate::init);
    let original: String = (0..10).map(|i| long_line(i, "30")).collect();
    let modified: String = (0..10).map(|i| long_line(i, "45")).collect();
    let file = DiffFile::parse(&format!(
        "diff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1,10 +1,10 @@\n{}{}",
        original.lines().map(|l| format!("-{l}\n")).collect::<String>(),
        modified.lines().map(|l| format!("+{l}\n")).collect::<String>(),
    ))
    .unwrap();
    let state = cx.new(|cx| DiffState::new(file, cx).with_mode(mode));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Harness { state, soft_wrap }
    });
    // The first frame measures; the second lays out with the measurement.
    for _ in 0..3 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }
    (state, cx)
}

fn scroll_horizontally(cx: &mut VisualTestContext, x: f32, dx: f32) {
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(x), px(150.)),
        delta: ScrollDelta::Pixels(point(px(dx), px(0.))),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn split_sides_fit_the_viewport_and_scroll_on_their_own(cx: &mut TestAppContext) {
    let (state, cx) = harness(cx, DiffMode::Split, false);
    let (viewport, content) = cx.update(|_, cx| {
        let state = state.read(cx);
        (state.viewport().width, state.content_width())
    });
    assert!(content / 2. > viewport / 2., "the fixture's lines must be wider than a column");

    // Over the right half: only the right side moves.
    scroll_horizontally(cx, WIDTH * 0.75, -120.);
    let offsets = cx.update(|_, cx| state.read(cx).split_offsets());
    assert_eq!(offsets, [px(0.), px(120.)]);
    // The list itself never scrolls sideways.
    assert_eq!(cx.update(|_, cx| state.read(cx).horizontal_scroll().offset().x), px(0.));

    // Over the left half: only the left side moves.
    scroll_horizontally(cx, WIDTH * 0.25, -50.);
    assert_eq!(cx.update(|_, cx| state.read(cx).split_offsets()), [px(50.), px(120.)]);

    // Neither side scrolls past its longest line, nor before its start.
    scroll_horizontally(cx, WIDTH * 0.75, -100_000.);
    scroll_horizontally(cx, WIDTH * 0.25, 100_000.);
    let [left, right] = cx.update(|_, cx| state.read(cx).split_offsets());
    assert_eq!(left, px(0.));
    assert_eq!(right, (content / 2. - viewport / 2.).max(px(0.)));
}

#[gpui::test]
fn wrapped_and_unified_diffs_keep_one_shared_scroll(cx: &mut TestAppContext) {
    for (mode, soft_wrap) in [(DiffMode::Split, true), (DiffMode::Unified, false)] {
        let (state, cx) = harness(cx, mode, soft_wrap);
        scroll_horizontally(cx, WIDTH * 0.75, -120.);
        assert_eq!(cx.update(|_, cx| state.read(cx).split_offsets()), [px(0.), px(0.)]);
    }
}
