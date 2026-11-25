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

/// Draws `content` moved right by `dx`: a transformation, not a layout.
struct Shifted<'a> {
    content: Element<'a, ()>,
    dx: f32,
}

impl Widget<(), Theme, Renderer> for Shifted<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
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
        use iced::advanced::Renderer as _;
        renderer.with_translation(Vector::new(self.dx, 0.0), |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        });
    }
}

#[test]
fn transformations_go_on_working_around_and_inside_a_layer() {
    let around = Element::new(Shifted {
        content: opacity_layer(0.5, block(RED, 40.0, 40.0)).into(),
        dx: 40.0,
    });
    let inside: Element<'_, ()> = opacity_layer(
        0.5,
        Element::new(Shifted {
            content: block(RED, 40.0, 40.0),
            dx: 40.0,
        }),
    )
    .into();
    for view in [around, inside] {
        let mut window = Frames::new(WINDOW);
        window.frame(view, &[], Instant::now());
        assert_eq!(window.pixel(Point::new(20.0, 20.0)), BACKGROUND);
        assert!(near(window.pixel(Point::new(60.0, 20.0)), faded(RED, 0.5)));
        assert_eq!(window.pixel(Point::new(100.0, 20.0)), BACKGROUND);
    }
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

// ------------------------------------------------------ the clip layer

/// A diamond centered at `center`, `a` across and `b` down from its center
/// to its corners.
#[derive(Clone, Copy)]
struct Diamond {
    center: Point,
    a: f32,
    b: f32,
}

impl Diamond {
    fn path(self) -> iced::widget::canvas::Path {
        let Point { x, y } = self.center;
        iced::widget::canvas::Path::new(|p| {
            p.move_to(Point::new(x - self.a, y));
            p.line_to(Point::new(x, y - self.b));
            p.line_to(Point::new(x + self.a, y));
            p.line_to(Point::new(x, y + self.b));
            p.close();
        })
    }

    /// How far `point` lies inside its edge (negative: outside).
    fn depth(self, point: Point) -> f32 {
        let s = (point.x - self.center.x).abs() / self.a + (point.y - self.center.y).abs() / self.b;
        (1.0 - s) / (1.0 / (self.a * self.a) + 1.0 / (self.b * self.b)).sqrt()
    }
}

/// Stripes of three colors filling the window, then (`content`) a
/// translucent antialiased disc, a rounded box, a line of text and a faded
/// block, inside `clip` if any, all moved by `shift`.
struct ClipProbe {
    content: bool,
    clip: Option<Diamond>,
    shift: Vector,
}

impl Widget<(), Theme, Renderer> for ClipProbe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::graphics::geometry::Renderer as _;
        use iced::advanced::text::Renderer as _;
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        for (i, color) in [RED, GREY, BLUE].into_iter().enumerate() {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: bounds.width / 3.0 * i as f32,
                        width: bounds.width / 3.0,
                        ..bounds
                    },
                    ..renderer::Quad::default()
                },
                color,
            );
        }
        if !self.content {
            return;
        }
        let content = |renderer: &mut Renderer| {
            let mut frame = iced::widget::canvas::Frame::new(renderer, bounds.size());
            frame.fill(
                &iced::widget::canvas::Path::circle(Point::new(60.0, 40.0), 33.0),
                Color::from_rgba(0.9, 0.8, 0.1, 0.7),
            );
            renderer.draw_geometry(frame.into_geometry());
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(Point::new(70.5, 8.25), Size::new(30.0, 20.0)),
                    border: iced::Border {
                        radius: 6.0.into(),
                        width: 1.5,
                        color: Color::WHITE,
                    },
                    ..renderer::Quad::default()
                },
                Color::from_rgba(0.2, 0.9, 0.4, 0.8),
            );
            renderer.fill_text(
                iced::advanced::text::Text {
                    content: "Wgy".to_string(),
                    bounds: Size::new(80.0, 30.0),
                    size: 22.0.into(),
                    line_height: iced::advanced::text::LineHeight::default(),
                    font: theme::font::REGULAR,
                    align_x: iced::advanced::text::Alignment::Default,
                    align_y: iced::alignment::Vertical::Top,
                    shaping: iced::advanced::text::Shaping::Advanced,
                    wrapping: iced::advanced::text::Wrapping::None,
                },
                Point::new(14.0, 44.0),
                Color::WHITE,
                bounds,
            );
            let faded = Rectangle::new(Point::new(20.0, 10.0), Size::new(30.0, 25.0));
            renderer.with_opacity_layer(faded, 0.5, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: faded,
                        ..renderer::Quad::default()
                    },
                    Color::WHITE,
                );
            });
        };
        renderer.with_translation(self.shift, |renderer| match self.clip {
            Some(clip) => renderer.with_clip_layer(&clip.path(), content),
            None => content(renderer),
        });
    }
}

/// Every pixel of `view` drawn alone in a window.
fn drawn(view: ClipProbe) -> Vec<Color> {
    let mut window = Frames::new(WINDOW);
    window.frame(Element::new(view), &[], Instant::now());
    pixels(&window)
}

#[test]
fn a_clip_layer_draws_inside_as_without_it_and_leaves_the_rest_as_it_was() {
    // Flutter's `ClipPath` with `Clip.hardEdge`: Skia draws each thing
    // inside it, antialiased as without the clip, on the pixels whose
    // centers the clip holds, and nothing on the others. The diamond's
    // edges cut the disc, the box, the text and the faded block slantwise.
    let clip = Diamond {
        center: Point::new(58.3, 37.6),
        a: 41.7,
        b: 30.35,
    };
    for shift in [Vector::ZERO, Vector::new(7.0, -3.0)] {
        let probe = |content: bool, clip: Option<Diamond>| ClipProbe {
            content,
            clip,
            shift,
        };
        let backdrop = drawn(probe(false, None));
        let unclipped = drawn(probe(true, None));
        let clipped = drawn(probe(true, Some(clip)));
        let (mut cut, mut kept) = (0, 0);
        for (i, drawn) in clipped.iter().enumerate() {
            let (x, y) = (i % WINDOW.width as usize, i / WINDOW.width as usize);
            let center = Point::new(x as f32 + 0.5, y as f32 + 0.5) - shift;
            let depth = clip.depth(center);
            if depth.abs() < 0.02 {
                // On the edge, to within how the rasterizer rounds it.
                continue;
            }
            let inside = depth > 0.0;
            if inside {
                // As drawn without the clip: on wgpu exactly; tiny-skia
                // draws a shape its layer's box cuts (here the clip's box)
                // through a mask, which rounds to within one step.
                let [a, b] = [*drawn, unclipped[i]].map(Color::into_rgba8);
                assert!(
                    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 1),
                    "pixel ({x}, {y}), inside the clip by {depth:.3}: {a:?}, not \
                     {b:?} (shift {shift:?})"
                );
            } else {
                assert_eq!(
                    *drawn, backdrop[i],
                    "pixel ({x}, {y}), outside the clip by {:.3} (shift {shift:?})",
                    -depth
                );
            }
            if unclipped[i] != backdrop[i] {
                if inside {
                    kept += 1;
                } else {
                    cut += 1;
                }
            }
        }
        assert!(cut > 200 && kept > 1000, "cut {cut}, kept {kept}");
    }
}

#[test]
fn a_clip_layer_with_an_empty_clip_draws_nothing() {
    let empty = Diamond {
        center: Point::new(60.0, 40.0),
        a: 0.0,
        b: 0.0,
    };
    let probe = |content: bool, clip: Option<Diamond>| ClipProbe {
        content,
        clip,
        shift: Vector::ZERO,
    };
    assert_eq!(drawn(probe(true, Some(empty))), drawn(probe(false, None)));
}
