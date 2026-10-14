//! UI-agnostic core of Difftower: snapshots of a working tree, its diff
//! against HEAD, file guards, approvals and a debounced file watcher, behind
//! a [`Backend`] that is either local or a remote agent reached over SSH.

mod approvals;
mod backend;
mod guard;
pub mod protocol;
mod remote;
mod repo;
mod watch;

pub use approvals::Approvals;
pub use backend::{Backend, Local, Status};
pub use guard::{Guard, dir_rule, file_rule, load_rules, save_rules};
pub use remote::{Remote, SshTarget};
pub use repo::{Change, ChangeStatus, Changes, Repo, TreeId};
pub use watch::{RepoWatcher, watch};
