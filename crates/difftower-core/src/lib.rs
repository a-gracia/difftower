//! UI-agnostic core of Difftower: baseline snapshots of a working tree, the
//! diff of everything that changed since then, file guards, and a debounced
//! file watcher. Kept free of GUI code so the remote agent can reuse it.

mod guard;
mod repo;
mod watch;

pub use guard::Guard;
pub use repo::{Change, ChangeStatus, Changes, Repo, TreeId};
pub use watch::{RepoWatcher, watch};
