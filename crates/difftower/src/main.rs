use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

mod assets;
mod file_icons;
mod file_tree;
mod home;
mod open;
mod recent;
mod settings;
mod terminal;
mod workspace;

use home::Home;

fn main() {
    // `difftower [path | user@host:path]`; with no argument, the start screen.
    let location = std::env::args().nth(1);

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
