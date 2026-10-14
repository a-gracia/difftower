# Difftower

**A control tower for coding agents.** Difftower is a fast, native diff
monitor for the way code gets written now: an agent edits, you supervise.

It is not an editor. It shows, live, everything your agents changed since
you started a session, lets you review it file by file, and guards the files
agents must not touch.

## Features

- **Live diff of your repo.** Everything that differs from HEAD, untracked
  files included, updated as the agent writes.
- **Review like a pull request.** Unified or split view, syntax highlighting,
  mark files as reviewed. A reviewed file that changes again goes back to
  unreviewed.
- **File tree.** Browse every file in the repository, see which folders
  contain changes, and open unchanged files read-only.
- **Protected files and folders.** Protect tests, migrations or whole
  folders from the tree. The status bar turns red the moment an agent
  touches one, with a single count however many files it hit, and the
  Protected tab lists each rule with what changed under it. Revert one
  file, one rule or everything in a click — or accept the change when it
  was intended. An acceptance covers that exact change: if the agent edits
  the file again, the alert comes back.
- **Revert any file** to HEAD.
- **Integrated terminal** to run your agent right next to its diff.

### Protected files

Protected paths live in `.difftower.toml` at the repository root, so a
team can commit and share them:

```toml
[guard]
protect = ["tests/**", "migrations/**", "Cargo.lock"]
```

The shield next to a file or folder in the tree, or on a diff header,
adds or removes the rule for it (`path` for a file, `path/**` for a folder).

## Keyboard

| Keys | Action |
| --- | --- |
| `Ctrl+1` | Changes |
| `Ctrl+2` | Files |
| `Ctrl+3` | Protected |

## How it works

The working tree, untracked files included, is written as a git tree
through a private index file (`.git/difftower/index`) and diffed against
HEAD.
Your own index, branches and stash are never touched.

## Building

Requires Rust 1.90+ and git. Linux only for now.

```sh
cargo build --release
./target/release/difftower [path/to/repo]
```

## Roadmap

- [x] Live diff against HEAD, review state, protected files
- [x] Integrated terminal to run agents side by side
- [ ] Line comments sent back to the agent as prompts
- [ ] Agent hooks that block edits to protected files before they happen
- [ ] Remote folders over SSH via a tiny server-side agent
- [ ] Several agents / git worktrees at once
- [ ] macOS

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
