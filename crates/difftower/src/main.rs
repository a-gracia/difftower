use std::path::PathBuf;

use anyhow::Context as _;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

mod assets;
mod file_tree;
mod terminal;
mod workspace;

use workspace::Workspace;

fn main() -> anyhow::Result<()> {
    let path = match std::env::args_os().nth(1) {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    let repo = difftower_core::Repo::discover(&path)?;
    let title = repo
        .root()
        .file_name()
        .map(|name| format!("{} — Difftower", name.to_string_lossy()))
        .unwrap_or_else(|| "Difftower".into());

    gpui_kit::application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        workspace::bind_keys(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some(title.into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
            app_id: Some("difftower".into()),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(repo, window, cx))
        })
        .context("failed to open the window")
        .unwrap();
    });
    Ok(())
}
