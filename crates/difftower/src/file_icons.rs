//! Colored file icons by language, from Material Icon Theme (MIT, see
//! `assets/file-icons/LICENSE`), embedded and served as `file-icons/<name>.svg`.

use gpui_kit::*;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../assets/file-icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "astro", "c", "changelog", "clojure", "cmake", "console", "cpp", "csharp", "css", "dart",
    "database", "docker", "document", "elixir", "erlang", "file", "git", "go", "gradle", "graphql",
    "h", "haskell", "hpp", "html", "image", "java", "javascript", "json", "julia", "kotlin", "less",
    "license", "lock", "log", "lua", "makefile", "markdown", "nix", "nodejs", "ocaml", "pdf", "php",
    "powershell", "prisma", "proto", "python", "r", "react", "react_ts", "readme", "ruby", "rust",
    "sass", "scala", "settings", "svelte", "svg", "swift", "terraform", "test-js", "test-jsx",
    "test-ts", "toml", "tune", "typescript", "vim", "vue", "xml", "yaml", "zig",
);

const PREFIX: &str = "file-icons/";

/// The SVG for an asset path under `file-icons/`, for the asset source.
pub fn load(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix(PREFIX)?.strip_suffix(".svg")?;
    ICONS.iter().find(|(n, _)| *n == name).map(|(_, bytes)| *bytes)
}

/// The icon for a file, by its name.
pub fn icon(file_name: &str) -> impl IntoElement {
    img(SharedString::from(format!("{PREFIX}{}.svg", icon_name(file_name)))).size(px(16.)).flex_none()
}

fn icon_name(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "poetry.lock"
        | "gemfile.lock" | "composer.lock" | "flake.lock" | "uv.lock" => Some("lock"),
        "package.json" => Some("nodejs"),
        "dockerfile" | "containerfile" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yml"
        | "compose.yaml" | ".dockerignore" => Some("docker"),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => Some("git"),
        "makefile" | "gnumakefile" | "justfile" => Some("makefile"),
        "cmakelists.txt" => Some("cmake"),
        "gemfile" | "rakefile" => Some("ruby"),
        "build.gradle" | "settings.gradle" => Some("gradle"),
        ".env" | ".envrc" => Some("tune"),
        ".vimrc" => Some("vim"),
        _ => None,
    };
    if let Some(name) = by_name {
        return name;
    }
    if lower.starts_with(".env.") {
        return "tune";
    }
    // `README.md`, `LICENSE-MIT`, `CHANGELOG`…
    let stem = lower.split('.').next().unwrap_or_default();
    for (start, icon) in [
        ("readme", "readme"),
        ("license", "license"),
        ("licence", "license"),
        ("copying", "license"),
        ("changelog", "changelog"),
    ] {
        if stem.starts_with(start) {
            return icon;
        }
    }
    let Some((_, ext)) = lower.rsplit_once('.') else { return "file" };
    if lower.contains(".test.") || lower.contains(".spec.") {
        match ext {
            "ts" | "tsx" | "mts" | "cts" => return "test-ts",
            "jsx" => return "test-jsx",
            "js" | "mjs" | "cjs" => return "test-js",
            _ => {}
        }
    }
    match ext {
        "rs" => "rust",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "jsx" => "react",
        "tsx" => "react_ts",
        "go" => "go",
        "java" => "java",
        "c" => "c",
        "cc" | "cpp" | "cxx" | "c++" => "cpp",
        "h" => "h",
        "hh" | "hpp" | "hxx" => "hpp",
        "cs" => "csharp",
        "rb" | "erb" | "rake" | "gemspec" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "lua" => "lua",
        "zig" => "zig",
        "ex" | "exs" | "heex" => "elixir",
        "erl" | "hrl" => "erlang",
        "hs" | "lhs" => "haskell",
        "dart" => "dart",
        "r" | "rmd" => "r",
        "jl" => "julia",
        "ml" | "mli" => "ocaml",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "nix" => "nix",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "sass",
        "less" => "less",
        "json" | "jsonc" | "json5" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "xml" | "xsd" | "xsl" | "plist" => "xml",
        "md" | "mdx" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" | "fish" => "console",
        "ps1" | "psm1" => "powershell",
        "sql" | "sqlite" | "db" => "database",
        "lock" => "lock",
        "svelte" => "svelte",
        "vue" => "vue",
        "astro" => "astro",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => "image",
        "svg" => "svg",
        "pdf" => "pdf",
        "ini" | "cfg" | "conf" | "config" | "properties" => "settings",
        "env" => "tune",
        "txt" | "rtf" => "document",
        "log" => "log",
        "cmake" => "cmake",
        "tf" | "tfvars" | "hcl" => "terraform",
        "gradle" => "gradle",
        "prisma" => "prisma",
        "vim" => "vim",
        _ => "file",
    }
}

#[cfg(test)]
mod tests {
    // Not a glob import: `gpui_kit::*` brings its own `test` attribute.
    use super::{PREFIX, icon_name, load};

    #[test]
    fn picks_icons_by_name_then_extension() {
        assert_eq!(icon_name("main.rs"), "rust");
        assert_eq!(icon_name("Cargo.lock"), "lock");
        assert_eq!(icon_name("Cargo.toml"), "toml");
        assert_eq!(icon_name("README.md"), "readme");
        assert_eq!(icon_name("LICENSE-MIT"), "license");
        assert_eq!(icon_name("Dockerfile"), "docker");
        assert_eq!(icon_name(".env.local"), "tune");
        assert_eq!(icon_name("app.test.tsx"), "test-ts");
        assert_eq!(icon_name("Button.tsx"), "react_ts");
        assert_eq!(icon_name("notes"), "file");
        assert_eq!(icon_name("weird.unknown"), "file");
    }

    #[test]
    fn every_icon_name_is_embedded() {
        let extensions = [
            "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp", "cs", "rb", "php",
            "swift", "kt", "scala", "lua", "zig", "ex", "erl", "hs", "dart", "r", "jl", "ml", "clj", "nix",
            "html", "css", "scss", "less", "json", "yml", "toml", "xml", "md", "sh", "ps1", "sql", "lock",
            "svelte", "vue", "astro", "graphql", "proto", "png", "svg", "pdf", "ini", "env", "txt", "log",
            "cmake", "tf", "gradle", "prisma", "vim", "unknown",
        ];
        let names = [
            "package.json", "Dockerfile", ".gitignore", "Makefile", "CMakeLists.txt", "Gemfile", ".env",
            ".vimrc", "README", "LICENSE", "CHANGELOG.md", "a.test.ts", "a.test.jsx", "a.spec.js", "noext",
        ];
        let files = extensions.iter().map(|ext| format!("file.{ext}")).chain(names.iter().map(|n| n.to_string()));
        for name in files {
            let path = format!("{PREFIX}{}.svg", icon_name(&name));
            assert!(load(&path).is_some(), "{path} is not embedded");
        }
    }
}
