//! The repository's files as a collapsible tree, flattened into the rows
//! that are currently visible.

use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<String>,
}

#[derive(Default)]
pub struct FileTree {
    root: Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Path relative to the repository root.
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

impl FileTree {
    pub fn new<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut root = Dir::default();
        for path in paths {
            let mut parts: Vec<&str> = path.split('/').collect();
            let Some(file) = parts.pop() else { continue };
            let dir = parts
                .into_iter()
                .fold(&mut root, |dir, part| dir.dirs.entry(part.to_owned()).or_default());
            dir.files.push(file.to_owned());
        }
        Self { root }
    }

    /// Visible rows: directories first, then files, both sorted by name,
    /// descending only into `expanded` directories.
    pub fn rows(&self, expanded: &HashSet<String>) -> Vec<Row> {
        let mut rows = Vec::new();
        push_rows(&self.root, "", 0, expanded, &mut rows);
        rows
    }
}

fn push_rows(dir: &Dir, prefix: &str, depth: usize, expanded: &HashSet<String>, rows: &mut Vec<Row>) {
    for (name, child) in &dir.dirs {
        let path = format!("{prefix}{name}");
        let is_expanded = expanded.contains(&path);
        rows.push(Row { path: path.clone(), name: name.clone(), depth, is_dir: true, expanded: is_expanded });
        if is_expanded {
            push_rows(child, &format!("{path}/"), depth + 1, expanded, rows);
        }
    }
    let mut files: Vec<&String> = dir.files.iter().collect();
    files.sort();
    for name in files {
        rows.push(Row {
            path: format!("{prefix}{name}"),
            name: name.clone(),
            depth,
            is_dir: false,
            expanded: false,
        });
    }
}

/// Every ancestor directory of `path`, nearest last: `a/b/c.rs` gives `a`, `a/b`.
pub fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(ix, _)| &path[..ix])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect()
    }

    #[test]
    fn dirs_first_and_collapsed_by_default() {
        let tree = FileTree::new(["src/main.rs", "README.md", "src/ui/view.rs", "Cargo.toml"]);
        assert_eq!(names(&tree.rows(&HashSet::new())), ["src", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn expands_only_open_dirs() {
        let tree = FileTree::new(["src/main.rs", "src/ui/view.rs", "src/ui/theme.rs"]);
        let expanded = HashSet::from(["src".to_owned()]);
        assert_eq!(names(&tree.rows(&expanded)), ["src", "  ui", "  main.rs"]);

        let expanded = HashSet::from(["src".to_owned(), "src/ui".to_owned()]);
        let rows = tree.rows(&expanded);
        assert_eq!(names(&rows), ["src", "  ui", "    theme.rs", "    view.rs", "  main.rs"]);
        assert_eq!(rows[2].path, "src/ui/theme.rs");
    }

    #[test]
    fn ancestors_of_a_path() {
        assert_eq!(ancestors("a/b/c.rs").collect::<Vec<_>>(), ["a", "a/b"]);
        assert_eq!(ancestors("c.rs").count(), 0);
    }
}
