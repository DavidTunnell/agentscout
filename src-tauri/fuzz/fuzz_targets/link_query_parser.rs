#![no_main]
//! Fuzz target for the disposition-link query parser. Goal: any input
//! string either parses cleanly or returns an Err — should never panic
//! (out-of-bounds, integer overflow, alloc failure under malformed input).

use agentscout::email::link_signer::parse_token_from_query;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_token_from_query(s);
    }
});
