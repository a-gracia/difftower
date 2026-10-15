# Difftower's copy of gpui-component 0.7.3

Upstream: https://github.com/longbridge/gpui-kit (Apache-2.0, see LICENSE-APACHE).

Changes, all in `src/diff/`, marked with `// Difftower:` comments:

- Five tests outside `src/diff` include files from the upstream repository that
  the published crate lacks; they read empty content instead.
- Split view without soft wrap fits both sides in the visible width, and
  each side scrolls horizontally on its own, under the pointer, while the
  line numbers stay in place. Upstream scrolls both columns together
  across a list as wide as the longest line.

- `DiffState::overview` and `scroll_to_fraction` summarize where changes
  and the visible rows sit, for an overview ruler beside the diff.
- `DiffState::replace_files` swaps in new contents of the same files
  without jumping back to the top.

- `perf_tests.rs` has an ignored benchmark of a frame with a whole file
  in the diff: `cargo test --release --lib frame_time -- --ignored --nocapture`.

Drop this copy once upstream covers these.
