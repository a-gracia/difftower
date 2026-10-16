# Difftower

**A control tower for coding agents.** Difftower is a fast, native diff
monitor for the way code gets written now: an agent edits, you supervise.

It is not an editor. It shows, live, everything your agents changed since
you started a session, lets you review it file by file, guards the files
agents must not touch and runs your tests.

## Features

- **Live diff of your repo.** Everything that differs from HEAD, untracked
  files included, updated as the agent writes.
- **Review like a pull request.** The selected file is shown in full with
  its changes, next to a ruler marking where they are. Unified or split
  view (each side scrolls sideways on its own) and syntax highlighting.
  A change is accepted when you commit it.
- **File tree.** Browse every file in the repository, see which folders
  contain changes, and open unchanged files read-only.
- **Protected files and folders.** Protect tests, migrations or whole
  folders from the tree. The status bar turns red the moment an agent
  touches one, with a single count however many files it hit, and the
  Protected tab lists each rule with what changed under it. Revert one
  file, one rule or everything in a click; the alert stays until the
  change is reverted or committed.
- **Revert any file** to HEAD.
- **Integrated terminal** to run your agent right next to its diff.
- **Tests.** The Tests tab lists every test of every repository, by folder
  and file, and runs them all, a folder, a file or a single test. The
  runner's output shows in the center panel, and the status bar turns red
  when a test fails.

### Tests

Difftower runs pytest, vitest and Rails (minitest) tests, found from the
project's files, with the command a terminal at the repository root would
use (`.venv/bin/python -m pytest`, `node_modules/.bin/vitest`,
`bin/rails test`). To choose the runner or its command, add to
`.difftower.toml`:

```toml
[tests]
runner = "rails"   # pytest, vitest or rails
command = "PARALLEL_WORKERS=2 nice -n 19 bin/rails test"
```

### Protected files

Protected paths live in `.difftower.toml` at the repository root, so a
team can commit and share them:

```toml
[guard]
protect = ["tests/**", "migrations/**", "Cargo.lock"]
```

As in `.gitignore`, a rule starting with `!` is an exception and the last
matching rule wins:

```toml
[guard]
protect = ["tests/**", "!tests/fixtures/scratch.json"]
```

The shield next to a file or folder in the tree works like a checkbox:
a folder is fully, partly or not protected, clicking it protects or
unprotects everything inside, and unprotecting a single file inside a
protected folder adds an exception. Difftower keeps the rule list minimal
as you click. The Protected tab lists the files each rule covers.

## Your preferences

Panel widths, unified or split diffs, wrapping and whether the terminal
is shown are kept per user in `~/.config/difftower/settings.json`
(`$XDG_CONFIG_HOME` is honored), separate from the repository's
`.difftower.toml`. Drag the dividers to resize: the file list and the
terminal keep their width and the diff takes the rest.

## Keyboard

| Keys | Action |
| --- | --- |
| `Ctrl+1` | Changes |
| `Ctrl+2` | Files |
| `Ctrl+3` | Protected |
| `Ctrl+4` | Tests |
| `Alt+Z` | Wrap long lines |

## Several repositories at once

Open a directory that is not a repository but holds some, such as
`~/repos` with `api/`, `web/` and `worker/` inside, and Difftower shows
all of them in one workspace, every path under its repository's name. No
`git init` in the parent is needed. Each repository keeps its own
`.difftower.toml`, so its protections and tests also apply when it is
opened alone. Repositories are found one level down. The parent may be a
repository too, as when it keeps notes beside code repositories it
ignores: it shows alongside them.

## Remote repositories over SSH

```sh
difftower user@host:/path/to/repo
difftower myserver:~/code/app      # any host from ~/.ssh/config
```

Difftower uses your own `ssh`, so keys, `~/.ssh/config`, `ProxyJump` and
the rest work as usual. On first connection it installs a small static
agent (about 3 MB, a few MB of RAM) in `~/.cache/difftower/` on the host;
the agent watches the repository there and sends only diffs. Each
Difftower version carries its matching agent and replaces the old one on
the next connection, so there is nothing to install or update by hand.
The terminal opens a shell on the host in the repository, so agents run
there. All of it shares one SSH connection.

If the connection drops, Difftower reconnects in the background, waiting
longer between attempts (up to 30 s), and refreshes once it is back; the
terminal opens a new session too. Programs running in the old remote
session end with it, so run long-lived agents inside `tmux` on the host to
pick them up again.

Requirements on the host: Linux x86_64 and git. Authentication must not
prompt (use a key or an SSH agent).

## How it works

The working tree, untracked files included, is written as a git tree
through a private index file (`.git/difftower/index`) and diffed against
HEAD.
Your own index, branches and stash are never touched.

## Credits

File icons come from [Material Icon Theme](https://github.com/material-extensions/vscode-material-icon-theme)
(MIT), see `crates/difftower/assets/file-icons/LICENSE`.

## Building

Requires Rust 1.90+ and git. Linux only for now.

```sh
cargo dt                     # start screen with recent repositories
cargo dt path/to/repo        # open a local repository
cargo dt user@host:/repo     # open one over SSH
```

`cargo dt` builds the `profiling` profile: as fast as release at runtime,
but incremental, so a change rebuilds in seconds. `cargo build --release`
adds LTO for the shipped binary and takes minutes after any change; use it
only to package (`./target/release/difftower`).

Without arguments Difftower shows a start screen: type a path or
`user@host:/path`, choose a folder, or reopen a recent repository. The icon
at the top left of a workspace goes back to it.

## Roadmap

- [x] Live diff against HEAD, protected files
- [x] Run tests by repository, folder, file or one at a time
- [x] Integrated terminal to run agents side by side
- [ ] Line comments sent back to the agent as prompts
- [ ] Agent hooks that block edits to protected files before they happen
- [x] Remote folders over SSH via a tiny server-side agent
- [ ] Several agents / git worktrees at once
- [ ] macOS

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
