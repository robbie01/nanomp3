//! nanomp3 must agree bit-for-bit with upstream minimp3 on arbitrary input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3_parity::{decode_reference, decode_rust, first_difference, Feed, PcmSample};

fn diff<S: PcmSample>(input: &[u8], feed: Feed, compat: bool) -> Option<String> {
    first_difference(&decode_reference::<S>(input, feed, compat), &decode_rust::<S>(input, feed, compat))
}

fuzz_target!(|data: &[u8]| {
    let Some((&w, input)) = data.split_first() else { return };
    let feed = if w == 0 { Feed::Whole } else { Feed::Window(usize::from(w) * 64) };
    // The low bits of the window selector pick the output format.
    let diff = match w % 8 {
        0 | 1 => diff::<f32>(input, feed, false),
        2 => diff::<i16>(input, feed, true),
        3 => diff::<i16>(input, feed, false),
        4 => diff::<i8>(input, feed, false),
        5 => diff::<u8>(input, feed, false),
        6 => diff::<i32>(input, feed, false),
        _ => diff::<u32>(input, feed, false),
    };
    if let Some(diff) = diff {
        panic!("{diff}");
    }
});
