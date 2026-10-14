//! A terminal panel for running agents next to their diff, built on
//! `alacritty_terminal` for parsing and the PTY, and painted with GPUI.

use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use alacritty_terminal::{
    Term,
    event::{Event as TermEvent, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions as _, Scroll},
    sync::FairMutex,
    term::{Config, TermMode, cell::Flags, test::TermSize},
    tty,
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 1.3;

/// What the terminal runs.
pub enum TerminalCommand {
    /// The user's shell in a local directory.
    Shell { cwd: PathBuf },
    /// A program, such as `ssh -t host`, that provides the shell itself.
    Program { program: String, args: Vec<String> },
}

#[derive(Clone)]
struct Listener(async_channel::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.try_send(event);
    }
}

pub struct TerminalView {
    term: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    focus: FocusHandle,
    size: (usize, usize),
    title: Option<SharedString>,
    exited: bool,
    _events: Task<()>,
}

impl TerminalView {
    pub fn new(command: TerminalCommand, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let (tx, rx) = async_channel::unbounded();
        let listener = Listener(tx);
        let size = (80, 24);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(size.0, size.1),
            listener.clone(),
        )));
        let (shell, working_directory) = match command {
            TerminalCommand::Shell { cwd } => (None, Some(cwd)),
            TerminalCommand::Program { program, args } => (Some(tty::Shell::new(program, args)), None),
        };
        let options = tty::Options {
            shell,
            working_directory,
            env: [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            ..Default::default()
        };
        let pty = tty::new(&options, window_size(size, Size::default()), 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(cx.new(|cx| Self::with_pty(term, sender, rx, window, cx)))
    }

    fn with_pty(
        term: Arc<FairMutex<Term<Listener>>>,
        pty: EventLoopSender,
        rx: async_channel::Receiver<TermEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    match event {
                        TermEvent::Wakeup => {}
                        TermEvent::Title(title) => this.title = Some(title.into()),
                        TermEvent::ResetTitle => this.title = None,
                        TermEvent::PtyWrite(text) => this.write(text.into_bytes()),
                        TermEvent::ClipboardStore(_, text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text))
                        }
                        TermEvent::ChildExit(_) | TermEvent::Exit => this.exited = true,
                        _ => return,
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        Self {
            term,
            pty,
            focus: cx.focus_handle(),
            size: (80, 24),
            title: None,
            exited: false,
            _events: events,
        }
    }

    pub fn title(&self) -> Option<SharedString> {
        self.title.clone()
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    /// Sends text as if it were pasted, so agents treat newlines as content.
    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write(format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes());
        } else {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r").into_bytes());
        }
    }

    fn resize(&mut self, cols: usize, rows: usize, cell: Size<Pixels>) {
        if (cols, rows) == self.size || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        self.term.lock().resize(TermSize::new(cols, rows));
        let _ = self.pty.send(Msg::Resize(window_size(self.size, cell)));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        if mods.control && mods.shift && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste(&text);
            }
        } else if mods.control && mods.shift && keystroke.key == "c" {
            if let Some(text) = self.term.lock().selection_to_string() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        } else {
            let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
            let Some(bytes) = key_bytes(keystroke, app_cursor) else { return };
            self.term.lock().scroll_display(Scroll::Bottom);
            self.write(bytes);
        }
        cx.stop_propagation();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = px(FONT_SIZE * LINE_HEIGHT);
        let lines = (event.delta.pixel_delta(line_height).y / line_height).round() as i32;
        if lines == 0 {
            return;
        }
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            // Full-screen programs scroll with arrow keys.
            let key = if lines > 0 { "\x1bOA" } else { "\x1bOB" };
            drop(term);
            self.write(key.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else {
            term.scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // Ends the PTY loop, which hangs up the shell or the ssh session.
        let _ = self.pty.send(Msg::Shutdown);
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = Palette { fg: theme.foreground, bg: theme.background, cursor: theme.primary };
        let font = Font { family: monospace_family(), ..Font::default() };
        let font_size = px(FONT_SIZE);
        let line_height = font_size * LINE_HEIGHT;
        let font_id = window.text_system().resolve_font(&font);
        let cell_width = window.text_system().ch_advance(font_id, font_size).unwrap_or(px(8.));
        let cell = size(cell_width, line_height);
        let focused = self.focus.is_focused(window);
        let term = self.term.clone();
        let this = cx.entity().downgrade();

        div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
            .size_full()
            .p_2()
            .bg(palette.bg)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let cols = (bounds.size.width / cell.width).floor() as usize;
                        let rows = (bounds.size.height / cell.height).floor() as usize;
                        this.update(cx, |this, _| this.resize(cols, rows, cell)).ok();
                    },
                    move |bounds, _, window, cx| {
                        paint_grid(&term.lock(), bounds.origin, cell, &font, font_size, &palette, focused, window, cx);
                    },
                )
                .size_full(),
            )
            .when(self.exited, |el| {
                el.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Process exited"))
            })
    }
}

/// The system's monospace font. GPUI does not resolve fontconfig aliases,
/// so ask fontconfig for the concrete family once.
fn monospace_family() -> SharedString {
    static FAMILY: OnceLock<SharedString> = OnceLock::new();
    FAMILY
        .get_or_init(|| {
            std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "monospace"])
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .filter(|family| !family.is_empty())
                .unwrap_or_else(|| "DejaVu Sans Mono".into())
                .into()
        })
        .clone()
}

struct Palette {
    fg: Hsla,
    bg: Hsla,
    cursor: Hsla,
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    term: &Term<Listener>,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    font: &Font,
    font_size: Pixels,
    palette: &Palette,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let rows = term.screen_lines();
    let mut lines: Vec<(String, Vec<TextRun>)> = (0..rows).map(|_| Default::default()).collect();

    for indexed in content.display_iter {
        let cell_flags = indexed.cell.flags;
        if cell_flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let row = (indexed.point.line.0 + offset) as usize;
        let Some((text, runs)) = lines.get_mut(row) else { continue };
        let mut fg = resolve(indexed.cell.fg, content.colors, palette);
        let mut bg = resolve(indexed.cell.bg, content.colors, palette);
        if cell_flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell_flags.contains(Flags::DIM) {
            fg = fg.opacity(0.6);
        }
        if bg != palette.bg {
            let x = origin.x + cell.width * indexed.point.column.0 as f32;
            let y = origin.y + cell.height * row as f32;
            let width = if cell_flags.contains(Flags::WIDE_CHAR) { cell.width * 2. } else { cell.width };
            window.paint_quad(fill(Bounds::new(point(x, y), size(width, cell.height)), bg));
        }
        let ch = if indexed.cell.c == '\0' { ' ' } else { indexed.cell.c };
        let len = ch.len_utf8();
        text.push(ch);
        let run_font = Font {
            weight: if cell_flags.contains(Flags::BOLD) { FontWeight::BOLD } else { FontWeight::NORMAL },
            style: if cell_flags.contains(Flags::ITALIC) { FontStyle::Italic } else { FontStyle::Normal },
            ..font.clone()
        };
        let underline = cell_flags
            .intersects(Flags::ALL_UNDERLINES)
            .then(|| UnderlineStyle { color: Some(fg), thickness: px(1.), wavy: false });
        match runs.last_mut() {
            Some(last) if last.color == fg && last.font == run_font && last.underline == underline => {
                last.len += len;
            }
            _ => runs.push(TextRun {
                len,
                font: run_font,
                color: fg,
                background_color: None,
                underline,
                strikethrough: None,
            }),
        }
    }

    for (row, (text, runs)) in lines.into_iter().enumerate() {
        if text.trim_end().is_empty() {
            continue;
        }
        let shaped = window.text_system().shape_line(text.into(), font_size, &runs, Some(cell.width));
        let y = origin.y + cell.height * row as f32;
        let _ = shaped.paint(point(origin.x, y), cell.height, TextAlign::Left, None, window, cx);
    }

    let cursor = content.cursor;
    if cursor.shape != CursorShape::Hidden {
        let row = cursor.point.line.0 + offset;
        if row >= 0 && (row as usize) < rows {
            let x = origin.x + cell.width * cursor.point.column.0 as f32;
            let y = origin.y + cell.height * row as f32;
            let bounds = Bounds::new(point(x, y), cell);
            if focused {
                window.paint_quad(fill(bounds, palette.cursor.opacity(0.7)));
            } else {
                window.paint_quad(outline(bounds, palette.cursor, BorderStyle::Solid));
            }
        }
    }
}

fn resolve(color: Color, overrides: &alacritty_terminal::term::color::Colors, palette: &Palette) -> Hsla {
    let index = match color {
        Color::Spec(rgb) => return to_hsla(rgb),
        Color::Indexed(ix) => ix as usize,
        Color::Named(name) => name as usize,
    };
    if let Some(rgb) = overrides[index] {
        return to_hsla(rgb);
    }
    match index {
        0..=255 => to_hsla(ansi_256(index as u8)),
        _ => match color {
            Color::Named(NamedColor::Background) => palette.bg,
            Color::Named(NamedColor::Cursor) => palette.cursor,
            Color::Named(NamedColor::DimForeground) => palette.fg.opacity(0.6),
            _ => palette.fg,
        },
    }
}

fn to_hsla(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255., g: rgb.g as f32 / 255., b: rgb.b as f32 / 255., a: 1. }.into()
}

/// The xterm 256-color palette, with a softer set of the 16 base colors.
fn ansi_256(ix: u8) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (0x1e, 0x1e, 0x2e), (0xf3, 0x8b, 0xa8), (0xa6, 0xe3, 0xa1), (0xf9, 0xe2, 0xaf),
        (0x89, 0xb4, 0xfa), (0xf5, 0xc2, 0xe7), (0x94, 0xe2, 0xd5), (0xba, 0xc2, 0xde),
        (0x58, 0x5b, 0x70), (0xf3, 0x8b, 0xa8), (0xa6, 0xe3, 0xa1), (0xf9, 0xe2, 0xaf),
        (0x89, 0xb4, 0xfa), (0xf5, 0xc2, 0xe7), (0x94, 0xe2, 0xd5), (0xa6, 0xad, 0xc8),
    ];
    let (r, g, b) = match ix {
        0..=15 => BASE[ix as usize],
        16..=231 => {
            let ix = ix - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(ix / 36), level((ix / 6) % 6), level(ix % 6))
        }
        _ => {
            let v = 8 + (ix - 232) * 10;
            (v, v, v)
        }
    };
    Rgb { r, g, b }
}

fn window_size((cols, rows): (usize, usize), cell: Size<Pixels>) -> WindowSize {
    WindowSize {
        num_cols: cols as u16,
        num_lines: rows as u16,
        cell_width: f32::from(cell.width) as u16,
        cell_height: f32::from(cell.height) as u16,
    }
}

/// Translates a keystroke into the bytes a terminal program expects.
fn key_bytes(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = keystroke.modifiers;
    let arrow = |code: char| {
        let prefix = if app_cursor && !mods.shift && !mods.control && !mods.alt { "\x1bO" } else { "\x1b[" };
        let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.control);
        if modifier > 1 {
            format!("\x1b[1;{modifier}{code}").into_bytes()
        } else {
            format!("{prefix}{code}").into_bytes()
        }
    };
    let bytes = match keystroke.key.as_str() {
        "enter" => if mods.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() },
        "backspace" => if mods.alt { b"\x1b\x7f".to_vec() } else if mods.control { b"\x08".to_vec() } else { b"\x7f".to_vec() },
        "tab" => if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() },
        "escape" => b"\x1b".to_vec(),
        "up" => arrow('A'),
        "down" => arrow('B'),
        "right" => arrow('C'),
        "left" => arrow('D'),
        "home" => arrow('H'),
        "end" => arrow('F'),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "space" if mods.control => vec![0],
        key if mods.control && key.len() == 1 => {
            let c = key.as_bytes()[0].to_ascii_lowercase();
            let byte = match c {
                b'a'..=b'z' => c - b'a' + 1,
                b'[' => 0x1b,
                b'\\' => 0x1c,
                b']' => 0x1d,
                b'/' => 0x1f,
                _ => return None,
            };
            if mods.alt { vec![0x1b, byte] } else { vec![byte] }
        }
        _ => {
            let text = keystroke.key_char.as_deref()?;
            if mods.alt {
                [b"\x1b".as_slice(), text.as_bytes()].concat()
            } else {
                text.as_bytes().to_vec()
            }
        }
    };
    Some(bytes)
}
