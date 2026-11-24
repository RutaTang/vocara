//! Tab and Shift+Tab through a scope's fields.

use iced::advanced::clipboard;
use iced::advanced::renderer::Headless;
use iced::event;
use iced::keyboard::{self, key::Named};
use iced::time::Instant;
use iced::widget::{column, container, scrollable, stack, Space};
use iced::{window, Point};
use iced_test::runtime::user_interface::{self, UserInterface};

use super::*;
use crate::ui::core::theme::{font, DEFAULT_TEXT_SIZE};
use crate::ui::core::widgets::studio_text::testing::load_fonts;
use crate::ui::core::widgets::text_field::{text_editor, text_input, Content};

#[derive(Debug, Clone, PartialEq)]
enum Msg {
    Input(usize, String),
    Edit,
}

/// A window running a view as the app does, its pointer outside it.
struct Window {
    renderer: Renderer,
    cache: user_interface::Cache,
    size: Size,
}

impl Window {
    fn new() -> Self {
        load_fonts();
        let renderer = iced_test::futures::futures::executor::block_on(
            <Renderer as Headless>::new(font::REGULAR, DEFAULT_TEXT_SIZE.into(), None),
        )
        .expect("a headless renderer");
        Self {
            renderer,
            cache: user_interface::Cache::default(),
            size: Size::new(400.0, 300.0),
        }
    }

    fn ui<'a>(&mut self, view: Element<'a, Msg>) -> UserInterface<'a, Msg, Theme, Renderer> {
        UserInterface::build(
            view,
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        )
    }

    /// Feeds `view` `events` and a frame: what it published, and whether
    /// each event was captured.
    fn run<'a>(
        &mut self,
        view: Element<'a, Msg>,
        events: &[Event],
    ) -> (Vec<Msg>, Vec<event::Status>) {
        let mut ui = self.ui(view);
        let mut messages = Vec::new();
        let cursor = mouse::Cursor::Available(Point::new(-10.0, -10.0));
        let (_, statuses) = ui.update(
            events,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let _ = ui.update(
            &[Event::Window(
                window::Event::RedrawRequested(Instant::now()),
            )],
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        self.cache = ui.into_cache();
        (messages, statuses)
    }

    /// Runs `operation` over `view`.
    fn operate<'a>(&mut self, view: Element<'a, Msg>, operation: &mut dyn Operation) {
        let mut ui = self.ui(view);
        ui.operate(&self.renderer, operation);
        self.cache = ui.into_cache();
    }

    /// Which of the fields in `view` has the focus, counting them all.
    fn focused<'a>(&mut self, view: Element<'a, Msg>) -> Option<usize> {
        let mut survey = Survey::default();
        self.operate(view, &mut survey);
        survey.fields.iter().position(|(_, focused, _)| *focused)
    }
}

fn tab(shift: bool) -> Event {
    let key = keyboard::Key::Named(Named::Tab);
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: if shift {
            keyboard::Modifiers::SHIFT
        } else {
            keyboard::Modifiers::default()
        },
        repeat: false,
        text: None,
    })
}

fn typed(text: &str) -> Event {
    iced_test::simulator::press_key(keyboard::Key::Character(text.into()), Some(text.into()))
}

/// A single-line field `index` holding `value`, editable or not.
fn line(index: usize, value: &str, enabled: bool) -> Element<'_, Msg> {
    let field = text_input("", value).size(14).line_height(1.0);
    if enabled {
        field.on_input(move |text| Msg::Input(index, text)).into()
    } else {
        field.into()
    }
}

/// Three single-line fields, the middle one disabled or not, in a scope.
fn form<'a>(middle_enabled: bool) -> Element<'a, Msg> {
    focus_scope(column![
        line(0, "alpha", true),
        line(1, "beta", middle_enabled),
        line(2, "gamma", true),
    ])
}

#[test]
fn tab_moves_through_the_fields_and_goes_round_shift_tab_back() {
    let mut window = Window::new();
    assert_eq!(window.focused(form(true)), None);
    // Nothing focused: Tab takes the first, Shift+Tab the last.
    let (_, statuses) = window.run(form(true), &[tab(false)]);
    assert_eq!(statuses, vec![event::Status::Captured], "the scope's key");
    assert_eq!(window.focused(form(true)), Some(0));
    window.run(form(true), &[tab(false)]);
    assert_eq!(window.focused(form(true)), Some(1));
    window.run(form(true), &[tab(false)]);
    assert_eq!(window.focused(form(true)), Some(2));
    window.run(form(true), &[tab(false)]);
    assert_eq!(window.focused(form(true)), Some(0), "round to the first");
    window.run(form(true), &[tab(true)]);
    assert_eq!(
        window.focused(form(true)),
        Some(2),
        "back round to the last"
    );
    window.run(form(true), &[tab(true)]);
    assert_eq!(window.focused(form(true)), Some(1));

    let mut window = Window::new();
    window.run(form(true), &[tab(true)]);
    assert_eq!(window.focused(form(true)), Some(2), "Shift+Tab: the last");
}

#[test]
fn a_disabled_field_takes_no_focus() {
    let mut window = Window::new();
    window.run(form(false), &[tab(false)]);
    assert_eq!(window.focused(form(false)), Some(0));
    window.run(form(false), &[tab(false)]);
    assert_eq!(
        window.focused(form(false)),
        Some(2),
        "past the disabled one"
    );
}

#[test]
fn a_single_line_field_tab_lands_on_selects_all_its_text() {
    let mut window = Window::new();
    window.run(form(true), &[tab(false), tab(false)]);
    // Typing replaces the whole text.
    let (messages, _) = window.run(form(true), &[typed("x")]);
    assert_eq!(messages, vec![Msg::Input(1, "x".into())]);
}

#[test]
fn tab_with_the_only_field_focused_keeps_it_as_it_was() {
    let mut window = Window::new();
    let view = || focus_scope(column![line(0, "alpha", true)]);
    window.run(view(), &[tab(false)]);
    // Its text selected; a key moves the caret to its end.
    window.run(
        view(),
        &[iced_test::simulator::press_key(
            keyboard::Key::Named(Named::ArrowRight),
            None,
        )],
    );
    // Tab again: the focus stays, nothing is selected anew.
    let (_, statuses) = window.run(view(), &[tab(false)]);
    assert_eq!(statuses, vec![event::Status::Captured]);
    let (messages, _) = window.run(view(), &[typed("x")]);
    assert_eq!(messages, vec![Msg::Input(0, "alphax".into())]);
}

#[test]
fn a_multi_line_field_keeps_its_selection_and_counts_in_the_order() {
    fn view(content: &Content) -> Element<'_, Msg> {
        focus_scope(column![
            line(0, "alpha", true),
            text_editor(content).on_action(|_| Msg::Edit),
        ])
    }
    let content = Content::with_text("prose");
    let mut window = Window::new();
    window.run(view(&content), &[tab(false), tab(false)]);
    assert_eq!(window.focused(view(&content)), Some(1));
    // Typing edits at its caret (the start of a fresh content), with
    // nothing selected to replace.
    let (messages, _) = window.run(view(&content), &[typed("x")]);
    assert_eq!(messages, vec![Msg::Edit]);
    assert_eq!(content.text(), "prose", "the owner applies the edit");
}

#[test]
fn an_inner_scope_keeps_tab_to_itself() {
    // A page's fields under a dialog with one of its own.
    let view = || -> Element<'_, Msg> {
        focus_scope(stack![
            column![line(0, "page", true), line(1, "page too", true)],
            focus_scope(container(line(2, "dialog", true)).padding(100)),
        ])
    };
    let mut window = Window::new();
    window.run(view(), &[tab(false)]);
    assert_eq!(window.focused(view()), Some(2), "the dialog's field");
    window.run(view(), &[tab(false)]);
    assert_eq!(window.focused(view()), Some(2), "round within the dialog");
}

/// Reads the first scrollable's offset down.
#[derive(Default)]
struct Offset(Option<f32>);

impl Operation for Offset {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert(translation.y);
    }
}

#[test]
fn the_view_scrolls_to_show_the_field_tab_lands_on() {
    // A 100 px view over three fields 150 px apart (14 px text boxes, no
    // padding).
    let view = || -> Element<'_, Msg> {
        let field = |i: usize| line(i, "text", true);
        focus_scope(
            scrollable(column![
                field(0),
                Space::new().height(150),
                field(1),
                Space::new().height(150),
                field(2),
            ])
            .height(100),
        )
    };
    let mut window = Window::new();
    let offset = |window: &mut Window| {
        let mut read = Offset::default();
        window.operate(view(), &mut read);
        read.0.expect("a scrollable")
    };
    let pad = text_field::input::DEFAULT_PADDING;
    let line = 14.0;
    let field_height = line + pad.top + pad.bottom;
    window.run(view(), &[tab(false)]);
    assert_eq!(offset(&mut window), 0.0, "in view already");
    // Forward: the second field's text box ends at the view's bottom.
    window.run(view(), &[tab(false)]);
    let box_bottom = field_height + 150.0 + pad.top + line;
    assert_eq!(offset(&mut window), box_bottom - 100.0);
    window.run(view(), &[tab(false)]);
    let box_bottom = 2.0 * (field_height + 150.0) + pad.top + line;
    assert_eq!(offset(&mut window), box_bottom - 100.0);
    // Back: the second's text box starts at the view's top.
    window.run(view(), &[tab(true)]);
    assert_eq!(offset(&mut window), field_height + 150.0 + pad.top);
}

use crate::ui::core::widgets::text_field;
