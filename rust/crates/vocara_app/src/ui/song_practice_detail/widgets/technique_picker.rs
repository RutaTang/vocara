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
//! its "Loading practice" dialog: under the window drag bar, which Dart's
//! dialog route covered), runs [`TechniquePicker::focus`] when it opens
//! (`autofocus`), and feeds it its messages until
//! [`TechniquePicker::update`] says it closed. It is modal like the studio
//! dialogs (`confirm_dialog::modal`): the same barrier, a tap on it or
//! Escape cancels, and nothing beneath sees a key. It comes in and goes as
//! they do, drawn at a fade ([`TechniquePicker::view_at`]) by the screen,
//! which keeps its `DialogMotion`.

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, center, column, container, opaque, row, scrollable, stack, text_input, Column, Space,
};
use iced::{keyboard, widget, Element, Event, Length, Padding, Task};
use vocara_model::models::technique::Technique;
use vocara_model::util::dart_string;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, font, Studio};
use crate::ui::core::widgets::confirm_dialog::{barrier, Fade};
use crate::ui::core::widgets::fading_scrollbar::Fading;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
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

    /// Handles `message`; `Some` when the picker closes: `Some(technique)`
    /// picked, `None` cancelled.
    pub fn update(&mut self, message: Message) -> Option<Option<Technique>> {
        match message {
            Message::Query(query) => {
                self.query = query;
                None
            }
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
            text(self.title.as_str())
                .size(16)
                .font(font::BOLD)
                .color(fade.color(Studio::TEXT_PRIMARY)),
            Space::new().height(12),
            search_field(&self.query, self.search.clone(), fade),
        ])
        .padding(Padding {
            top: 18.0,
            right: 20.0,
            bottom: 12.0,
            left: 20.0,
        });
        let card = container(
            column![
                header,
                container(Space::new())
                    .width(Length::Fill)
                    .height(1)
                    .style(theme::fill(fade.color(Studio::BORDER))),
                self.body(fade),
            ]
            .width(Length::Fill),
        )
        .width(WIDTH)
        // Inside the 1 px border (a decoration's border pads its child).
        .padding(1)
        .style(fade.style(theme::dialog_card));
        modal(container(card).padding(24).into(), fade)
    }

    fn body(&self, fade: Fade) -> Element<'_, Message> {
        let Some(all) = &self.all else {
            return container(spinner(20.0, 2.0, fade.color(Studio::TEXT_SECONDARY)))
                .width(Length::Fill)
                .height(120)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .into();
        };
        if self.failed {
            return hint(
                "Couldn't open the technique library. Try again in a moment.",
                fade,
            );
        }
        if all.is_empty() {
            return hint(
                "No techniques yet. Add some in the technique library first.",
                fade,
            );
        }
        let filtered = self.filtered();
        if filtered.is_empty() {
            return hint("No techniques match.", fade);
        }
        let rows = Column::with_children(filtered.into_iter().map(|t| technique_row(t, fade)))
            .width(Length::Fill)
            .padding([6, 0]);
        container(scrollable(rows).fading_with(fade.scrollbar(theme::scrollbar)))
            .width(Length::Fill)
            .max_height(LIST_MAX_HEIGHT)
            .into()
    }
}

/// `_SearchField`: 34 tall, the inset well (radius 8), a search glyph before
/// the field. The well's height is fixed, so the field's own inset
/// ([`theme::FIELD_PADDING`]) moves its text in from the sides only: the
/// field stays centred in the well.
fn search_field(query: &str, id: widget::Id, fade: Fade) -> Element<'_, Message> {
    container(
        row![
            icons::icon(Icon::Search, 14.0, fade.color(Studio::TEXT_FAINT)),
            Space::new().width(8),
            text_input("Search techniques", query)
                .id(id)
                .on_input(Message::Query)
                .size(13)
                .line_height(1.0)
                .padding(theme::FIELD_PADDING)
                .style(fade.input(theme::studio_input)),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(34)
    // 10 inside the 1 px border.
    .padding([0, 11])
    .align_y(Vertical::Center)
    .style(fade.style(theme::inset(8.0)))
    .into()
}

/// `_TechniqueRow`: `Studio.hover` under the pointer, 20 / 9 padding; a
/// coral waypoints glyph, the name (13.5 px w600) and, when it has tags,
/// them joined by " · " under it — one line each.
fn technique_row(t: &Technique, fade: Fade) -> Element<'_, Message> {
    let mut labels = column![studio_text(t.name.as_str())
        .size(13.5)
        .font(font::SEMIBOLD)
        .color(fade.color(Studio::TEXT_PRIMARY))
        .ellipsis(1)]
    .width(Length::Fill);
    if !t.tags.is_empty() {
        labels = labels.push(Space::new().height(2)).push(
            studio_text(t.tags.join(" · "))
                .size(11)
                .color(fade.color(Studio::TEXT_FAINT))
                .ellipsis(1),
        );
    }
    button(
        row![
            icons::icon(Icon::Waypoints, 13.0, fade.color(Studio::ACCENT)),
            Space::new().width(10),
            labels,
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding([9, 20])
    .style(move |t, s| theme::faded_button(theme::menu_item(t, s), fade.card))
    .on_press(Message::Pick(t.id.clone()))
    .into()
}

/// `_Hint`: a faint centred message, 20 / 28 padding.
fn hint(label: &str, fade: Fade) -> Element<'_, Message> {
    container(
        text(label)
            .size(12.5)
            .line_height(1.4)
            .align_x(Horizontal::Center)
            .width(Length::Fill)
            .color(fade.color(Studio::TEXT_FAINT)),
    )
    .width(Length::Fill)
    .padding([28, 20])
    .into()
}

/// `content` centred over the studio dialogs' dismissible barrier
/// (`Color(0xB0000000)`), as `confirm_dialog::modal` draws it: a tap on the
/// barrier cancels, the card takes its own taps, Escape cancels and no key
/// reaches what lies beneath. Both at `fade`.
fn modal(content: Element<'_, Message>, fade: Fade) -> Element<'_, Message> {
    let barrier = barrier(theme::DIALOG_BARRIER, fade.barrier, Some(Message::Cancel));
    let layers = stack![barrier, center(opaque(content))];
    Wrap::<Message, ()>::new(
        layers,
        Box::new(|_, event, _, _, shell| {
            if let Event::Keyboard(event) = event {
                if let keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                } = event
                {
                    shell.publish(Message::Cancel);
                }
                // The picker holds the keyboard (its search field has
                // already had its turn).
                shell.capture_event();
            }
        }),
    )
    .into()
}

#[cfg(test)]
mod tests;
