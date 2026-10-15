//! The center panel, as its own view so GPUI can reuse its last frame.
//!
//! Hovering a row in the sidebar re-renders the workspace; without this,
//! that would re-render the whole diff, with a full file, on every mouse
//! move. The panel renders again only when what it shows changes.

use gpui_kit::*;

use super::Workspace;

pub struct MainPanel {
    workspace: WeakEntity<Workspace>,
    /// [`Workspace::main_key`] as of the last render.
    key: u64,
    _subscriptions: Vec<Subscription>,
}

impl MainPanel {
    pub fn new(workspace: &Entity<Workspace>, diff: &Entity<gpui_kit::component::diff::DiffState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            // The workspace also notifies on hover changes in the sidebar;
            // only a change to what this panel shows re-renders it.
            cx.observe(workspace, |this, workspace, cx| {
                let key = workspace.read(cx).main_key();
                if key != this.key {
                    this.key = key;
                    cx.notify();
                }
            }),
            // Syntax highlighting arriving, folds, selections.
            cx.observe(diff, |_, _, cx| cx.notify()),
        ];
        Self { workspace: workspace.downgrade(), key: 0, _subscriptions: subscriptions }
    }

    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }
}

impl Render for MainPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        workspace.update(cx, |workspace, cx| workspace.render_main(cx))
    }
}
