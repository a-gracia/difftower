//! Go to file: `Ctrl+P` (`Cmd+P` on macOS) and a few letters of a path, matched in order.

use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::*;

use super::Workspace;

/// Matches shown at once; typing more narrows them.
const SHOWN: usize = 50;

pub(super) struct Finder {
    state: Entity<CommandState>,
    query: String,
}

impl Workspace {
    pub(super) fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        self.finder = Some(Finder { state, query: String::new() });
        cx.notify();
    }

    fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        self.focus_workspace(window, cx);
        cx.notify();
    }

    /// Takes the keyboard back for the navigation keys.
    pub(super) fn focus_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    pub(super) fn render_finder(&self, _: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let finder = self.finder.as_ref()?;
        let matches: Rc<Vec<String>> = Rc::new(best_matches(&finder.query, &self.files, SHOWN));
        let this = cx.entity().downgrade();
        let (on_query, on_confirm, on_cancel) = (this.clone(), this.clone(), this);
        let picked = matches.clone();
        let command = Command::new(&finder.state)
            .filterable(false)
            .placeholder("Go to file…")
            .items(matches.iter().map(|path| CommandItem::new().label(path.clone())))
            .on_query(move |query, _, cx| {
                let query = query.to_owned();
                on_query
                    .update(cx, |this, cx| {
                        if let Some(finder) = &mut this.finder {
                            finder.query = query;
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .on_confirm(move |ix, window, cx| {
                let Some(path) = picked.get(ix.row).cloned() else { return };
                on_confirm
                    .update(cx, |this, cx| {
                        this.close_finder(window, cx);
                        this.open(&path, window, cx);
                    })
                    .ok();
            })
            .on_cancel(move |window, cx| {
                on_cancel.update(cx, |this, cx| this.close_finder(window, cx)).ok();
            });
        Some(
            div()
                .id("finder")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                // Not stretched to the overlay's height: the palette sizes itself.
                .items_start()
                .pt(px(64.))
                .bg(gpui_kit::black().opacity(0.25))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_finder(window, cx)))
                .child(div().occlude().w(px(600.)).max_w_full().child(command.shadow_lg()))
                .into_any_element(),
        )
    }
}

/// The `limit` paths that match `query` best: its characters in order,
/// preferring the file name, word starts and runs of consecutive letters.
pub(super) fn best_matches(query: &str, paths: &[String], limit: usize) -> Vec<String> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return paths.iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(i64, &String)> =
        paths.iter().filter_map(|path| Some((score(&query, path)?, path))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, path)| path.clone()).collect()
}

fn score(query: &[char], path: &str) -> Option<i64> {
    let chars: Vec<char> = path.chars().flat_map(char::to_lowercase).collect();
    let name_start = chars.iter().rposition(|c| *c == '/').map_or(0, |ix| ix + 1);
    // Matched backwards from the end, so letters land in the file name
    // before its directories.
    let mut positions = Vec::with_capacity(query.len());
    let mut at = chars.len();
    for q in query.iter().rev() {
        at = chars[..at].iter().rposition(|c| c == q)?;
        positions.push(at);
    }
    positions.reverse();
    let mut score = 0i64;
    for (i, &pos) in positions.iter().enumerate() {
        score += 1;
        if pos >= name_start {
            score += 4;
        }
        if pos == 0 || matches!(chars[pos - 1], '/' | '_' | '-' | '.' | ' ') {
            score += 6;
        }
        if i > 0 && positions[i - 1] + 1 == pos {
            score += 5;
        }
    }
    Some(score * 100 - chars.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::best_matches;

    #[test]
    fn prefers_the_file_name_and_word_starts() {
        let paths: Vec<String> = [
            "app/models/user.rb",
            "test/models/user_test.rb",
            "app/controllers/users_controller.rb",
            "lib/unused/stuff.rb",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(best_matches("user", &paths, 2), ["app/models/user.rb", "test/models/user_test.rb"]);
        assert_eq!(best_matches("uctrl", &paths, 1), ["app/controllers/users_controller.rb"]);
        assert_eq!(best_matches("zzz", &paths, 5), Vec::<String>::new());
        assert_eq!(best_matches("", &paths, 2).len(), 2);
    }
}
