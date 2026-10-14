//! UI-agnostic core of Difftower: snapshots of a working tree, its diff
//! against HEAD, file guards, approvals, and a debounced file watcher. Kept free of GUI code so the
//! remote agent can reuse it.

mod approvals;
mod guard;
mod repo;
mod watch;

pub use approvals::Approvals;
pub use guard::{Guard, dir_rule, file_rule};
pub use repo::{Change, ChangeStatus, Changes, Repo, TreeId};
pub use watch::{RepoWatcher, watch};
