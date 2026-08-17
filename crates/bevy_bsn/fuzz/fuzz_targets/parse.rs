//! Fuzz target: `bevy_bsn::parse` must never panic or hang on arbitrary bytes.
//!
//! `.bsn` files are untrusted user input loaded at runtime by a game engine, so any
//! reachable panic from file bytes is a real bug.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Only valid UTF-8 reaches the parser in production (the loader decodes first),
    // but lossy conversion lets the fuzzer explore boundary bytes cheaply too.
    let source = String::from_utf8_lossy(data);
    let _ = bevy_bsn::parse(&source);
});
