//! Colour emoji in every text, in the platform's own style.
//!
//! fastframe-emoji finds the platform's colour emoji font (Apple Color Emoji,
//! Segoe UI Emoji, the desktop's font on Linux) and its egui plugin, added
//! in [`crate::theme::install`], paints each emoji's picture over the
//! monochrome glyph egui laid out. Layout, truncation and right-to-left
//! reordering stay egui's own.
//!
//! Without a colour emoji font (a Linux desktop with none installed) the
//! bundled monochrome face draws the emoji, as it always has.

/// Chooses the platform's emoji font and warms it up before the first frame.
/// `synchronous` draws each picture in the frame that first shows it, for
/// demo captures that must show every emoji in their first frame.
pub fn install(synchronous: bool) {
    fastframe_emoji::EmojiSetup::default()
        .system(true)
        .synchronous(synchronous)
        .install();
    // Find and open the emoji fonts now, on this thread, so the first frame
    // with an emoji does not pay for it and the texture atlas does not grow
    // while the window is already up.
    fastframe_emoji::warm_up();
}
