//! Fuzz target: for every input that parses, the printer round-trip must hold.
//!
//! Invariants checked on successful parses (the coverage-guided version of the
//! in-tree `adversarial.rs::exercise` checks):
//! - `parse(print(doc))` succeeds and is `structural_eq` to `doc`
//! - printing is a fixed point (`print(reparsed) == printed`)
//! - node/value IDs equal their arena indices (the documented pre-order contract)

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let Ok(document) = bevy_bsn::parse(&source) else {
        return;
    };

    for (index, node) in document.nodes.iter().enumerate() {
        assert_eq!(node.id.0 as usize, index, "node IDs must be arena indices");
    }
    for (index, value) in document.values.iter().enumerate() {
        assert_eq!(value.id.0 as usize, index, "value IDs must be arena indices");
    }

    let printed = bevy_bsn::print_document(&document);
    let reparsed = bevy_bsn::parse(&printed)
        .unwrap_or_else(|error| panic!("printed output failed to reparse: {error}\n{printed}"));
    assert!(
        document.structural_eq(&reparsed),
        "round trip changed the document:\n{printed}"
    );
    assert_eq!(
        bevy_bsn::print_document(&reparsed),
        printed,
        "printing is not a fixed point"
    );
});
