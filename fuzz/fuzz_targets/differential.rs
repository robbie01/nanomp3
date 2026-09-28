//! nanomp3 must agree bit-for-bit with upstream minimp3 on arbitrary input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3_parity::{decode_c, decode_rust, first_difference, Feed, Flavor};

fuzz_target!(|data: &[u8]| {
    let Some((&w, input)) = data.split_first() else { return };
    let feed = if w == 0 { Feed::Whole } else { Feed::Window(usize::from(w) * 64) };
    // Alternate sample formats on the low bit of the window selector.
    let diff = if w & 1 == 0 {
        first_difference(&decode_c::<f32>(input, feed, Flavor::reference::<f32>()), &decode_rust::<f32>(input, feed))
    } else {
        first_difference(&decode_c::<i16>(input, feed, Flavor::reference::<i16>()), &decode_rust::<i16>(input, feed))
    };
    if let Some(diff) = diff {
        panic!("{diff}");
    }
});
