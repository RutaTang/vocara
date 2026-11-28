//! Port of `ui/song_practice_detail/widgets/technique_picker.dart`: the
//! picker that chooses a technique from the user's library, to drop on a
//! take's technique track (`showTechniquePicker`). It closes with the chosen
//! [`Technique`], or nothing if cancelled; its title says where the technique
//! lands (a playhead point or a drawn range).
//!
//! The library loads lazily (from the repository): the picker opens with a
//! spinner and the screen hands it the list when the read completes
//! ([`TechniquePicker::loaded`]; `None` when it could not be read — said so,
//! not "no techniques yet"). Dart tied that read to the dialog it opened and
//! dropped a result that came back after the dialog closed (`mounted`); here
//! the screen holds the open picker, and a result for a picker that is gone
//! has nowhere to land.
//!
//! The screen draws [`TechniquePicker::view`] over its page (as Home draws
//! its "Loading practice" dialog: under the window's toolbar, which Dart's
//! dialog route covered), runs [`TechniquePicker::focus`] when it opens
//! (`autofocus`), and feeds it its messages until
//! [`TechniquePicker::update`] says it closed. It is modal like the studio
//! dialogs (`confirm_dialog::modal`): the same barrier, a tap on it or
//! Escape cancels, and nothing beneath sees a key. It comes in and goes as
//! they do, drawn at a fade ([`TechniquePicker::view_at`]) by the screen,
//! which keeps its `DialogMotion`.
//!
//! The keyboard picks too, as a macOS search list does: one row is
//! highlighted (the first match, while the search changes), Up and Down
//! move the highlight, and Return picks it. The card's top stays put while
//! the list grows and shrinks under the search, so the field does not move
//! under the caret as the user types.

use crate::ui::core::text;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, center, column, container, opaque, row, stack, Column, Space};
use iced::{
    keyboard, mouse, widget, Element, Event, Length, Padding, Pixels, Rectangle, Renderer, Size,
    Task, Theme, Vector,
};
use vocara_model::models::technique::Technique;
use vocara_model::util::dart_string;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, font, Studio};
use crate::ui::core::widgets::confirm_dialog::{self, barrier, Fade};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::focus_traversal::focus_scope;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::text_field::text_input;
use crate::ui::core::widgets::wrap::Wrap;

/// `showTechniquePicker`'s default title.
pub const DEFAULT_TITLE: &str = "Add technique at playhead";

/// The card's width.
pub const WIDTH: f32 = 420.0;

/// The list's height cap; a longer library scrolls inside it.
pub const LIST_MAX_HEIGHT: f32 = 360.0;

/// What the user did in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The search text changed.
    Query(String),
    /// A technique (its id) was tapped.
    Pick(String),
    /// Up: the highlight moves to the row above.
    Previous,
    /// Down: the highlight moves to the row below.
    Next,
    /// Return: the highlighted technique is picked.
    PickHighlighted,
    /// A tap on the barrier, or Escape.
    Cancel,
}

/// An open picker: its title, the search as typed, and the library once it
/// has loaded.
#[derive(Debug, Clone)]
pub struct TechniquePicker {
    title: String,
    query: String,
    /// The library: `None` while it loads.
    all: Option<Vec<Technique>>,
    /// The library could not be read: said so, not "no techniques yet".
    failed: bool,
    /// The highlighted row, in the list as filtered (Return picks it).
    highlight: usize,
    search: widget::Id,
}

impl TechniquePicker {
    /// A picker titled `title`, its library still loading.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            query: String::new(),
            all: None,
            failed: false,
            highlight: 0,
            search: widget::Id::unique(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The search as typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The search field's id.
    pub fn search_id(&self) -> widget::Id {
        self.search.clone()
    }

    /// The library read completed: `None` when it could not be read.
    pub fn loaded(&mut self, list: Option<Vec<Technique>>) {
        self.failed = list.is_none();
        self.all = Some(list.unwrap_or_default());
        self.highlight = 0;
    }

    /// Whether the library is still loading.
    pub fn is_loading(&self) -> bool {
        self.all.is_none()
    }

    /// The task that puts the keyboard in the search field when the picker
    /// opens (`autofocus: true`).
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        iced::widget::operation::focus(self.search.clone())
    }

    /// The techniques the search lets through: those whose name, an alias or
    /// a tag contains the trimmed, lower-cased query; all of them for an
    /// empty query.
    pub fn filtered(&self) -> Vec<&Technique> {
        let all = self.all.as_deref().unwrap_or_default();
        let q = dart_string::trim(&self.query).to_lowercase();
        if q.is_empty() {
            return all.iter().collect();
        }
        let matches = |t: &Technique| {
            t.name.to_lowercase().contains(&q)
                || t.aliases.iter().any(|a| a.to_lowercase().contains(&q))
                || t.tags.iter().any(|tag| tag.to_lowercase().contains(&q))
        };
        all.iter().filter(|t| matches(t)).collect()
    }

    /// The highlighted technique: the first match until Up or Down moves
    /// the highlight.
    pub fn highlighted(&self) -> Option<&Technique> {
        let filtered = self.filtered();
        let last = filtered.len().checked_sub(1)?;
        filtered.get(self.highlight.min(last)).copied()
    }

    /// Handles `message`; `Some` when the picker closes: `Some(technique)`
    /// picked, `None` cancelled.
    pub fn update(&mut self, message: Message) -> Option<Option<Technique>> {
        match message {
            Message::Query(query) => {
                // A new search highlights its first match.
                self.query = query;
                self.highlight = 0;
                None
            }
            Message::Previous => {
                self.highlight = self.highlight.saturating_sub(1);
                None
            }
            Message::Next => {
                let last = self.filtered().len().saturating_sub(1);
                self.highlight = (self.highlight + 1).min(last);
                None
            }
            Message::PickHighlighted => self.highlighted().map(|t| Some(t.clone())),
            Message::Cancel => Some(None),
            Message::Pick(id) => self
                .all
                .as_ref()
                .and_then(|all| all.iter().find(|t| t.id == id))
                .map(|t| Some(t.clone())),
        }
    }

    /// The picker over the page, all in: the barrier and the centred card.
    pub fn view(&self) -> Element<'_, Message> {
        self.view_at(Fade::SHOWN)
    }

    /// The picker at `fade` (`confirm_dialog`'s module docs: its host keeps
    /// its `DialogMotion`).
    pub fn view_at(&self, fade: Fade) -> Element<'_, Message> {
        let header = container(column![
            confirm_dialog::title(self.title.as_str()),
            Space::new().height(12),
            search_field(&self.query, self.search.clone()),
        ])
        .padding(Padding {
            top: confirm_dialog::CARD_PADDING,
            right: confirm_dialog::CARD_PADDING,
            bottom: 12.0,
            left: confirm_dialog::CARD_PADDING,
        });
        let card = container(
            column![
                header,
                container(Space::new())
                    .width(Length::Fill)
                    .height(1)
                    .style(theme::fill(Studio::BORDER)),
                self.body(),
            ]
            .width(Length::Fill),
        )
        .width(WIDTH)
        .style(theme::dialog_card)
        // The rows' hover keeps inside the card's rounded corners.
        .clip(true);
        modal(container(card).padding(CARD_MARGIN).into(), fade)
    }

    fn body(&self) -> Element<'_, Message> {
        let Some(all) = &self.all else {
            return container(spinner(18.0, 2.0, Studio::TEXT_SECONDARY))
                .width(Length::Fill)
                .height(120)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .into();
        };
        if self.failed {
            return hint("Couldn't open the technique library. Try again in a moment.");
        }
        if all.is_empty() {
            return hint("No techniques yet. Add some in the technique library first.");
        }
        let filtered = self.filtered();
        if filtered.is_empty() {
            return hint("No techniques match.");
        }
        let highlight = self.highlight.min(filtered.len() - 1);
        let rows = Column::with_children(
            filtered
                .into_iter()
                .enumerate()
                .map(|(i, t)| technique_row(t, i == highlight)),
        )
        .width(Length::Fill)
        .padding(LIST_PADDING);
        container(scrollable(rows).fading())
            .width(Length::Fill)
            .max_height(LIST_MAX_HEIGHT)
            .into()
    }
}

/// The list's inset inside the card, under the search.
const LIST_PADDING: f32 = 6.0;

/// The card at its tallest: the header (the padding, an 18 px title, 12,
/// the field, 12), the rule and the list at its cap. The card hangs from
/// the top of a box this tall centred in the window, where a full list's
/// card is centred, and keeps its top there however short the list.
const CARD_MAX_HEIGHT: f32 =
    confirm_dialog::CARD_PADDING + 18.0 + 12.0 + theme::FIELD_HEIGHT + 12.0 + 1.0 + LIST_MAX_HEIGHT;

/// `_SearchField`: `DESIGN.md`'s text field (28 px, `WELL`, radius 6, an
/// inset outline, the accent at 60 % while focused, 12 px text) with a
/// 14 px search glyph inside its left end: the field's text starts past it.
fn search_field(query: &str, id: widget::Id) -> Element<'_, Message> {
    let field = text_input("Search techniques", query)
        .id(id)
        .on_input(Message::Query)
        .size(theme::FIELD_TEXT_SIZE)
        .line_height(Pixels(theme::type_scale::LINE))
        .padding(Padding {
            left: SEARCH_TEXT_INSET,
            ..theme::FIELD_PADDING
        })
        .caret_color(Studio::ACCENT)
        .style(theme::field);
    stack![
        field,
        container(icons::icon(Icon::Search, 14.0, Studio::TEXT_FAINT))
            .padding(Padding {
                left: 9.0,
                ..Padding::ZERO
            })
            .height(theme::FIELD_HEIGHT)
            .align_y(Vertical::Center),
    ]
    .width(Length::Fill)
    .into()
}

/// Where the search field's text starts: past the glyph's 9 px inset, the
/// 14 px glyph and a 7 px gap.
pub const SEARCH_TEXT_INSET: f32 = 9.0 + 14.0 + 7.0;

/// `_TechniqueRow`: a list row (`HOVER` under the pointer and while
/// `highlighted`, radius 4), 10 px in: a 14 px `TEXT_SECONDARY` waypoints
/// glyph, the name (12 px medium) and, when it has tags, them joined by
/// " · " under it (11 px `TEXT_FAINT`) — one line each.
fn technique_row(t: &Technique, highlighted: bool) -> Element<'_, Message> {
    let mut labels = column![studio_text(t.name.as_str())
        .size(theme::type_scale::BODY)
        .line_height(Pixels(theme::type_scale::LINE))
        .font(font::MEDIUM)
        .color(Studio::TEXT_PRIMARY)
        .ellipsis(1)]
    .width(Length::Fill);
    if !t.tags.is_empty() {
        labels = labels.push(
            studio_text(t.tags.join(" · "))
                .size(theme::type_scale::LABEL)
                .line_height(Pixels(14.0))
                .color(Studio::TEXT_FAINT)
                .ellipsis(1),
        );
    }
    button(
        row![
            icons::icon(Icon::Waypoints, 14.0, Studio::TEXT_SECONDARY),
            labels,
        ]
        .spacing(10)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding([6, 10])
    .style(move |theme, status| row_style(theme, status, highlighted))
    .on_press(Message::Pick(t.id.clone()))
    .into()
}

/// A list row's face: `HOVER` under the pointer at once, and while it is
/// the highlighted row, radius 4.
fn row_style(_: &iced::Theme, status: button::Status, highlighted: bool) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: (hovered || highlighted).then_some(Studio::HOVER.into()),
        text_color: Studio::TEXT_PRIMARY,
        border: iced::Border {
            radius: theme::radius::SMALL.into(),
            ..iced::Border::default()
        },
        shadow: iced::Shadow::default(),
        snap: true,
    }
}

/// `_Hint`: a faint centred message (12 px `TEXT_FAINT`), 20 / 28 padding.
fn hint(label: &str) -> Element<'_, Message> {
    container(
        text(label)
            .size(theme::type_scale::BODY)
            .line_height(1.5)
            .align_x(Horizontal::Center)
            .width(Length::Fill)
            .color(Studio::TEXT_FAINT),
    )
    .width(Length::Fill)
    .padding([28, 20])
    .into()
}

/// `content` over the studio dialogs' dismissible barrier
/// (`Color(0xB0000000)`), as `confirm_dialog::modal` draws it: a tap on the
/// barrier cancels, the card takes its own taps, Escape cancels and no key
/// reaches what lies beneath. Both at `fade` (the card as one layer).
/// Centred across; down, hung from the top of a box as tall as the card at
/// its tallest ([`CARD_MAX_HEIGHT`]), centred, so it keeps its place as the
/// list filters. Up and Down move the highlight ([`ArrowKeys`]) and Return
/// picks it.
fn modal(content: Element<'_, Message>, fade: Fade) -> Element<'_, Message> {
    let barrier = barrier(theme::DIALOG_BARRIER, fade.barrier, Some(Message::Cancel));
    let card = container(opaque(fade.card(content)))
        .height(CARD_MAX_HEIGHT + 2.0 * CARD_MARGIN)
        .align_y(Vertical::Top);
    let layers = stack![barrier, center(card)];
    let modal = Wrap::<Message, ()>::new(
        // A dialog route's scope: Tab goes round its fields.
        focus_scope(layers),
        Box::new(|_, event, _, _, shell| {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                ..
            }) = event
            {
                match named {
                    keyboard::key::Named::Escape => shell.publish(Message::Cancel),
                    keyboard::key::Named::Enter if !shell.is_event_captured() => {
                        shell.publish(Message::PickHighlighted);
                    }
                    _ => {}
                }
            }
            if let Event::Keyboard(_) = event {
                // The picker holds the keyboard (its search field has
                // already had its turn).
                shell.capture_event();
            }
        }),
    );
    Element::new(ArrowKeys {
        content: modal.into(),
    })
}

/// The space around the card ([`TechniquePicker::view_at`]'s padding).
const CARD_MARGIN: f32 = 24.0;

/// Takes Up and Down (with no modifier) before its content sees them, as
/// [`Message::Previous`] and [`Message::Next`]: the search field's own Up
/// and Down move its caret to the ends of the text, under the typing. With
/// Shift they are still the field's, selecting.
struct ArrowKeys<'a> {
    content: Element<'a, Message>,
}

impl Widget<Message, Theme, Renderer> for ArrowKeys<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
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

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(named),
            modifiers,
            ..
        }) = event
        {
            let step = match named {
                keyboard::key::Named::ArrowUp => Some(Message::Previous),
                keyboard::key::Named::ArrowDown => Some(Message::Next),
                _ => None,
            };
            if let Some(step) = step.filter(|_| modifiers.is_empty()) {
                shell.publish(step);
                shell.capture_event();
                return;
            }
        }
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
        tree: &Tree,
        layout: Layout<'_>,
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
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests;
