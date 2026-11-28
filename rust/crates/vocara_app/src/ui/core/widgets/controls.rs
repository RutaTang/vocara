//! The shared desktop controls of `rust/DESIGN.md` (Components): small,
//! precise, neutral controls in one scale, for every screen to build on.
//!
//! | Control | Builder | Size |
//! |---|---|---|
//! | Icon button | [`icon_button`] | 24 (panels) or 28 (toolbar) square |
//! | Toolbar group | [`ToolbarGroup`], [`group_button`], [`group_toggle`] | 30 tall, 34 wide buttons, 1 × 16 dividers |
//! | Ghost button | [`ghost_button`] | 22 tall |
//! | Pop-up button | [`popup_button`] | 22 tall, an up-down chevron |
//! | Primary / secondary button (dialogs) | [`primary_button`], [`secondary_button`] | 28 tall |
//! | Segmented control | [`segmented`] | 22 (20 in a lane bar) |
//! | Tab strip | [`tab_strip`], [`Tab`] | 20 / 22 / 24 by [`TabSize`] |
//! | Thin slider | [`thin_slider`] | 12 tall row, 2 px track, 10 or 8 px knob |
//! | Parameter row | [`parameter_row`] | 22 tall: 62 px label, control, 54 px value |
//! | Power button | [`power_button`] | 18 px circle |
//! | Switch (a setting on or off) | [`toggle_switch`] | 28 × 16 |
//! | M / S / R | [`msr_button`] | 22 × 18 |
//! | Section label | `cap_label` (sentence case, 11 px semibold) | |
//! | Fader and meters (mixer) | [`draw_fader`], [`draw_meter`] | 22 px column, 4 px meters |
//!
//! Every button here is an iced `button` whose style sets its fill and its
//! text colour by status; its label and icon take that colour (they set
//! none of their own, [`line_icon_inherit`]), so the hover reaches both at
//! once. Outlines are iced borders, which iced draws inside the box: the
//! inset 1 px outline `DESIGN.md` asks for, taking no room. Icon-only
//! buttons take a tooltip ([`IconButton::tooltip`]); iced has no
//! accessibility tree, so the tooltip is the label a pointer user reads.
//!
//! Behaviour (what a press does, menus, drags) stays with the callers: the
//! pop-up button only looks like one; the menu it opens is its caller's.

use std::borrow::Cow;

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, gradient, Frame, Geometry, Gradient, Path, Stroke};
use iced::widget::{button, canvas as canvas_widget, container, row, stack, Space};
use iced::{
    mouse, Background, Border, Color, Element, Length, Padding, Point, Rectangle, Renderer, Shadow,
    Size, Theme, Vector,
};

use super::line_icon::{glyphs, line_icon_inherit, line_icon_stroked, Glyph};
use super::material_tooltip::material_tooltip;
use super::studio_text::studio_text;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, font, height, radius, type_scale, Studio};

/// What an icon button shows: a line glyph (the design's own set) or a
/// glyph of the Lucide font.
#[derive(Debug, Clone, Copy)]
pub enum IconSource {
    Line(Glyph),
    Font(Icon),
}

impl From<Glyph> for IconSource {
    fn from(glyph: Glyph) -> Self {
        IconSource::Line(glyph)
    }
}

impl From<Icon> for IconSource {
    fn from(icon: Icon) -> Self {
        IconSource::Font(icon)
    }
}

/// `source` in a `size` box, in its parent's text colour.
fn inherit_icon<'a, M: 'a>(source: IconSource, size: f32) -> Element<'a, M> {
    match source {
        IconSource::Line(glyph) => line_icon_inherit(glyph, size).into(),
        IconSource::Font(icon) => icons::icon(icon, size, Color::TRANSPARENT)
            .style(|_| iced::widget::text::Style { color: None })
            .into(),
    }
}

/// A button style from its status: `(fill, text colour)` for it, its
/// radius, and an optional inset outline.
fn styled(fill: Option<Color>, text: Color, radius: f32, outline: Option<Color>) -> button::Style {
    button::Style {
        background: fill.map(Background::Color),
        text_color: text,
        border: Border {
            color: outline.unwrap_or(Color::TRANSPARENT),
            width: if outline.is_some() { 1.0 } else { 0.0 },
            radius: Radius::from(radius),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn is_hovered(status: button::Status) -> bool {
    matches!(status, button::Status::Hovered | button::Status::Pressed)
}

// -------------------------------------------------------------- icon button

/// An icon button's size (`DESIGN.md`: 24 in panels, 28 in the toolbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconButtonSize {
    /// 24 square, radius 5, a 14 px icon.
    Panel,
    /// 28 square, radius 6, a 16 px icon.
    #[default]
    Toolbar,
}

impl IconButtonSize {
    /// The button's side.
    pub fn side(self) -> f32 {
        match self {
            IconButtonSize::Panel => height::PANEL_BUTTON,
            IconButtonSize::Toolbar => height::TOOLBAR_BUTTON,
        }
    }

    /// Its corner radius.
    pub fn radius(self) -> f32 {
        match self {
            IconButtonSize::Panel => radius::CONTROL,
            IconButtonSize::Toolbar => radius::WELL,
        }
    }

    /// Its icon box.
    pub fn icon(self) -> f32 {
        match self {
            IconButtonSize::Panel => super::line_icon::PANEL_SIZE,
            IconButtonSize::Toolbar => super::line_icon::TOOLBAR_SIZE,
        }
    }
}

/// An icon-only button (`DESIGN.md`, Icon button): transparent, the icon
/// in `TEXT_SECONDARY`; under the pointer a `HOVER` fill and the icon in
/// `TEXT_PRIMARY`; active (a toggle that is on) `TEXT_PRIMARY`, or
/// `ACCENT_BRIGHT` where it marks an enabled effect; disabled
/// `TEXT_DISABLED`. Build with [`icon_button`].
pub struct IconButton<'a, M> {
    icon: IconSource,
    on_press: Option<M>,
    size: IconButtonSize,
    icon_size: Option<f32>,
    tooltip: Option<Cow<'a, str>>,
    active: bool,
    accent: bool,
}

/// A 28 px toolbar icon button showing `icon`, pressing `on_press`.
pub fn icon_button<'a, M>(icon: impl Into<IconSource>, on_press: M) -> IconButton<'a, M> {
    IconButton {
        icon: icon.into(),
        on_press: Some(on_press),
        size: IconButtonSize::Toolbar,
        icon_size: None,
        tooltip: None,
        active: false,
        accent: false,
    }
}

impl<'a, M> IconButton<'a, M> {
    /// The button's size.
    pub fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    /// A 24 px panel button.
    pub fn panel(self) -> Self {
        self.size(IconButtonSize::Panel)
    }

    /// The icon box, when not the size's own (15 in a dense toolbar).
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    /// The tooltip naming what it does (every icon-only button has one).
    pub fn tooltip(mut self, tip: impl Into<Cow<'a, str>>) -> Self {
        self.tooltip = Some(tip.into());
        self
    }

    /// Shown as on (a toggle): the icon in `TEXT_PRIMARY` on a
    /// [`TOGGLE_ON`] fill, under the pointer too ([`icon_button_fill`]).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// When active, the icon in `ACCENT_BRIGHT` (an enabled effect).
    pub fn accent(mut self, accent: bool) -> Self {
        self.accent = accent;
        self
    }

    /// Enabled or not: a disabled button takes no press.
    pub fn enabled(mut self, enabled: bool) -> Self {
        if !enabled {
            self.on_press = None;
        }
        self
    }
}

/// The fill behind a toggle that is on (`DESIGN.md`, Toggle): a selected
/// segment's, as a selected tab or segment shows it. `CONTROL_ACTIVE`, a
/// selected tab's on a panel, would hardly show on a toolbar group's
/// `SURFACE_RAISED` or next to the `HOVER` fill.
pub const TOGGLE_ON: Color = Studio::SEGMENT_SELECTED;

/// The fill of an icon button: [`TOGGLE_ON`] while it is on — under the
/// pointer too, as a selected tab keeps its fill, so a click on it shows at
/// once — else `HOVER` under the pointer.
pub fn icon_button_fill(status: button::Status, active: bool) -> Option<Color> {
    match status {
        _ if active => Some(TOGGLE_ON),
        button::Status::Disabled => None,
        _ if is_hovered(status) => Some(Studio::HOVER),
        _ => None,
    }
}

/// The icon colour of an icon button.
pub fn icon_button_color(status: button::Status, active: bool, accent: bool) -> Color {
    match status {
        button::Status::Disabled => Studio::TEXT_DISABLED,
        _ if active && accent => Studio::ACCENT_BRIGHT,
        _ if active || is_hovered(status) => Studio::TEXT_PRIMARY,
        _ => Studio::TEXT_SECONDARY,
    }
}

impl<'a, M: Clone + 'a> From<IconButton<'a, M>> for Element<'a, M> {
    fn from(b: IconButton<'a, M>) -> Self {
        let side = b.size.side();
        let corner = b.size.radius();
        let (active, accent) = (b.active, b.accent);
        let face = container(inherit_icon(b.icon, b.icon_size.unwrap_or(b.size.icon())))
            .center_x(side)
            .center_y(side);
        let target = button(face)
            .padding(0)
            .width(side)
            .height(side)
            .on_press_maybe(b.on_press)
            .style(move |_, status| {
                styled(
                    icon_button_fill(status, active),
                    icon_button_color(status, active, accent),
                    corner,
                    None,
                )
            });
        match b.tooltip {
            // Below the button, its top 6 px clear of it; the tip is also
            // its label for the widget tree.
            Some(tip) => {
                let tip = tip.into_owned();
                labelled(material_tooltip(target, tip.clone(), 6.0), tip)
            }
            None => target.into(),
        }
    }
}

/// `content` labelled `label` for the widget tree: it reports the label to
/// operations as the text over its bounds (what a test's `find` and
/// `click` read), as an icon-only button's accessibility label would name
/// it. It lays out, draws and takes input as `content` does.
pub fn labelled<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    label: impl Into<String>,
) -> Element<'a, M> {
    Element::new(Labelled {
        content: content.into(),
        label: label.into(),
    })
}

struct Labelled<'a, M> {
    content: Element<'a, M>,
    label: String,
}

impl<M> iced::advanced::Widget<M, Theme, Renderer> for Labelled<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        operation.text(None, layout.bounds(), &self.label);
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

// ------------------------------------------------------------ toolbar group

/// The width of a button in a toolbar group.
pub const GROUP_BUTTON_WIDTH: f32 = 34.0;

/// A toolbar group (`DESIGN.md`: the transport, recording): 30 px tall,
/// radius 7, `SURFACE_RAISED` with an inset `BORDER_STRONG` outline, its
/// buttons 34 wide, subgroups split by 1 × 16 `GRID_STRONG` dividers.
pub struct ToolbarGroup<'a, M> {
    items: Vec<Element<'a, M>>,
}

impl<'a, M: 'a> ToolbarGroup<'a, M> {
    pub fn new() -> Self {
        ToolbarGroup { items: Vec::new() }
    }

    /// Adds a control (a [`group_button`], or any 30 px tall element).
    pub fn push(mut self, item: impl Into<Element<'a, M>>) -> Self {
        self.items.push(item.into());
        self
    }

    /// Adds a divider between two subgroups.
    pub fn divider(mut self) -> Self {
        self.items.push(group_divider());
        self
    }
}

impl<'a, M: 'a> Default for ToolbarGroup<'a, M> {
    fn default() -> Self {
        Self::new()
    }
}

/// A 1 × 16 `GRID_STRONG` divider, centred in a 30 px row.
pub fn group_divider<'a, M: 'a>() -> Element<'a, M> {
    container(
        container(Space::new())
            .width(1)
            .height(16)
            .style(|_| crate::ui::core::theme::boxed(Some(Studio::GRID_STRONG), 0.0, None)),
    )
    .center_y(height::TOOLBAR_GROUP)
    .into()
}

impl<'a, M: 'a> From<ToolbarGroup<'a, M>> for Element<'a, M> {
    fn from(group: ToolbarGroup<'a, M>) -> Self {
        container(row(group.items).align_y(Vertical::Center))
            .height(height::TOOLBAR_GROUP)
            .style(|_| {
                crate::ui::core::theme::boxed(
                    Some(Studio::SURFACE_RAISED),
                    radius::GROUP,
                    Some(Studio::BORDER_STRONG),
                )
            })
            .into()
    }
}

/// A button inside a [`ToolbarGroup`]: 34 × 30, transparent, a 15 px icon
/// in `TEXT_SECONDARY` (`TEXT_PRIMARY` under the pointer or when
/// `active`), with a tooltip.
pub fn group_button<'a, M: Clone + 'a>(
    icon: impl Into<IconSource>,
    tip: impl Into<String>,
    on_press: Option<M>,
    active: bool,
) -> Element<'a, M> {
    let icon = icon.into();
    let face = container(inherit_icon(icon, super::line_icon::DENSE_SIZE))
        .center_x(GROUP_BUTTON_WIDTH)
        .center_y(height::TOOLBAR_GROUP);
    let target = button(face)
        .padding(0)
        .on_press_maybe(on_press)
        .style(move |_, status| styled(None, icon_button_color(status, active, false), 0.0, None));
    let tip: String = tip.into();
    labelled(material_tooltip(target, tip.clone(), 6.0), tip)
}

/// How far a [`group_toggle`]'s fill sits in from the button's edges: clear
/// of the group's rounded corners, as a segment sits in its control.
pub const TOGGLE_INSET: f32 = 3.0;

/// A [`group_button`] that stays on or off (Loop, Follow playhead, Input
/// monitoring). On, its icon in `TEXT_PRIMARY` sits on a [`TOGGLE_ON`] fill
/// [`TOGGLE_INSET`] in from its edges, radius 4 — under the pointer too, so
/// a click on it shows at once (the icon's colour alone did not change
/// while the pointer was over it). Off, it is a plain group button.
pub fn group_toggle<'a, M: Clone + 'a>(
    icon: impl Into<IconSource>,
    tip: impl Into<String>,
    on_press: Option<M>,
    on: bool,
) -> Element<'a, M> {
    let toggle = group_button(icon, tip, on_press, on);
    if !on {
        return toggle;
    }
    let fill = container(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| crate::ui::core::theme::boxed(Some(TOGGLE_ON), radius::SMALL, None)),
    )
    .padding(TOGGLE_INSET)
    .width(GROUP_BUTTON_WIDTH)
    .height(height::TOOLBAR_GROUP);
    stack![fill, toggle].into()
}

// ------------------------------------------------------- text buttons

/// A ghost button (`DESIGN.md`: "New take", "New bus"): 22 px, radius 5,
/// transparent with an inset `BORDER_STRONG` outline, 11 px
/// `TEXT_PRIMARY` text after an optional 10 px glyph; a `HOVER` fill under
/// the pointer.
pub fn ghost_button<'a, M: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    glyph: Option<Glyph>,
    on_press: Option<M>,
) -> Element<'a, M> {
    let mut content = row![].spacing(5).align_y(Vertical::Center);
    if let Some(glyph) = glyph {
        content = content.push(line_icon_inherit(glyph, 10.0));
    }
    content = content.push(label_text(label, type_scale::LABEL));
    button(container(content).center_y(Length::Fill))
        .padding(Padding::from([0, 8]))
        .height(height::CONTROL)
        .on_press_maybe(on_press)
        .style(|_, status| {
            let disabled = status == button::Status::Disabled;
            styled(
                (is_hovered(status) && !disabled).then_some(Studio::HOVER),
                if disabled {
                    Studio::TEXT_DISABLED
                } else {
                    Studio::TEXT_PRIMARY
                },
                radius::CONTROL,
                Some(Studio::BORDER_STRONG),
            )
        })
        .into()
}

/// A one-line label in the parent's text colour.
fn label_text<'a, M: 'a>(label: impl Into<Cow<'a, str>>, size: f32) -> Element<'a, M> {
    studio_text(label.into())
        .size(size)
        .line_height(iced::Pixels(type_scale::LINE))
        .ellipsis(1)
        .into()
}

/// A pop-up button's look (`DESIGN.md`: presets, interpretation, output,
/// send): 22 px, radius 5, `SURFACE_RAISED` with an inset `BORDER_STRONG`
/// outline, 11 px text, an up-down chevron in `TEXT_FAINT` on the right.
/// Build with [`popup_button`]; the menu it opens is the caller's.
pub struct PopupButton<'a, M> {
    label: Cow<'a, str>,
    on_press: Option<M>,
    width: Length,
    placeholder: bool,
    max_width: Option<f32>,
}

/// A pop-up button reading `label`, pressing `on_press` (`None`:
/// disabled), as wide as its label.
pub fn popup_button<'a, M>(
    label: impl Into<Cow<'a, str>>,
    on_press: Option<M>,
) -> PopupButton<'a, M> {
    PopupButton {
        label: label.into(),
        on_press,
        width: Length::Shrink,
        placeholder: false,
        max_width: None,
    }
}

impl<M> PopupButton<'_, M> {
    /// Its width (the chevron sits at its right end).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// The label is a placeholder ("None"): `TEXT_SECONDARY`.
    pub fn placeholder(mut self, placeholder: bool) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// At most `width` wide, however long its label: a longer label is
    /// ellipsized and the chevron keeps its place (a user-named item in a
    /// narrow pane).
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }
}

/// What a pop-up button's label leaves of its width: the padding (8 left,
/// 6 right), the 8 px gap and the 10 px chevron.
pub const POPUP_CHROME_WIDTH: f32 = 8.0 + 6.0 + 8.0 + 10.0;

impl<'a, M: Clone + 'a> From<PopupButton<'a, M>> for Element<'a, M> {
    fn from(b: PopupButton<'a, M>) -> Self {
        let placeholder = b.placeholder;
        let mut label = container(label_text(b.label, type_scale::LABEL)).width(
            if matches!(b.width, Length::Shrink) {
                Length::Shrink
            } else {
                Length::Fill
            },
        );
        if let Some(max) = b.max_width {
            label = label.max_width((max - POPUP_CHROME_WIDTH).max(0.0));
        }
        let content = row![
            label,
            line_icon_stroked(glyphs::UP_DOWN, 10.0, Studio::TEXT_FAINT, 1.0)
        ]
        .spacing(8)
        .align_y(Vertical::Center);
        button(container(content).center_y(Length::Fill))
            .padding(Padding {
                top: 0.0,
                right: 6.0,
                bottom: 0.0,
                left: 8.0,
            })
            .width(b.width)
            .height(height::CONTROL)
            .on_press_maybe(b.on_press)
            .style(move |_, status| {
                let disabled = status == button::Status::Disabled;
                let text = if disabled {
                    Studio::TEXT_DISABLED
                } else if placeholder {
                    Studio::TEXT_SECONDARY
                } else {
                    Studio::TEXT_PRIMARY
                };
                styled(
                    Some(if is_hovered(status) && !disabled {
                        Studio::CONTROL_ACTIVE
                    } else {
                        Studio::SURFACE_RAISED
                    }),
                    text,
                    radius::CONTROL,
                    Some(Studio::BORDER_STRONG),
                )
            })
            .into()
    }
}

/// A dialog's primary action (`DESIGN.md`: at most one per screen): 28 px,
/// radius 6, filled `ACCENT` (`ACCENT_BRIGHT` under the pointer), 12 px
/// semibold `ON_ACCENT` text; disabled, a `CONTROL` fill and
/// `TEXT_DISABLED` text.
pub fn primary_button<'a, M: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    on_press: Option<M>,
) -> Element<'a, M> {
    dialog_button(label, on_press, DialogButton::Primary)
}

/// A dialog's secondary action: 28 px, radius 6, transparent with an inset
/// `BORDER_STRONG` outline (a `HOVER` fill under the pointer), 12 px
/// medium `TEXT_PRIMARY` text.
pub fn secondary_button<'a, M: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    on_press: Option<M>,
) -> Element<'a, M> {
    dialog_button(label, on_press, DialogButton::Secondary)
}

/// A destructive primary action (a delete's confirm): [`primary_button`]
/// filled with `RECORD`.
pub fn destructive_button<'a, M: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    on_press: Option<M>,
) -> Element<'a, M> {
    dialog_button(label, on_press, DialogButton::Destructive)
}

/// The three dialog buttons' looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogButton {
    Primary,
    Secondary,
    Destructive,
}

/// A dialog button's `(fill, text, outline)` at `status`.
pub fn dialog_button_colors(
    kind: DialogButton,
    status: button::Status,
) -> (Option<Color>, Color, Option<Color>) {
    let hovered = is_hovered(status);
    if status == button::Status::Disabled {
        return match kind {
            DialogButton::Secondary => (None, Studio::TEXT_DISABLED, Some(Studio::BORDER_STRONG)),
            _ => (Some(Studio::CONTROL), Studio::TEXT_DISABLED, None),
        };
    }
    match kind {
        DialogButton::Primary => (
            Some(if hovered {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::ACCENT
            }),
            Studio::ON_ACCENT,
            None,
        ),
        DialogButton::Destructive => (
            Some(if hovered {
                Studio::RECORD_TEXT
            } else {
                Studio::RECORD
            }),
            Studio::ON_ACCENT,
            None,
        ),
        DialogButton::Secondary => (
            hovered.then_some(Studio::HOVER),
            Studio::TEXT_PRIMARY,
            Some(Studio::BORDER_STRONG),
        ),
    }
}

/// A 28 px dialog button of `kind`.
pub fn dialog_button<'a, M: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    on_press: Option<M>,
    kind: DialogButton,
) -> Element<'a, M> {
    let weight = if kind == DialogButton::Secondary {
        font::MEDIUM
    } else {
        font::SEMIBOLD
    };
    let label = studio_text(label.into())
        .size(type_scale::BODY)
        .line_height(iced::Pixels(type_scale::LINE))
        .font(weight);
    button(container(label).center_y(Length::Fill))
        .padding(Padding::from([0, 12]))
        .height(height::TOOLBAR_BUTTON)
        .on_press_maybe(on_press)
        .style(move |_, status| {
            let (fill, text, outline) = dialog_button_colors(kind, status);
            styled(fill, text, radius::WELL, outline)
        })
        .into()
}

// ------------------------------------------------------- segmented control

/// A segmented control (`DESIGN.md`): 22 px (20 in a lane bar), radius 6,
/// a `SEGMENTED` background with an inset `CONTROL_ACTIVE` outline and 2 px
/// of padding; the selected segment `SEGMENT_SELECTED`, radius 4,
/// `TEXT_PRIMARY` medium; the others transparent, `TEXT_SECONDARY`.
/// Selecting the selected segment again sends nothing.
pub fn segmented<'a, T, M>(
    options: &[(T, &'a str)],
    selected: T,
    on_select: impl Fn(T) -> M + 'a,
) -> Segmented<'a, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'a,
{
    Segmented {
        segments: options
            .iter()
            .map(|&(value, label)| (label, value == selected, on_select(value)))
            .collect(),
        height: height::CONTROL,
    }
}

/// See [`segmented`].
pub struct Segmented<'a, M> {
    segments: Vec<(&'a str, bool, M)>,
    height: f32,
}

impl<M> Segmented<'_, M> {
    /// Its height: 22, or 20 inside a lane bar.
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
}

impl<'a, M: Clone + 'a> From<Segmented<'a, M>> for Element<'a, M> {
    fn from(s: Segmented<'a, M>) -> Self {
        let inner = s.height - 4.0;
        let segments = s.segments.into_iter().map(|(label, on, message)| {
            let text = studio_text(label)
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .font(if on { font::MEDIUM } else { font::REGULAR });
            button(container(text).center_y(Length::Fill))
                .padding(Padding::from([0, 10]))
                .height(inner)
                .on_press_maybe((!on).then_some(message))
                .style(move |_, status| {
                    if on {
                        styled(
                            Some(Studio::SEGMENT_SELECTED),
                            Studio::TEXT_PRIMARY,
                            radius::SMALL,
                            None,
                        )
                    } else {
                        styled(
                            None,
                            if is_hovered(status) {
                                Studio::TEXT_PRIMARY
                            } else {
                                Studio::TEXT_SECONDARY
                            },
                            radius::SMALL,
                            None,
                        )
                    }
                })
                .into()
        });
        container(row(segments).align_y(Vertical::Center))
            .padding(2)
            .height(s.height)
            .style(|_| {
                crate::ui::core::theme::boxed(
                    Some(Studio::SEGMENTED),
                    radius::WELL,
                    Some(Studio::CONTROL_ACTIVE),
                )
            })
            .into()
    }
}

// --------------------------------------------------------------- tab strip

/// A tab strip's size (`DESIGN.md`, Tabs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabSize {
    /// Lane tabs: 20 tall, 11 px, radius 4, 8 px padding.
    Lane,
    /// Dock tabs: 22 tall, 11 px, radius 4, 8 px padding.
    Dock,
    /// Panel tabs: 24 tall, 12 px, radius 5, 9 px padding.
    Panel,
}

impl TabSize {
    fn metrics(self) -> (f32, f32, f32, f32) {
        // (height, text, radius, padding)
        match self {
            TabSize::Lane => (height::MINI, type_scale::LABEL, radius::SMALL, 8.0),
            TabSize::Dock => (height::CONTROL, type_scale::LABEL, radius::SMALL, 8.0),
            TabSize::Panel => (height::PANEL_BUTTON, type_scale::BODY, radius::CONTROL, 9.0),
        }
    }
}

/// One tab of a [`tab_strip`]: sentence-case text, an optional 12 px line
/// glyph before it (the dock's "Mixer"), an optional count after it in
/// `TEXT_FAINT` ("Notes 4"), and whether a group divider comes before it.
pub struct Tab<'a, T> {
    pub value: T,
    pub label: Cow<'a, str>,
    pub icon: Option<Glyph>,
    pub count: Option<usize>,
    pub divider_before: bool,
}

/// The glyph box of a tab's icon ([`Tab::icon`]), and its gap to the label.
pub const TAB_ICON_SIZE: f32 = 12.0;
pub const TAB_ICON_GAP: f32 = 6.0;

/// A tab reading `label`.
pub fn tab<'a, T>(value: T, label: impl Into<Cow<'a, str>>) -> Tab<'a, T> {
    Tab {
        value,
        label: label.into(),
        icon: None,
        count: None,
        divider_before: false,
    }
}

impl<T> Tab<'_, T> {
    /// A line glyph before the label, in the label's colour.
    pub fn icon(mut self, glyph: Glyph) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// A count after the label.
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// A group divider before it (the dock's groups).
    pub fn divided(mut self) -> Self {
        self.divider_before = true;
        self
    }
}

/// A strip of text tabs (`DESIGN.md`, Tabs): the selected one on a
/// `CONTROL_ACTIVE` fill in `TEXT_PRIMARY` medium, the others transparent in
/// `TEXT_SECONDARY` (`TEXT_PRIMARY` under the pointer), 2 px apart; a
/// divider is 1 × 14 `BORDER_STRONG` with 6 px either side. A tab's glyph
/// ([`Tab::icon`]) sits 6 px before its label, in the label's colour. A
/// press on a tab sends `on_select` with its value (the selected one too: a
/// caller may toggle).
pub fn tab_strip<'a, T, M>(
    tabs: Vec<Tab<'a, T>>,
    selected: Option<T>,
    size: TabSize,
    on_select: impl Fn(T) -> M + 'a,
) -> Element<'a, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'a,
{
    let (tall, text_size, corner, pad) = size.metrics();
    let mut strip = row![].spacing(2).align_y(Vertical::Center);
    for tab in tabs {
        if tab.divider_before {
            strip = strip.push(
                container(container(Space::new()).width(1).height(14).style(|_| {
                    crate::ui::core::theme::boxed(Some(Studio::BORDER_STRONG), 0.0, None)
                }))
                .padding(Padding::from([0, 6])),
            );
        }
        let on = selected == Some(tab.value);
        let mut label = row![].spacing(4).align_y(Vertical::Center);
        if let Some(glyph) = tab.icon {
            label = label.push(container(line_icon_inherit(glyph, TAB_ICON_SIZE)).padding(
                Padding {
                    right: TAB_ICON_GAP - 4.0,
                    ..Padding::ZERO
                },
            ));
        }
        label = label.push(
            studio_text(tab.label)
                .size(text_size)
                .line_height(iced::Pixels(type_scale::LINE))
                .font(if on { font::MEDIUM } else { font::REGULAR }),
        );
        if let Some(count) = tab.count {
            label = label.push(
                studio_text(count.to_string())
                    .size(text_size)
                    .line_height(iced::Pixels(type_scale::LINE))
                    .tabular()
                    .color(Studio::TEXT_FAINT),
            );
        }
        strip = strip.push(
            button(container(label).center_y(Length::Fill))
                .padding(Padding::from([0.0, pad]))
                .height(tall)
                .on_press(on_select(tab.value))
                .style(move |_, status| {
                    if on {
                        styled(
                            Some(Studio::CONTROL_ACTIVE),
                            Studio::TEXT_PRIMARY,
                            corner,
                            None,
                        )
                    } else {
                        styled(
                            None,
                            if is_hovered(status) {
                                Studio::TEXT_PRIMARY
                            } else {
                                Studio::TEXT_SECONDARY
                            },
                            corner,
                            None,
                        )
                    }
                }),
        );
    }
    strip.into()
}

// --------------------------------------------------------------- the slider

/// The thin slider's hit row.
pub const SLIDER_HEIGHT: f32 = 12.0;
/// Its track (2 px).
pub const SLIDER_TRACK: Color = Studio::CONTROL_ACTIVE;
/// The part of the track before the knob.
pub const SLIDER_FILL: Color = Color::from_rgb8(0x8A, 0x8A, 0x96);
/// That part when the slider's owner is off (a bypassed effect).
pub const SLIDER_FILL_OFF: Color = Color::from_rgb8(0x3A, 0x3A, 0x43);
/// The knob's diameter: 10, or 8 in a track header.
pub const KNOB: f32 = 10.0;
pub const KNOB_SMALL: f32 = 8.0;

/// A thin slider (`DESIGN.md`, Slider): a 12 px tall hit row, a 2 px track
/// in [`SLIDER_TRACK`], filled up to the knob in [`SLIDER_FILL`]
/// ([`SLIDER_FILL_OFF`] when its owner is off), a round `TEXT_PRIMARY` knob
/// with a 1 px drop shadow. `value` is 0 to 1; the knob's centre runs
/// between its radius in from each end.
///
/// A press anywhere on it sets the value from the pointer's x and takes
/// the pointer; every move while held sets it again; the release sends
/// `on_release` once (where a caller commits). Build with [`thin_slider`].
pub struct ThinSlider<'a, M> {
    value: f32,
    on_change: Box<dyn Fn(f32) -> M + 'a>,
    on_release: Option<M>,
    knob: f32,
    off: bool,
    height: f32,
}

/// A thin slider at `value` (0 to 1).
pub fn thin_slider<'a, M>(
    value: f32,
    on_change: impl Fn(f32) -> M + 'a,
    on_release: M,
) -> ThinSlider<'a, M> {
    ThinSlider {
        value: value.clamp(0.0, 1.0),
        on_change: Box::new(on_change),
        on_release: Some(on_release),
        knob: KNOB,
        off: false,
        height: SLIDER_HEIGHT,
    }
}

impl<M> ThinSlider<'_, M> {
    /// The knob's diameter ([`KNOB`] or [`KNOB_SMALL`]).
    pub fn knob(mut self, diameter: f32) -> Self {
        self.knob = diameter;
        self
    }

    /// Its owner is off: the fill dims to [`SLIDER_FILL_OFF`].
    pub fn off(mut self, off: bool) -> Self {
        self.off = off;
        self
    }

    /// A taller hit row (the knob stays centred).
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
}

/// The value a thin slider `width` wide with a `knob` px knob reads at
/// `dx` from its left edge.
pub fn thin_slider_value(dx: f32, width: f32, knob: f32) -> f32 {
    let r = knob / 2.0;
    let span = (width - knob).max(1.0);
    ((dx - r) / span).clamp(0.0, 1.0)
}

/// Where a thin slider's knob centre sits, from its left edge.
pub fn thin_slider_knob_x(value: f32, width: f32, knob: f32) -> f32 {
    knob / 2.0 + value.clamp(0.0, 1.0) * (width - knob).max(0.0)
}

impl<'a, M: Clone + 'a> From<ThinSlider<'a, M>> for Element<'a, M> {
    fn from(s: ThinSlider<'a, M>) -> Self {
        let height = s.height;
        canvas_widget(s).width(Length::Fill).height(height).into()
    }
}

/// A slider's hold: set while a press or drag owes its release.
#[derive(Debug, Default)]
pub struct SliderHold {
    held: bool,
}

impl<M: Clone> canvas::Program<M> for ThinSlider<'_, M> {
    type State = SliderHold;

    fn update(
        &self,
        state: &mut SliderHold,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<M>> {
        let value_at = |x: f32| thin_slider_value(x - bounds.x, bounds.width, self.knob);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                state.held = true;
                Some(canvas::Action::publish((self.on_change)(value_at(p.x))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if state.held => {
                Some(canvas::Action::publish((self.on_change)(value_at(position.x))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.held =>
            {
                state.held = false;
                Some(match self.on_release.clone() {
                    Some(release) => canvas::Action::publish(release).and_capture(),
                    None => canvas::Action::capture(),
                })
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &SliderHold,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw_thin_slider(
            &mut frame,
            bounds.size(),
            self.value,
            self.knob,
            if self.off {
                SLIDER_FILL_OFF
            } else {
                SLIDER_FILL
            },
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &SliderHold,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.held || cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Draws a thin slider at `value` filling `size`: the track, the fill in
/// `fill`, the knob (`knob` across) and its shadow. Shared with the
/// horizontal fader (`studio_controls::studio_fader`).
pub fn draw_thin_slider(frame: &mut Frame, size: Size, value: f32, knob: f32, fill: Color) {
    let mid = (size.height / 2.0).round();
    let x = thin_slider_knob_x(value, size.width, knob);
    let track = |from: f32, to: f32| {
        Path::rounded_rectangle(
            Point::new(from, mid - 1.0),
            Size::new((to - from).max(0.0), 2.0),
            Radius::from(1.0),
        )
    };
    frame.fill(&track(0.0, size.width), SLIDER_TRACK);
    frame.fill(&track(0.0, x), fill);
    let r = knob / 2.0;
    // `0 1px 2px rgba(0,0,0,.6)`: a soft disc a pixel down.
    frame.fill(
        &Path::circle(Point::new(x, mid + 1.0), r + 0.5),
        Color::from_rgba(0.0, 0.0, 0.0, 0.35),
    );
    frame.fill(&Path::circle(Point::new(x, mid), r), Studio::TEXT_PRIMARY);
}

// ----------------------------------------------------------- parameter row

/// The parameter row's label column.
pub const PARAMETER_LABEL_WIDTH: f32 = 62.0;
/// Its value column.
pub const PARAMETER_VALUE_WIDTH: f32 = 54.0;

/// A parameter row (`DESIGN.md`): 22 px; the label in a 62 px column (11 px
/// `TEXT_SECONDARY`), the control, the value right-aligned in a 54 px
/// column (11 px `TEXT_PRIMARY`, tabular figures); 8 px between them.
pub fn parameter_row<'a, M: 'a>(
    label: impl Into<Cow<'a, str>>,
    control: impl Into<Element<'a, M>>,
    value: impl Into<Cow<'a, str>>,
) -> Element<'a, M> {
    row![
        container(
            studio_text(label.into())
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .color(Studio::TEXT_SECONDARY)
                .ellipsis(1),
        )
        .width(PARAMETER_LABEL_WIDTH),
        container(control.into())
            .width(Length::Fill)
            .center_y(height::CONTROL),
        container(
            studio_text(value.into())
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .align_x(Horizontal::Right),
        )
        .width(PARAMETER_VALUE_WIDTH),
    ]
    .spacing(8)
    .height(height::CONTROL)
    .align_y(Vertical::Center)
    .into()
}

// ----------------------------------------------------------- power button

/// The power button's diameter.
pub const POWER_SIZE: f32 = 18.0;

/// The power button's `(fill, glyph)` colours.
pub fn power_colors(on: bool, hovered: bool) -> (Color, Color) {
    if on {
        (
            alpha(Studio::ACCENT, if hovered { 0.24 } else { 0.16 }),
            Studio::ACCENT_BRIGHT,
        )
    } else {
        (
            if hovered {
                Studio::CONTROL_ACTIVE
            } else {
                Studio::CONTROL
            },
            if hovered {
                Studio::TEXT_SECONDARY
            } else {
                Studio::TEXT_DISABLED
            },
        )
    }
}

/// An effect's power button (`DESIGN.md`): an 18 px circle with the power
/// glyph; on, `ACCENT` at 16 % behind `ACCENT_BRIGHT`; off, `CONTROL`
/// behind `TEXT_DISABLED` (the effect's body then draws at 55 %, its
/// caller's). A press sends `on_toggle`.
pub fn power_button<'a, M: Clone + 'a>(on: bool, on_toggle: M) -> Element<'a, M> {
    let glyph = line_icon_inherit(glyphs::POWER, 10.0).stroke(1.25);
    button(container(glyph).center_x(POWER_SIZE).center_y(POWER_SIZE))
        .padding(0)
        .on_press(on_toggle)
        .style(move |_, status| {
            let (fill, glyph) = power_colors(on, is_hovered(status));
            styled(Some(fill), glyph, POWER_SIZE / 2.0, None)
        })
        .into()
}

// ------------------------------------------------------------------ switch

/// The switch's track.
pub const SWITCH_WIDTH: f32 = 28.0;
pub const SWITCH_HEIGHT: f32 = 16.0;
/// Its knob's diameter (2 px inside the track).
pub const SWITCH_KNOB: f32 = 12.0;

/// The switch's track colour: on, `ACCENT` (`ACCENT_BRIGHT` under the
/// pointer); off, `CONTROL_ACTIVE` (`SEGMENT_SELECTED` under the pointer).
pub fn switch_track(on: bool, hovered: bool) -> Color {
    match (on, hovered) {
        (true, false) => Studio::ACCENT,
        (true, true) => Studio::ACCENT_BRIGHT,
        (false, false) => Studio::CONTROL_ACTIVE,
        (false, true) => Studio::SEGMENT_SELECTED,
    }
}

/// A switch (`DESIGN.md`: a setting that is on or off, such as Settings'
/// "Headphones only"; desktop-sized, not Material's): a 28 × 16 track,
/// radius 8, with an inset `BORDER_STRONG` outline while off, and a 12 px
/// `TEXT_PRIMARY` knob at its left (off) or right (on). A press sends
/// `on_toggle`; `None` disables it (the knob at `TEXT_DISABLED`).
pub fn toggle_switch<'a, M: Clone + 'a>(on: bool, on_toggle: Option<M>) -> Element<'a, M> {
    let enabled = on_toggle.is_some();
    let knob = container(Space::new())
        .width(SWITCH_KNOB)
        .height(SWITCH_KNOB)
        .style(move |_| container::Style {
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: Vector::new(0.0, 1.0),
                blur_radius: 2.0,
            },
            ..crate::ui::core::theme::boxed(
                Some(if enabled {
                    Studio::TEXT_PRIMARY
                } else {
                    Studio::TEXT_DISABLED
                }),
                SWITCH_KNOB / 2.0,
                None,
            )
        });
    let face = container(knob)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(2)
        .align_x(if on {
            Horizontal::Right
        } else {
            Horizontal::Left
        })
        .align_y(Vertical::Center);
    button(face)
        .padding(0)
        .width(SWITCH_WIDTH)
        .height(SWITCH_HEIGHT)
        .on_press_maybe(on_toggle)
        .style(move |_, status| {
            let hovered = is_hovered(status) && status != button::Status::Disabled;
            styled(
                Some(switch_track(on, hovered)),
                Studio::TEXT_PRIMARY,
                SWITCH_HEIGHT / 2.0,
                (!on).then_some(Studio::BORDER_STRONG),
            )
        })
        .into()
}

// ------------------------------------------------------------------- M S R

/// What an M / S / R button toggles: its letter and its "on" colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsrKind {
    /// Mute: `RECORD` at 20 % behind `RECORD_TEXT` when on.
    Mute,
    /// Solo: `WARN` at 22 % behind `WARN_TEXT` when on.
    Solo,
    /// Record arm: `RECORD` at 24 % behind `RECORD_TEXT` when on.
    Record,
}

impl MsrKind {
    /// Its letter.
    pub fn letter(self) -> &'static str {
        match self {
            MsrKind::Mute => "M",
            MsrKind::Solo => "S",
            MsrKind::Record => "R",
        }
    }
}

/// An M / S / R button's `(fill, letter)` colours.
pub fn msr_colors(kind: MsrKind, on: bool, hovered: bool) -> (Color, Color) {
    if !on {
        return (
            if hovered {
                Studio::CONTROL_ACTIVE
            } else {
                Studio::CONTROL
            },
            if hovered {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_SECONDARY
            },
        );
    }
    match kind {
        MsrKind::Mute => (alpha(Studio::RECORD, 0.20), Studio::RECORD_TEXT),
        MsrKind::Record => (alpha(Studio::RECORD, 0.24), Studio::RECORD_TEXT),
        MsrKind::Solo => (alpha(Studio::WARN, 0.22), Studio::WARN_TEXT),
    }
}

/// An M / S / R button (`DESIGN.md`): 22 × 18, radius 4, a 10 px semibold
/// letter; off `CONTROL` / `TEXT_SECONDARY`, on per [`MsrKind`]. A press
/// sends `on_press` (`None`: disabled, drawn off and dimmed). Build with
/// [`msr_button`].
pub struct MsrButton<'a, M> {
    kind: MsrKind,
    label: Option<&'a str>,
    on: bool,
    on_press: Option<M>,
    size: Size,
}

/// An M / S / R button of `kind`, on or off.
pub fn msr_button<'a, M>(kind: MsrKind, on: bool, on_press: Option<M>) -> MsrButton<'a, M> {
    MsrButton {
        kind,
        label: None,
        on,
        on_press,
        size: Size::new(22.0, 18.0),
    }
}

impl<'a, M> MsrButton<'a, M> {
    /// Another letter with the kind's colours (the input strip's "I").
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Its size (20–22 × 18–20).
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = Size::new(width, height);
        self
    }
}

impl<'a, M: Clone + 'a> From<MsrButton<'a, M>> for Element<'a, M> {
    fn from(b: MsrButton<'a, M>) -> Self {
        let (kind, on) = (b.kind, b.on);
        let letter = studio_text(b.label.unwrap_or(kind.letter()))
            .size(10)
            .line_height(1.0)
            .font(font::SEMIBOLD);
        button(
            container(letter)
                .center_x(b.size.width)
                .center_y(b.size.height),
        )
        .padding(0)
        .on_press_maybe(b.on_press)
        .style(move |_, status| {
            let (fill, text) = msr_colors(kind, on, is_hovered(status));
            let text = if status == button::Status::Disabled {
                Studio::TEXT_DISABLED
            } else {
                text
            };
            styled(Some(fill), text, radius::SMALL, None)
        })
        .into()
    }
}

// -------------------------------------------------------- fader and meters

/// The mixer fader's column width.
pub const FADER_WIDTH: f32 = 22.0;
/// Its cap.
pub const FADER_CAP: Size = Size::new(21.0, 12.0);
/// A channel meter's width, and the gap between two.
pub const METER_WIDTH: f32 = 4.0;
pub const METER_GAP: f32 = 2.0;
/// The gap between the fader's column and its meters.
pub const FADER_METER_GAP: f32 = 8.0;

/// Where a meter's colours change, from its foot (0) to its top (1):
/// `METER_LOW` up to 70 %, `WARN` by 86 %, `RECORD` at the top.
pub const METER_STOPS: [(f32, Color); 4] = [
    (0.0, Studio::METER_LOW),
    (0.70, Studio::METER_LOW),
    (0.86, Studio::WARN),
    (1.0, Studio::RECORD),
];

/// A meter's colour at `level` (0 to 1) of its height.
pub fn meter_color(level: f32) -> Color {
    let level = level.clamp(0.0, 1.0);
    for pair in METER_STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (pair[0], pair[1]);
        if level <= b {
            let t = if b > a { (level - a) / (b - a) } else { 1.0 };
            return Color {
                r: ca.r + (cb.r - ca.r) * t,
                g: ca.g + (cb.g - ca.g) * t,
                b: ca.b + (cb.b - ca.b) * t,
                a: 1.0,
            };
        }
    }
    Studio::RECORD
}

/// Draws the mixer fader (`DESIGN.md`, Fader) in `area` (a [`FADER_WIDTH`]
/// column), its cap at `value` (0 at the foot, 1 at the top): a 3 px
/// `INSET` slot at 9 px from the left, 4 px scale ticks at the right
/// (`ticks`, 0 to 1 from the top, in `GRID_STRONG`), and a 21 × 12 cap
/// (radius 3, a `#4A4A54` → `#34343D` gradient, a 1 px `TEXT_PRIMARY`
/// centre line, a drop shadow).
pub fn draw_fader(frame: &mut Frame, area: Rectangle, value: f32, ticks: &[f32]) {
    let cap_half = FADER_CAP.height / 2.0;
    let top = area.y + cap_half;
    let span = (area.height - FADER_CAP.height).max(0.0);
    for &t in ticks {
        let y = (top + t.clamp(0.0, 1.0) * span).round() + 0.5;
        frame.stroke(
            &Path::line(
                Point::new(area.x + area.width - 4.0, y),
                Point::new(area.x + area.width, y),
            ),
            Stroke::default()
                .with_color(Studio::GRID_STRONG)
                .with_width(1.0),
        );
    }
    frame.fill(
        &Path::rounded_rectangle(
            Point::new(area.x + 9.0, area.y),
            Size::new(3.0, area.height),
            Radius::from(1.5),
        ),
        Studio::INSET,
    );
    let cy = top + (1.0 - value.clamp(0.0, 1.0)) * span;
    let cap = Rectangle::new(Point::new(area.x, cy - cap_half), FADER_CAP);
    frame.fill(
        &Path::rounded_rectangle(
            cap.position() + Vector::new(0.0, 1.0),
            cap.size(),
            Radius::from(3.0),
        ),
        Color::from_rgba(0.0, 0.0, 0.0, 0.45),
    );
    let gradient = Gradient::Linear(
        gradient::Linear::new(
            Point::new(cap.x, cap.y),
            Point::new(cap.x, cap.y + cap.height),
        )
        .add_stop(0.0, Color::from_rgb8(0x4A, 0x4A, 0x54))
        .add_stop(1.0, Studio::SEGMENT_SELECTED),
    );
    frame.fill(
        &Path::rounded_rectangle(cap.position(), cap.size(), Radius::from(3.0)),
        canvas::Fill::from(gradient),
    );
    // `inset 0 1px 0 rgba(255,255,255,.08)`: a highlight along its top.
    frame.fill(
        &Path::rectangle(
            Point::new(cap.x + 2.0, cap.y),
            Size::new(cap.width - 4.0, 1.0),
        ),
        Color::from_rgba(1.0, 1.0, 1.0, 0.08),
    );
    frame.fill(
        &Path::rectangle(
            Point::new(cap.x + 3.0, (cy - 0.5).round()),
            Size::new(cap.width - 6.0, 1.0),
        ),
        Studio::TEXT_PRIMARY,
    );
}

/// Draws a channel meter in `area` (a [`METER_WIDTH`] well, radius 1,
/// `INSET`) lit from its foot to `level` (0 to 1) in [`METER_STOPS`]'
/// colours, each at its own height (a quiet signal is all green).
pub fn draw_meter(frame: &mut Frame, area: Rectangle, level: f32) {
    frame.fill(
        &Path::rounded_rectangle(area.position(), area.size(), Radius::from(1.0)),
        Studio::INSET,
    );
    let lit = area.height * level.clamp(0.0, 1.0);
    if lit <= 0.0 {
        return;
    }
    let foot = area.y + area.height;
    let mut gradient = gradient::Linear::new(Point::new(area.x, foot), Point::new(area.x, area.y));
    for (at, color) in METER_STOPS {
        gradient = gradient.add_stop(at, color);
    }
    frame.fill(
        &Path::rounded_rectangle(
            Point::new(area.x, foot - lit),
            Size::new(area.width, lit),
            Radius::from(1.0f32.min(lit / 2.0)),
        ),
        canvas::Fill::from(Gradient::Linear(gradient)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::widgets::animated::testing::{near, Frames, BACKGROUND};
    use iced::time::Instant;
    use iced::widget::{center, column};
    use iced::Event;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Go,
        Pick(u8),
        Level(f32),
        Done,
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }
    fn move_to(x: f32, y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(x, y),
        })
    }

    /// A toggle shows its state under the pointer, where the click lands: on
    /// and off draw differently while hovered (the icon's colour alone was
    /// the same for both, so a click seemed to do nothing).
    #[test]
    fn a_toggle_on_and_one_off_draw_differently_under_the_pointer() {
        assert_eq!(
            icon_button_fill(button::Status::Hovered, false),
            Some(Studio::HOVER)
        );
        assert_eq!(
            icon_button_fill(button::Status::Hovered, true),
            Some(TOGGLE_ON)
        );
        assert_eq!(
            icon_button_fill(button::Status::Active, true),
            Some(TOGGLE_ON)
        );
        assert_ne!(TOGGLE_ON, Studio::HOVER);
        // In a toolbar group, drawn: hovered, the one on keeps its fill.
        let view = |on: bool| -> Element<'static, Msg> {
            container(ToolbarGroup::new().push(group_toggle(
                glyphs::LOOP,
                "Loop",
                Some(Msg::Go),
                on,
            )))
            .padding(10)
            .into()
        };
        let hovered = |on: bool| {
            let mut window = Frames::new(Size::new(100.0, 80.0));
            window.point_at(Point::new(27.0, 25.0));
            window.frame(view(on), &[move_to(27.0, 25.0)], Instant::now());
            window.frame(view(on), &[], Instant::now());
            // Inside the button, clear of the glyph and the group's outline.
            window.pixel(Point::new(15.0, 15.0))
        };
        let (off, on) = (hovered(false), hovered(true));
        assert!(near(off, Studio::SURFACE_RAISED), "{off:?}");
        assert!(near(on, TOGGLE_ON), "{on:?}");
    }

    #[test]
    fn an_icon_button_is_transparent_until_hovered_and_its_icon_follows() {
        assert_eq!(
            icon_button_color(button::Status::Active, false, false),
            Studio::TEXT_SECONDARY
        );
        assert_eq!(
            icon_button_color(button::Status::Hovered, false, false),
            Studio::TEXT_PRIMARY
        );
        assert_eq!(
            icon_button_color(button::Status::Active, true, true),
            Studio::ACCENT_BRIGHT
        );
        assert_eq!(
            icon_button_color(button::Status::Disabled, true, false),
            Studio::TEXT_DISABLED
        );
        // Drawn: nothing at rest, the hover fill under the pointer.
        let view = || -> Element<'static, Msg> {
            container(icon_button(glyphs::PLUS, Msg::Go).tooltip("New practice"))
                .padding(10)
                .into()
        };
        let mut window = Frames::new(Size::new(100.0, 80.0));
        window.frame(view(), &[], Instant::now());
        // Inside the 28 px button, clear of the glyph.
        let corner = Point::new(13.0, 13.0);
        assert!(near(window.pixel(corner), BACKGROUND));
        let glyph = (Point::new(22.0, 22.0), Point::new(26.0, 26.0));
        let rest = window.ink(glyph.0, glyph.1);
        window.point_at(Point::new(24.0, 24.0));
        window.frame(view(), &[move_to(24.0, 24.0)], Instant::now());
        window.frame(view(), &[], Instant::now());
        assert!(
            near(window.pixel(corner), Studio::HOVER),
            "{:?}",
            window.pixel(corner)
        );
        // The glyph takes the hover's text colour: lighter than at rest.
        let hovered = window.ink(glyph.0, glyph.1);
        assert!(
            rest > 0x40 && hovered > rest + 0x20,
            "{rest:#x} {hovered:#x}"
        );
    }

    #[test]
    fn an_icon_button_presses_and_a_disabled_one_does_not() {
        let mut ui = iced_test::simulator(Element::from(center(
            icon_button(glyphs::SETTINGS, Msg::Go).tooltip("Settings"),
        )));
        ui.point_at(Point::new(512.0, 384.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Go]);
        let mut ui = iced_test::simulator(Element::from(center(
            icon_button(glyphs::SETTINGS, Msg::Go).enabled(false),
        )));
        ui.point_at(Point::new(512.0, 384.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn the_buttons_are_their_design_sizes() {
        let mut ui = iced_test::simulator::<Msg, _, _>(column![
            ghost_button("New take", Some(glyphs::PLUS), Some(Msg::Go)),
            Element::from(popup_button("Interpretation 1", Some(Msg::Go))),
            primary_button("Create", Some(Msg::Go)),
            secondary_button("Cancel", Some(Msg::Go)),
            Element::from(msr_button(MsrKind::Mute, false, Some(Msg::Go))),
            power_button(true, Msg::Go),
        ]);
        let tall = |ui: &mut iced_test::Simulator<'_, Msg>, label: &str| {
            ui.find(label).expect(label).bounds().height
        };
        assert!(tall(&mut ui, "New take") <= height::CONTROL);
        assert!(tall(&mut ui, "Interpretation 1") <= height::CONTROL);
        assert!(tall(&mut ui, "Create") <= height::TOOLBAR_BUTTON);
        ui.click("Cancel").expect("the secondary button");
        ui.click("M").expect("the mute button");
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Go, Msg::Go]
        );
    }

    #[test]
    fn dialog_buttons_paint_primary_secondary_and_destructive() {
        use button::Status::{Active, Disabled, Hovered};
        assert_eq!(
            dialog_button_colors(DialogButton::Primary, Active),
            (Some(Studio::ACCENT), Studio::ON_ACCENT, None)
        );
        assert_eq!(
            dialog_button_colors(DialogButton::Primary, Hovered).0,
            Some(Studio::ACCENT_BRIGHT)
        );
        assert_eq!(
            dialog_button_colors(DialogButton::Secondary, Active),
            (None, Studio::TEXT_PRIMARY, Some(Studio::BORDER_STRONG))
        );
        assert_eq!(
            dialog_button_colors(DialogButton::Destructive, Active).0,
            Some(Studio::RECORD)
        );
        assert_eq!(
            dialog_button_colors(DialogButton::Primary, Disabled).1,
            Studio::TEXT_DISABLED
        );
    }

    #[test]
    fn a_pop_up_button_with_a_long_label_keeps_to_its_max_width() {
        let long = "Powerful belt, second verse, with the key change";
        let mut ui = iced_test::simulator(Element::from(row![
            popup_button(long, Some(Msg::Go)).max_width(140.0)
        ]));
        let label = ui.find(long).expect("the label").bounds();
        assert!(
            label.x + label.width <= 140.0 - POPUP_CHROME_WIDTH + 8.0 + 0.5,
            "ellipsized inside the button: {label:?}"
        );
        // A short one stays as wide as its label.
        let mut ui =
            iced_test::simulator(Element::from(row![
                popup_button("Tender", Some(Msg::Go)).max_width(140.0)
            ]));
        let label = ui.find("Tender").expect("the label").bounds();
        assert!(label.width < 60.0, "{label:?}");
    }

    #[test]
    fn a_segmented_control_selects_the_others_only() {
        let options = [(1u8, "Accompaniment"), (2u8, "Original")];
        let mut ui = iced_test::simulator(Element::from(segmented(&options, 1, Msg::Pick)));
        ui.click("Accompaniment").expect("the selected segment");
        ui.click("Original").expect("the other segment");
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Pick(2)]);
        // The selected segment's fill, 2 px in.
        let mut window = Frames::new(Size::new(300.0, 40.0));
        window.frame(
            Element::from(segmented(&options, 1, Msg::Pick)),
            &[],
            Instant::now(),
        );
        assert!(near(
            window.pixel(Point::new(4.0, 11.0)),
            Studio::SEGMENT_SELECTED
        ));
        assert!(near(window.pixel(Point::new(1.0, 11.0)), Studio::SEGMENTED));
    }

    #[test]
    fn a_tab_strip_reads_sentence_case_with_counts_and_selects() {
        let tabs = vec![
            tab(1u8, "Lyrics"),
            tab(2u8, "Notes").count(4),
            tab(3u8, "Techniques").divided(),
        ];
        let mut ui = iced_test::simulator(tab_strip(tabs, Some(1), TabSize::Panel, Msg::Pick));
        assert!(ui.find("Notes").is_ok() && ui.find("4").is_ok());
        assert!(ui.find("NOTES").is_err());
        ui.click("Techniques").expect("a tab");
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Pick(3)]);
    }

    #[test]
    fn a_tab_with_a_glyph_draws_it_before_its_label_and_still_selects() {
        let strip = || -> Element<'static, Msg> {
            container(tab_strip(
                vec![tab(1u8, "Mixer").icon(glyphs::MIXER)],
                Some(1),
                TabSize::Dock,
                Msg::Pick,
            ))
            .padding(4)
            .into()
        };
        let mut ui = iced_test::simulator(strip());
        let label = ui.find("Mixer").expect("the label").bounds();
        // The 8 px padding, the 12 px glyph and its 6 px gap.
        assert_eq!(label.x, 4.0 + 8.0 + TAB_ICON_SIZE + TAB_ICON_GAP);
        ui.click("Mixer").expect("the tab");
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Pick(1)]);
        // The selected tab's fill, and the glyph drawn left of the label in
        // its text colour.
        let mut window = Frames::new(Size::new(120.0, 30.0));
        window.frame(strip(), &[], Instant::now());
        let fill = window.pixel(Point::new(6.0, 6.0));
        assert!(near(fill, Studio::CONTROL_ACTIVE), "{fill:?}");
        let ink = window.ink(Point::new(12.0, 9.0), Point::new(24.0, 21.0));
        assert!(ink > 0x60, "{ink:#x}");
    }

    #[test]
    fn a_thin_slider_sets_on_press_and_drag_and_commits_on_release() {
        assert_eq!(thin_slider_value(5.0, 110.0, 10.0), 0.0);
        assert_eq!(thin_slider_value(105.0, 110.0, 10.0), 1.0);
        assert!((thin_slider_value(55.0, 110.0, 10.0) - 0.5).abs() < 1e-6);
        assert_eq!(thin_slider_knob_x(0.5, 110.0, 10.0), 55.0);
        let view = || -> Element<'static, Msg> {
            container(thin_slider(0.25, Msg::Level, Msg::Done))
                .width(110)
                .into()
        };
        let mut ui = iced_test::simulator(view());
        ui.point_at(Point::new(55.0, 6.0));
        let _ = ui.simulate([press(), move_to(105.0, 6.0), release()]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Level(0.5), Msg::Level(1.0), Msg::Done]
        );
    }

    #[test]
    fn a_thin_slider_draws_its_track_fill_and_knob() {
        let mut window = Frames::new(Size::new(110.0, 12.0));
        window.frame(
            container(thin_slider(0.5, Msg::Level, Msg::Done)).width(110),
            &[],
            Instant::now(),
        );
        // The track (2 px at the middle) before and after the knob.
        assert!(near(window.pixel(Point::new(20.0, 5.0)), SLIDER_FILL));
        assert!(near(window.pixel(Point::new(90.0, 5.0)), SLIDER_TRACK));
        // The knob's middle.
        assert!(near(
            window.pixel(Point::new(55.0, 6.0)),
            Studio::TEXT_PRIMARY
        ));
        // Off the track: nothing.
        assert!(near(window.pixel(Point::new(20.0, 1.0)), BACKGROUND));
    }

    #[test]
    fn the_power_and_msr_buttons_take_their_on_colours() {
        assert_eq!(
            power_colors(true, false),
            (alpha(Studio::ACCENT, 0.16), Studio::ACCENT_BRIGHT)
        );
        assert_eq!(
            power_colors(false, false),
            (Studio::CONTROL, Studio::TEXT_DISABLED)
        );
        assert_eq!(
            msr_colors(MsrKind::Mute, true, false),
            (alpha(Studio::RECORD, 0.20), Studio::RECORD_TEXT)
        );
        assert_eq!(
            msr_colors(MsrKind::Solo, true, false),
            (alpha(Studio::WARN, 0.22), Studio::WARN_TEXT)
        );
        assert_eq!(
            msr_colors(MsrKind::Record, true, true),
            (alpha(Studio::RECORD, 0.24), Studio::RECORD_TEXT)
        );
        assert_eq!(
            msr_colors(MsrKind::Solo, false, false),
            (Studio::CONTROL, Studio::TEXT_SECONDARY)
        );
    }

    #[test]
    fn a_switch_toggles_and_draws_its_knob_on_its_side() {
        // A press toggles; a disabled switch takes none.
        let mut ui =
            iced_test::simulator(Element::from(center(toggle_switch(false, Some(Msg::Go)))));
        ui.point_at(Point::new(512.0, 384.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Go]);
        let mut ui = iced_test::simulator(Element::from(center(toggle_switch::<Msg>(true, None))));
        ui.point_at(Point::new(512.0, 384.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert!(ui.into_messages().next().is_none());

        // Its track and knob: the knob at the left while off, the right
        // while on; the track the accent while on.
        let mut window = Frames::new(Size::new(SWITCH_WIDTH, SWITCH_HEIGHT));
        let (left, right) = (Point::new(8.0, 8.0), Point::new(20.0, 8.0));
        window.frame(toggle_switch(false, Some(Msg::Go)), &[], Instant::now());
        assert!(
            near(window.pixel(left), Studio::TEXT_PRIMARY),
            "off: the knob"
        );
        assert!(near(window.pixel(right), switch_track(false, false)));
        window.frame(toggle_switch(true, Some(Msg::Go)), &[], Instant::now());
        assert!(
            near(window.pixel(right), Studio::TEXT_PRIMARY),
            "on: the knob"
        );
        assert!(near(window.pixel(left), Studio::ACCENT));
    }

    #[test]
    fn a_parameter_row_lays_out_label_control_and_value() {
        let mut ui = iced_test::simulator::<Msg, _, _>(
            container(parameter_row(
                "Threshold",
                thin_slider(0.5, Msg::Level, Msg::Done),
                "−28 dB",
            ))
            .width(264),
        );
        let label = ui.find("Threshold").expect("the label").bounds();
        let value = ui.find("−28 dB").expect("the value").bounds();
        assert_eq!(label.x, 0.0);
        // Right-aligned in the last 54 px.
        assert!((value.x + value.width - 264.0).abs() < 0.5, "{value:?}");
        assert!(value.x >= 264.0 - PARAMETER_VALUE_WIDTH);
    }

    #[test]
    fn a_meter_is_green_low_warn_high_and_red_at_the_top() {
        assert_eq!(meter_color(0.3), Studio::METER_LOW);
        assert_eq!(meter_color(0.70), Studio::METER_LOW);
        assert!(near(meter_color(0.86), Studio::WARN));
        assert!(near(meter_color(1.0), Studio::RECORD));
    }

    #[test]
    fn the_fader_and_meters_draw_in_their_column() {
        struct Strip;
        impl canvas::Program<Msg> for Strip {
            type State = ();
            fn draw(
                &self,
                _: &(),
                renderer: &Renderer,
                _: &Theme,
                bounds: Rectangle,
                _: mouse::Cursor,
            ) -> Vec<Geometry> {
                let mut frame = Frame::new(renderer, bounds.size());
                draw_fader(
                    &mut frame,
                    Rectangle::new(Point::ORIGIN, Size::new(FADER_WIDTH, 100.0)),
                    0.5,
                    &[0.0, 0.5, 1.0],
                );
                draw_meter(
                    &mut frame,
                    Rectangle::new(Point::new(30.0, 0.0), Size::new(METER_WIDTH, 100.0)),
                    0.5,
                );
                vec![frame.into_geometry()]
            }
        }
        let mut window = Frames::new(Size::new(40.0, 100.0));
        window.frame(
            Element::<Msg>::from(canvas_widget(Strip).width(40).height(100)),
            &[],
            Instant::now(),
        );
        // The cap's centre line at the middle, the slot above it.
        assert!(
            near(window.pixel(Point::new(10.0, 49.0)), Studio::TEXT_PRIMARY)
                || near(window.pixel(Point::new(10.0, 50.0)), Studio::TEXT_PRIMARY)
        );
        assert!(near(window.pixel(Point::new(10.0, 20.0)), Studio::INSET));
        // The meter: lit green at its foot, dark above its level.
        assert!(near(
            window.pixel(Point::new(32.0, 95.0)),
            Studio::METER_LOW
        ));
        assert!(near(window.pixel(Point::new(32.0, 20.0)), Studio::INSET));
    }
}
