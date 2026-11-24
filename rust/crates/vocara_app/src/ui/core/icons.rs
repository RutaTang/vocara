//! Icons. forui's `FIcons` are the Lucide set, so `FIcons.fooBar` is
//! [`Icon::FooBar`] here (`FIcons.trash2` → `Icon::Trash2`,
//! `FIcons.triangleAlert` → `Icon::TriangleAlert`, `FIcons.chevronLeft` →
//! `Icon::ChevronLeft`, ...). The shared widgets take an [`Icon`] wherever the
//! Dart ones took an `IconData`.

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::Text;
use iced::{Color, Font};

pub use lucide_icons::Icon;

/// The Lucide font's family name, as `lucide-icons` registers it.
pub const FONT: Font = Font::with_name("lucide");

/// The Lucide font bytes, to load at startup.
pub const FONT_BYTES: &[u8] = lucide_icons::LUCIDE_FONT_BYTES;

/// `Icon(icon, size: size, color: color)`: the glyph centred in a
/// `size` × `size` box.
pub fn icon<'a>(icon: Icon, size: f32, color: Color) -> Text<'a> {
    text(char::from(icon).to_string())
        .font(FONT)
        .size(size)
        .line_height(iced::widget::text::LineHeight::Relative(1.0))
        .width(size)
        .height(size)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FIcons.fooBar2` → `foo-bar-2`, the Lucide name.
    fn kebab(camel: &str) -> String {
        let mut out = String::new();
        let mut prev_digit = false;
        for (i, c) in camel.chars().enumerate() {
            let digit = c.is_ascii_digit();
            if i > 0 && (c.is_ascii_uppercase() || (digit && !prev_digit)) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev_digit = digit;
        }
        out
    }

    #[test]
    fn every_ficon_the_dart_ui_uses_is_a_lucide_icon() {
        // Every `FIcons.x` in client/lib (grep -rhoE "FIcons\.[a-zA-Z0-9]+").
        let used = [
            "trash2",
            "plus",
            "pencil",
            "waypoints",
            "triangleAlert",
            "stickyNote",
            "mic",
            "download",
            "chevronLeft",
            "chevronDown",
            "wandSparkles",
            "square",
            "refreshCw",
            "play",
            "music2",
            "check",
            "wrench",
            "video",
            "search",
            "rotateCw",
            "pause",
            "music",
            "loader",
            "library",
            "headphones",
            "fileUp",
            "copy",
            "chevronRight",
            "zoomOut",
            "zoomIn",
            "x",
            "waves",
            "timer",
            "target",
            "tag",
            "speaker",
            "settings",
            "scrollText",
            "scissors",
            "repeat",
            "quote",
            "panelRightOpen",
            "panelRightClose",
            "panelLeftOpen",
            "panelLeftClose",
            "panelBottomOpen",
            "panelBottomClose",
            "minimize2",
            "maximize2",
            "locateFixed",
            "info",
            "gripVertical",
            "fileX",
            "filePlus",
            "fileDown",
            "ellipsis",
            "drama",
            "disc3",
            "combine",
            "clipboard",
            "circleArrowUp",
        ];
        for name in used {
            let lucide = kebab(name);
            assert!(
                Icon::try_from(lucide.as_str()).is_ok(),
                "FIcons.{name} ({lucide}) is not in lucide-icons"
            );
        }
        assert_eq!(kebab("trash2"), "trash-2");
        assert_eq!(kebab("triangleAlert"), "triangle-alert");
    }
}
