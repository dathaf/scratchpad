//! The two interface languages. Every text is written where it is used, as
//! `tr("Deutsch", "English")`; a single flag picks the side.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_english(on: bool) {
    ENGLISH.store(on, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn tr<'a>(german: &'a str, english_text: &'a str) -> &'a str {
    if english() { english_text } else { german }
}
