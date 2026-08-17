//! Fuzz target: error rendering must never panic, whatever the source and span.
//!
//! `BsnParseError::render` slices the source to quote the offending line and draw a
//! caret; the fuzzer hunts for char-boundary and off-the-end mistakes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    if let Err(error) = bevy_bsn::parse(&source) {
        let _ = error.render(&source, Some("fuzz.bsn"));
        let _ = error.render(&source, None);
        // Rendering against a *different* source than the error came from must also be
        // memory-safe (spans may be out of bounds); split the input to simulate it.
        let mut mid = source.len() / 2;
        while !source.is_char_boundary(mid) {
            mid -= 1;
        }
        let (a, b) = source.split_at(mid);
        let _ = error.render(a, None);
        let _ = error.render(b, None);
    }
});
