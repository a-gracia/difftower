// Difftower: the overview summarizes where changes sit among the rows.

use gpui::{AppContext as _, TestAppContext};

use super::{DiffFile, DiffMode, DiffState, OverviewKind};

/// A whole 20-line file with line 6 replaced and line 16 added after it.
fn full_file_patch() -> String {
    let mut body = String::new();
    for i in 1..=20 {
        match i {
            6 => body.push_str("-six\n+SIX\n"),
            16 => body.push_str(" sixteen\n+inserted\n"),
            _ => body.push_str(&format!(" line {i}\n")),
        }
    }
    format!("diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,20 +1,21 @@\n{body}")
}

#[gpui::test]
fn marks_follow_the_changed_rows(cx: &mut TestAppContext) {
    let file = DiffFile::parse(&full_file_patch()).unwrap();
    let state = cx.new(|cx| DiffState::new(file, cx).with_context_lines(None).with_mode(DiffMode::Unified));
    let overview = cx.update(|cx| state.read(cx).overview());
    let kinds: Vec<_> = overview.marks.iter().map(|m| m.kind).collect();
    // The replacement shows as a removed then an added row in unified mode.
    assert_eq!(kinds, [OverviewKind::Removed, OverviewKind::Added, OverviewKind::Added]);
    for mark in &overview.marks {
        assert!(mark.start < mark.end && mark.end <= 1.);
    }
    assert!(overview.marks[0].start > 0.2 && overview.marks[0].start < 0.35, "{:?}", overview.marks);
    assert!(overview.marks[2].start > 0.7 && overview.marks[2].start < 0.85, "{:?}", overview.marks);
}

#[gpui::test]
fn side_by_side_replacements_are_one_modified_mark(cx: &mut TestAppContext) {
    let file = DiffFile::parse(&full_file_patch()).unwrap();
    let state = cx.new(|cx| DiffState::new(file, cx).with_context_lines(None).with_mode(DiffMode::Split));
    let kinds: Vec<_> = cx.update(|cx| state.read(cx).overview().marks.iter().map(|m| m.kind).collect());
    assert_eq!(kinds, [OverviewKind::Modified, OverviewKind::Added]);
}
