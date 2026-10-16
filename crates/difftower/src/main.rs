use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

mod assets;
mod file_icons;
mod file_tree;
mod home;
mod open;
mod profile;
mod recent;
mod settings;
mod terminal;
mod workspace;

use home::Home;

const USAGE: &str = "\
Difftower: a control tower for coding agents.

Usage:
  difftower                     start screen with recent repositories
  difftower <path>              open a repository, or a directory of them
  difftower <user@host:path>    open one over SSH

Options:
  -h, --help       show this help
  -V, --version    show the version
";

fn main() {
    // `difftower [path | user@host:path]`; with no argument, the start screen.
    let location = std::env::args().nth(1);
    match location.as_deref() {
        Some("-V" | "--version") => {
            println!("difftower {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return;
        }
        Some(flag) if flag.starts_with('-') => {
            eprint!("difftower: unknown option {flag}\n\n{USAGE}");
            std::process::exit(2);
        }
        _ => {}
    }

    gpui_kit::application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        workspace::bind_keys(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("Difftower".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
            app_id: Some("difftower".into()),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Home::new(location, window, cx)))
            .expect("failed to open the window");
    });
}
