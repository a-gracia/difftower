// Difftower: the line numbers and change sign of a code row as one element.
//
// Upstream builds them from nested flex `div`s with measured text, which made
// the gutter alone about 40% of a frame. This element has a fixed size, so
// layout never measures it, and paints its few glyphs directly.

use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, Entity, Font, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, SharedString, Size, Style, TextAlign, TextRun, Window, fill, point, px, size,
};

use super::{DiffLinePosition, state::DiffState};

/// What sits after the line numbers.
pub(crate) enum Indicator {
    /// `+`, `−` or a blank, in a column one rem wide.
    Sign { text: &'static str, color: Hsla },
    /// A thin colored bar, in a column one rem wide.
    Bar(Option<Hsla>),
    /// Half a rem of space.
    Space,
}

pub(crate) struct LineGutter {
    /// One entry per line-number column; `None` leaves the column empty.
    pub slots: Vec<Option<DiffLinePosition>>,
    pub slot_width: Pixels,
    pub indicator: Indicator,
    pub row_height: Pixels,
    pub font_size: Pixels,
    pub font: Font,
    pub number_color: Hsla,
    pub state: Entity<DiffState>,
}

impl LineGutter {
    fn indicator_width(&self, rem: Pixels) -> Pixels {
        match self.indicator {
            Indicator::Sign { .. } | Indicator::Bar(_) => rem,
            Indicator::Space => rem / 2.,
        }
    }

    fn shape(&self, text: SharedString, color: Hsla, window: &Window) -> gpui::ShapedLine {
        let run = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(text, self.font_size, &[run], None)
    }
}

impl IntoElement for LineGutter {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for LineGutter {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let width = self.slot_width * self.slots.len() as f32 + self.indicator_width(window.rem_size());
        let mut style = Style::default();
        style.size = size(width.into(), self.row_height.into());
        style.flex_shrink = 0.;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Numbers right-aligned, two quarters of a rem from the column edge,
        // as upstream's `pr_1` gutter with a `px_1` hit target placed them.
        let inset = window.rem_size() / 2.;
        for (column, slot) in self.slots.iter().enumerate() {
            let Some(position) = slot else { continue };
            let line = self.shape(position.line().to_string().into(), self.number_color, window);
            let right = bounds.origin.x + self.slot_width * (column + 1) as f32 - inset;
            let origin = point(right - line.width, bounds.origin.y);
            let _ = line.paint(origin, self.row_height, TextAlign::Left, None, window, cx);
        }

        let indicator_x = bounds.origin.x + self.slot_width * self.slots.len() as f32;
        match &self.indicator {
            Indicator::Sign { text, color } => {
                let line = self.shape((*text).into(), *color, window);
                let _ = line.paint(point(indicator_x, bounds.origin.y), self.row_height, TextAlign::Left, None, window, cx);
            }
            Indicator::Bar(Some(color)) => {
                window.paint_quad(fill(
                    Bounds::new(point(indicator_x, bounds.origin.y), size(px(3.), self.row_height)),
                    *color,
                ));
            }
            Indicator::Bar(None) | Indicator::Space => {}
        }

        if self.slots.iter().all(Option::is_none) {
            return;
        }
        let numbers = Bounds::new(bounds.origin, Size::new(self.slot_width * self.slots.len() as f32, bounds.size.height));
        if numbers.contains(&window.mouse_position()) {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        // Clicking a number selects its line; dragging over others extends it.
        let slot_at = {
            let slots = self.slots.clone();
            let (left, width) = (bounds.origin.x, self.slot_width);
            move |x: Pixels| -> Option<DiffLinePosition> {
                let column = ((x - left) / width).floor();
                (column >= 0.).then(|| slots.get(column as usize).cloned().flatten()).flatten()
            }
        };
        let (down_hitbox, move_hitbox) = (hitbox.clone(), hitbox.clone());
        let (down_state, move_state) = (self.state.clone(), self.state.clone());
        let down_slot = slot_at.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && down_hitbox.is_hovered(window)
                && let Some(position) = down_slot(event.position.x)
            {
                down_state.update(cx, |state, cx| {
                    state.begin_line_selection(position, event.modifiers.shift, window, cx)
                });
            }
        });
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.pressed_button == Some(MouseButton::Left)
                && move_hitbox.is_hovered(window)
                && let Some(position) = slot_at(event.position.x)
            {
                move_state.update(cx, |state, cx| state.drag_line_selection(position, cx));
            }
        });
    }
}
