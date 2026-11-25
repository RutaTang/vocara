use iced::time::Instant;
use iced::widget::{column, container, image, stack, text, Space};
use iced::{Color, Element, Length, Point, Size};

use super::*;
use crate::ui::core::theme;
use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND};

const RED: Color = Color::from_rgb(1.0, 0.0, 0.0);
const BLUE: Color = Color::from_rgb(0.0, 0.0, 1.0);
const GREY: Color = Color::from_rgb(0.4, 0.4, 0.4);

const WINDOW: Size = Size::new(120.0, 80.0);

/// A `width` × `height` box filled with `color`.
fn block<'a>(color: Color, width: f32, height: f32) -> Element<'a, ()> {
    container(Space::new().width(width).height(height))
        .style(theme::fill(color))
        .into()
}

/// `color` at `opacity` over the window's background.
fn faded(color: Color, opacity: f32) -> Color {
    over(
        Color {
            a: opacity,
            ..color
        },
        BACKGROUND,
    )
}

/// Every pixel of the last frame, row by row.
fn pixels(window: &Frames) -> Vec<Color> {
    let mut all = Vec::new();
    for y in 0..WINDOW.height as u32 {
        for x in 0..WINDOW.width as u32 {
            all.push(window.pixel(Point::new(x as f32, y as f32)));
        }
    }
    all
}

/// Draws `view` at full opacity and at `opacity`, and checks that every
/// pixel of the second is the first's at `opacity` over the background:
/// the subtree composited as one layer, whatever it holds.
fn composites_as_one_layer(view: impl Fn(f32) -> Element<'static, ()>, opacity: f32) {
    let mut window = Frames::new(WINDOW);
    let now = Instant::now();
    window.frame(view(1.0), &[], now);
    let full = pixels(&window);
    window.frame(view(opacity), &[], now);
    let layered = pixels(&window);
    let mut inked = 0;
    for (i, (full, layered)) in full.iter().zip(&layered).enumerate() {
        let expected = over(
            Color {
                a: opacity,
                ..*full
            },
            BACKGROUND,
        );
        assert!(
            near(*layered, expected),
            "pixel ({}, {}): {layered:?}, not {expected:?} ({full:?} at {opacity})",
            i % WINDOW.width as usize,
            i / WINDOW.width as usize,
        );
        if *full != BACKGROUND {
            inked += 1;
        }
    }
    assert!(inked > 100, "the view drew something ({inked} pixels)");
}

#[test]
fn overlapping_parts_composite_as_one_layer_not_each_faded() {
    // A 60 × 40 blue box, a red one over its right 40, in one layer (the
    // outer box's fill, then the inner's) and in two (a stack draws its
    // second child in a layer of its own).
    let one_layer = |opacity: f32| -> Element<'static, ()> {
        opacity_layer(
            opacity,
            container(block(RED, 40.0, 40.0))
                .padding(iced::Padding::ZERO.left(20.0))
                .style(theme::fill(BLUE)),
        )
        .into()
    };
    let two_layers = |opacity: f32| -> Element<'static, ()> {
        opacity_layer(
            opacity,
            stack![
                block(BLUE, 60.0, 40.0),
                container(block(RED, 40.0, 40.0)).padding(iced::Padding::ZERO.left(20.0)),
            ],
        )
        .into()
    };
    for view in [
        &one_layer as &dyn Fn(f32) -> Element<'static, ()>,
        &two_layers,
    ] {
        let mut window = Frames::new(WINDOW);
        window.frame(view(0.5), &[], Instant::now());
        // Where the red box covers the blue one, only red shows, at half
        // opacity: not red at half over blue at half.
        for x in [30.0, 50.0] {
            let overlap = window.pixel(Point::new(x, 20.0));
            assert!(near(overlap, faded(RED, 0.5)), "{overlap:?}");
            let blended = over(Color { a: 0.5, ..RED }, faded(BLUE, 0.5));
            assert!(!near(overlap, blended), "{overlap:?}");
        }
        assert!(near(window.pixel(Point::new(10.0, 20.0)), faded(BLUE, 0.5)));
        assert_eq!(window.pixel(Point::new(100.0, 20.0)), BACKGROUND);
    }
}

#[test]
fn text_and_quads_fade_as_the_full_drawing_does() {
    let view = |opacity: f32| -> Element<'static, ()> {
        opacity_layer(
            opacity,
            column![
                container(text("Hg fade").size(24).color(Color::WHITE)).style(theme::fill(GREY)),
                text("over nothing")
                    .size(16)
                    .color(Color::from_rgb(0.2, 0.9, 0.5)),
            ],
        )
        .into()
    };
    composites_as_one_layer(view, 0.5);
    composites_as_one_layer(view, 0.3);
}

#[test]
fn an_image_fades_as_the_full_drawing_does() {
    // A 16 × 16 image, each pixel its own color, opaque but for a
    // translucent column, over a fill.
    let mut rgba = Vec::new();
    for y in 0..16_u8 {
        for x in 0..16_u8 {
            let alpha = if x == 3 { 128 } else { 255 };
            rgba.extend([x * 16, y * 16, 200, alpha]);
        }
    }
    let handle = image::Handle::from_rgba(16, 16, rgba);
    let view = move |opacity: f32| -> Element<'static, ()> {
        opacity_layer(
            opacity,
            container(image(handle.clone()).width(32).height(32))
                .padding(8)
                .style(theme::fill(GREY)),
        )
        .into()
    };
    composites_as_one_layer(view, 0.5);
}

#[test]
fn nested_layers_multiply_and_clips_inside_go_on_clipping() {
    // A layer at half inside a layer at half: a quarter. Inside, a box
    // clipped to half its width by its container.
    let mut window = Frames::new(WINDOW);
    let view = opacity_layer(
        0.5,
        column![
            opacity_layer(0.5, block(RED, 40.0, 20.0)),
            container(block(BLUE, 80.0, 20.0)).width(40).clip(true),
        ],
    );
    window.frame(view, &[], Instant::now());
    assert!(near(window.pixel(Point::new(20.0, 10.0)), faded(RED, 0.25)));
    assert!(near(window.pixel(Point::new(20.0, 30.0)), faded(BLUE, 0.5)));
    assert_eq!(window.pixel(Point::new(60.0, 30.0)), BACKGROUND);
}

#[test]
fn at_full_opacity_it_is_drawn_as_it_is_and_at_none_not_at_all() {
    let mut window = Frames::new(WINDOW);
    let now = Instant::now();
    window.frame(opacity_layer(1.0, block(RED, 40.0, 40.0)), &[], now);
    assert_eq!(window.pixel(Point::new(20.0, 20.0)), RED);
    window.frame(opacity_layer(0.0, block(RED, 40.0, 40.0)), &[], now);
    assert_eq!(window.pixel(Point::new(20.0, 20.0)), BACKGROUND);
}

#[test]
fn what_it_draws_past_its_box_fades_with_it() {
    // A box with a shadow below it: the shadow, past the box, is in the
    // layer too.
    let mut window = Frames::new(WINDOW);
    let now = Instant::now();
    let view = |opacity: f32| -> Element<'static, ()> {
        container(opacity_layer(
            opacity,
            container(Space::new().width(40).height(20)).style(|_: &iced::Theme| {
                container::Style {
                    background: Some(GREY.into()),
                    shadow: iced::Shadow {
                        color: Color::WHITE,
                        offset: iced::Vector::new(0.0, 10.0),
                        blur_radius: 0.0,
                    },
                    ..container::Style::default()
                }
            }),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    };
    window.frame(view(1.0), &[], now);
    let shadow = window.pixel(Point::new(20.0, 25.0));
    assert!(near(shadow, Color::WHITE), "{shadow:?}");
    window.frame(view(0.5), &[], now);
    assert!(near(
        window.pixel(Point::new(20.0, 25.0)),
        faded(Color::WHITE, 0.5)
    ));
}

#[test]
fn a_layer_inside_a_clip_stays_inside_it() {
    // A page half slid out of its 60-wide box, which clips it in a layer
    // of its own (handing its content the window as viewport): the faded
    // block inside goes no further than the box.
    let mut window = Frames::new(WINDOW);
    let page = crate::ui::core::widgets::slide::slide(
        container(opacity_layer(0.5, block(RED, 60.0, 40.0))).width(60),
        0.5,
    );
    window.frame(page, &[], Instant::now());
    assert_eq!(window.pixel(Point::new(10.0, 20.0)), BACKGROUND);
    assert!(near(window.pixel(Point::new(45.0, 20.0)), faded(RED, 0.5)));
    assert_eq!(window.pixel(Point::new(75.0, 20.0)), BACKGROUND);
}
