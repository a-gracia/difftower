// Difftower: a file opened into an empty diff scrolls at once, inside a
// cached view as Difftower's center panel is.

use gpui::{
    AnyView, AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, StyleRefinement, Styled as _, TestAppContext, VisualTestContext,
    Window, div, point, px,
};

use super::{Diff, DiffFile, DiffMode, DiffState};

struct Panel {
    state: Entity<DiffState>,
}

impl Render for Panel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Difftower's center panel: the diff, with its header, beside a strip.
        div()
            .size_full()
            .flex()
            .child(div().flex_1().min_w_0().h_full().child(Diff::new(&self.state)))
            .child(div().flex_none().w(px(12.)).h_full())
    }
}

struct Root {
    panel: Entity<Panel>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Difftower's workspace: toolbar, sidebar, center panel, status bar.
        div()
            .w(px(900.))
            .h(px(400.))
            .flex()
            .flex_col()
            .child(div().h(px(40.)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(div().flex_none().w(px(250.)).h_full())
                    .child(
                        div().flex_1().min_w_0().h_full().overflow_hidden().child(
                            AnyView::from(self.panel.clone()).cached(StyleRefinement::default().size_full()),
                        ),
                    ),
            )
            .child(div().h(px(28.)))
    }
}

fn whole_file(name: &str, lines: usize) -> DiffFile {
    let body: String = (0..lines).map(|i| format!(" let value_{i} = {i};\n")).collect();
    let patch = format!(
        "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1,{lines} +1,{lines} @@\n{body}"
    );
    DiffFile::parse(&patch).unwrap().remove(0)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }
}

fn settle(cx: &mut VisualTestContext, forced: bool) {
    if forced {
        draw(cx);
    } else {
        cx.run_until_parked();
    }
}

fn scroll_down(cx: &mut VisualTestContext, forced: bool) {
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(550.), px(200.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-400.))),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    if forced {
        draw(cx);
    } else {
        cx.run_until_parked();
    }
}

#[gpui::test]
fn a_file_opened_into_an_empty_diff_scrolls_at_once(cx: &mut TestAppContext) {
    // As in a window: frames only when something asks for one.
    for (mode, forced) in [(DiffMode::Unified, false), (DiffMode::Split, false), (DiffMode::Unified, true)] {
        cx.update(crate::init);
        let state = cx.new(|cx| DiffState::new([], cx).with_mode(mode).with_context_lines(None));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, cx| {
                let panel = cx.new(|cx| {
                    cx.observe(&state, |_, _, cx| cx.notify()).detach();
                    Panel { state: state.clone() }
                });
                Root { panel }
            }
        });
        draw(cx);
        // Opening a file, then another, as clicking in Difftower's list:
        // the diff empties, then the file arrives.
        for name in ["a.rs", "b.rs"] {
            state.update(cx, |state, cx| state.set_files([], cx));
            settle(cx, forced);
            state.update(cx, |state, cx| state.set_files([whole_file(name, 500)], cx));
            settle(cx, forced);
            scroll_down(cx, forced);
            scroll_down(cx, forced);
            let top = cx.update(|_, cx| state.read(cx).minimap_view().1);
            assert!(top > 1., "{mode:?} forced={forced} {name}: the wheel did not scroll, first row in view {top}");
        }
    }
}

struct Narrow {
    state: Entity<DiffState>,
    soft_wrap: bool,
}

impl Render for Narrow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(240.)).h(px(400.)).flex().flex_col().child(Diff::new(&self.state).header_visible(false).soft_wrap(self.soft_wrap))
    }
}

/// GPUI's list drops the row height estimate of unmeasured rows on its first
/// layout; the wheel must still reach the last row, wrapped or not.
#[gpui::test]
fn the_wheel_reaches_the_end_of_a_file(cx: &mut TestAppContext) {
    for soft_wrap in [true, false] {
        reach_the_end(cx, soft_wrap);
    }
}

fn reach_the_end(cx: &mut TestAppContext, soft_wrap: bool) {
    cx.update(crate::init);
    let body: String = (0..300)
        .map(|i| format!(" \"key_{i}\": \"a value long enough to wrap over several rows in a narrow panel, number {i}\",\n"))
        .collect();
    let patch = format!("diff --git a/a.json b/a.json\n--- a/a.json\n+++ b/a.json\n@@ -1,300 +1,300 @@\n{body}");
    let file = DiffFile::parse(&patch).unwrap().remove(0);
    let state = cx.new(|cx| DiffState::new([file], cx).with_context_lines(None));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Narrow { state, soft_wrap }
    });
    draw(cx);
    for _ in 0..400 {
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(120.), px(200.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-300.))),
            modifiers: Modifiers::default(),
            ..Default::default()
        });
        cx.run_until_parked();
        if std::env::var_os("SCROLL_TRACE").is_some() {
            let (first, max) = cx.update(|_, cx| {
                let list = state.read(cx).list().clone();
                (state.read(cx).minimap_view().1, list.max_offset_for_scrollbar().y)
            });
            let width = cx.update(|_, cx| state.read(cx).list().viewport_bounds().size.width);
            eprintln!("first row {first:.2}, scroll max {max:?}, list width {width:?}");
        }
    }
    draw(cx);
    let (last_shown, at_end, top) = cx.update(|_, cx| {
        let state = state.read(cx);
        let last = state.rows().len() - 1;
        (state.list().bounds_for_item(last).is_some(), state.list().is_scrolled_to_end(), state.minimap_view().1)
    });
    assert!(
        last_shown && at_end == Some(true),
        "soft_wrap={soft_wrap}: the last row never came into view: first row {top}, at end {at_end:?}"
    );
}
