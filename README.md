# Difftower

**A control tower for coding agents.** Difftower is a fast, native diff
monitor for the way code gets written now: an agent edits, you supervise.

It is not an editor. It shows, live, everything your agents changed since
you started a session, lets you review it file by file, and guards the files
agents must not touch.

## Features

- **Live diff of your repo.** Everything that differs from HEAD, untracked
  files included, updated as the agent writes.
- **Sessions.** Start a session to accumulate every change from that moment
  on, even after the agent commits. A session survives restarts until you
  end it.
- **Review like a pull request.** Unified or split view, syntax highlighting,
  mark files as reviewed. A reviewed file that changes again goes back to
  unreviewed.
- **Protected files.** Pin files such as tests or migrations. If an agent
  modifies one, Difftower alerts you and reverts it in one click.
- **Revert any file** to HEAD, or to the start of the session.
- **Integrated terminal** to run your agent right next to its diff.

### Protected files

Protected paths live in `.difftower.toml` at the repository root, so a
team can commit and share them:

```toml
[guard]
protect = ["tests/**", "migrations/**", "Cargo.lock"]
```

The shield button on each file header adds or removes that exact path.

## How it works

The working tree, untracked files included, is written as a git tree
through a private index file (`.git/difftower/index`) and diffed against
HEAD or the session's tree, kept alive by `refs/difftower/baseline`.
Your own index, branches and stash are never touched.

## Building

Requires Rust 1.90+ and git. Linux only for now.

```sh
cargo build --release
./target/release/difftower [path/to/repo]
```

## Roadmap

- [x] Live diff against HEAD or a session, review state, protected files
- [x] Integrated terminal to run agents side by side
- [ ] Line comments sent back to the agent as prompts
- [ ] Agent hooks that block edits to protected files before they happen
- [ ] Remote folders over SSH via a tiny server-side agent
- [ ] Several agents / git worktrees at once
- [ ] macOS

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
