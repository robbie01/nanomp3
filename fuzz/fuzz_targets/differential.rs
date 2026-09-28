//! nanomp3 must agree bit-for-bit with upstream minimp3 on arbitrary input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3_parity::{decode_c, decode_rust, first_difference, Feed, Flavor};

fuzz_target!(|data: &[u8]| {
    let Some((&w, input)) = data.split_first() else { return };
    let feed = if w == 0 { Feed::Whole } else { Feed::Window(usize::from(w) * 64) };
    let c = decode_c(input, feed, Flavor::Scalar);
    let r = decode_rust(input, feed);
    if let Some(diff) = first_difference(&c, &r) {
        panic!("{diff}");
    }
});
