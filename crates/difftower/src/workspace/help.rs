//! The help sheet, on `?` or F1: what the ideas in Difftower mean, and the
//! keys that move around it.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::Workspace;

const IDEAS: &[(&str, &str)] = &[
    ("Changes", "Every file that differs from the last commit, or from the start of the session if one is running."),
    (
        "Session",
        "Start one before handing work to an agent: what changes stays in view even after the agent commits it, until you accept or revert it.",
    ),
    (
        "Protected",
        "Files and folders the agent must not touch, saved in .difftower.toml. A change to one is flagged in red, or in amber when it only adds lines.",
    ),
    ("Tests", "Run all of them, a folder, a file or a single test; failures link to the line that failed."),
];

const KEYS: &[(&str, &str)] = &[
    ("Ctrl+P", "Go to file"),
    ("J / K", "Next / previous file"),
    ("N / P", "Next / previous change in the file"),
    ("Ctrl+1 … 4", "Changes, Files, Protected, Tests"),
    ("Ctrl+Shift+T", "Run all tests"),
    ("Ctrl+`", "Move the keyboard to the terminal and back"),
    ("Alt+Z", "Wrap long lines"),
    ("? or F1", "This help"),
    ("Esc", "Close it"),
];

impl Workspace {
    pub(super) fn render_help(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_help {
            return None;
        }
        let theme = cx.theme();
        let heading = |text: &'static str| div().text_xs().font_semibold().text_color(theme.muted_foreground).child(text);
        let ideas = IDEAS.iter().map(|(name, text)| {
            v_flex().gap_0p5().child(div().font_semibold().child(*name)).child(
                div().text_sm().text_color(theme.muted_foreground).child(*text),
            )
        });
        let keys = KEYS.iter().map(|(key, what)| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(
                    h_flex().w(px(110.)).flex_none().child(
                        div().flex_none().px_1p5().rounded_sm().bg(theme.secondary).child(*key),
                    ),
                )
                .child(*what)
        });
        Some(
            div()
                .id("help")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(64.))
                .bg(gpui_kit::black().opacity(0.25))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.toggle_help(window, cx)))
                .child(
                    h_flex()
                        .occlude()
                        .items_start()
                        .w(px(980.))
                        .max_w_full()
                        .p_5()
                        .gap_8()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .shadow_lg()
                        .child(v_flex().flex_1().min_w_0().gap_4().child(heading("HOW IT WORKS")).children(ideas))
                        .child(v_flex().flex_none().w(px(420.)).gap_2().child(heading("KEYBOARD")).children(keys)),
                )
                .into_any_element(),
        )
    }

    /// Opening it takes the keyboard from the terminal, so Esc closes it.
    pub(super) fn toggle_help(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_help = !self.show_help;
        if self.show_help {
            self.focus_workspace(window, cx);
        }
        cx.notify();
    }
}
