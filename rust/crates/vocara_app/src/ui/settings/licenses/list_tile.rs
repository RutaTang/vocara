//! A two-line `ListTile`'s layout (`_RenderListTile._computeSizes`) at the
//! desktop's `VisualDensity.compact`, for a title in `bodyLarge` (16 px)
//! over a subtitle in `bodyMedium` (14 px), both at forui's line height 1.
//!
//! The tile is `72 + baseSizeAdjustment.dy` = 64 tall. The title's
//! baseline sits 32 down and the subtitle's `52 + density.vertical · 2` =
//! 48 down, each text placed so its first line's alphabetic baseline lands
//! there; were they to overlap (a title wrapped onto a second line) they
//! move apart by the same distance, and when that leaves less than the
//! 8 px `minVerticalPadding` above or below, the tile grows to hold them
//! 8 px apart from its edges ("compact mode"). Content is inset 16 at the
//! start and 24 at the end (`contentPadding`).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{renderer, Clipboard, Shell, Widget};
use iced::{mouse, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme};

/// Inter's ascent as a share of its line at height 1 (`hhea` ascender 2728
/// over ascender + descender 2728 + 680): where Flutter puts the first
/// line's baseline in a line exactly the font size tall (`height: 1`,
/// proportional leading).
pub const INTER_ASCENT: f32 = 2728.0 / 3408.0;

/// The title's and the subtitle's font sizes (`bodyLarge`, `bodyMedium`).
pub const TITLE_SIZE: f32 = 16.0;
pub const SUBTITLE_SIZE: f32 = 14.0;

/// `contentPadding`: `EdgeInsetsDirectional.only(start: 16, end: 24)`.
const START: f32 = 16.0;
const END: f32 = 24.0;

/// `VisualDensity.compact.vertical`.
const DENSITY: f32 = -2.0;

/// The two-line tile's height: 72, less 8 for the compact density.
pub const TILE_HEIGHT: f32 = 72.0 + DENSITY * 4.0;

/// `minVerticalPadding` (Material 3's).
const MIN_VERTICAL_PADDING: f32 = 8.0;

/// Where a title `title_height` tall and a subtitle `subtitle_height` tall
/// go: the title's top, the subtitle's top and the tile's height.
pub fn two_line(title_height: f32, subtitle_height: f32) -> (f32, f32, f32) {
    let title_baseline = TITLE_SIZE * INTER_ASCENT;
    let subtitle_baseline = SUBTITLE_SIZE * INTER_ASCENT;
    let target_title_y = 32.0 - title_baseline;
    let target_subtitle_y = 52.0 + DENSITY * 2.0 - subtitle_baseline;
    let half_overlap = (target_title_y + title_height - target_subtitle_y).max(0.0) / 2.0;
    let title_y = target_title_y - half_overlap;
    let subtitle_y = target_subtitle_y + half_overlap;
    let compact = title_y < MIN_VERTICAL_PADDING
        || subtitle_y + subtitle_height + MIN_VERTICAL_PADDING > TILE_HEIGHT;
    if compact {
        (
            MIN_VERTICAL_PADDING,
            MIN_VERTICAL_PADDING + title_height,
            2.0 * MIN_VERTICAL_PADDING + title_height + subtitle_height,
        )
    } else {
        (title_y, subtitle_y, TILE_HEIGHT)
    }
}

/// See the module docs: `title` over `subtitle`, filling the width.
pub struct TwoLine<'a, M> {
    title: Element<'a, M>,
    subtitle: Element<'a, M>,
}

/// See [`TwoLine`].
pub fn two_line_tile<'a, M>(
    title: impl Into<Element<'a, M>>,
    subtitle: impl Into<Element<'a, M>>,
) -> TwoLine<'a, M> {
    TwoLine {
        title: title.into(),
        subtitle: subtitle.into(),
    }
}

impl<M> Widget<M, Theme, Renderer> for TwoLine<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.title), Tree::new(&self.subtitle)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.title, &self.subtitle]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width;
        let text = layout::Limits::new(
            Size::ZERO,
            Size::new((width - START - END).max(0.0), f32::INFINITY),
        );
        let title = self
            .title
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &text);
        let subtitle = self
            .subtitle
            .as_widget_mut()
            .layout(&mut tree.children[1], renderer, &text);
        let (title_y, subtitle_y, height) = two_line(title.size().height, subtitle.size().height);
        layout::Node::with_children(
            Size::new(width, height),
            vec![
                title.move_to(Point::new(START, title_y)),
                subtitle.move_to(Point::new(START, subtitle_y)),
            ],
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        for ((child, tree), layout) in [&mut self.title, &mut self.subtitle]
            .into_iter()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
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
        for ((child, tree), layout) in [&self.title, &self.subtitle]
            .into_iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, tree), layout) in [&mut self.title, &mut self.subtitle]
                .into_iter()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
        });
    }
}

impl<'a, M: 'a> From<TwoLine<'a, M>> for Element<'a, M> {
    fn from(tile: TwoLine<'a, M>) -> Self {
        Element::new(tile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_line_tile_is_64_tall_its_baselines_at_32_and_48() {
        let (title, subtitle, height) = two_line(TITLE_SIZE, SUBTITLE_SIZE);
        assert_eq!(height, 64.0);
        assert!((title - 19.193).abs() < 1e-3, "{title}");
        assert!((subtitle - 36.793).abs() < 1e-3, "{subtitle}");
        assert!((title + TITLE_SIZE * INTER_ASCENT - 32.0).abs() < 1e-4);
        assert!((subtitle + SUBTITLE_SIZE * INTER_ASCENT - 48.0).abs() < 1e-4);
    }

    #[test]
    fn a_wrapped_title_puts_the_tile_in_compact_mode() {
        // Two lines of title: moved apart, the subtitle would end under the
        // tile's bottom padding, so both sit 8 from the edges.
        let (title, subtitle, height) = two_line(2.0 * TITLE_SIZE, SUBTITLE_SIZE);
        assert_eq!((title, subtitle), (8.0, 40.0));
        assert_eq!(height, 8.0 + 32.0 + 14.0 + 8.0);
    }
}
