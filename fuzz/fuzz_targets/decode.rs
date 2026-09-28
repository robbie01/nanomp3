//! Arbitrary input must never panic, and the decoder must always make progress.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};

fuzz_target!(|data: &[u8]| {
    // The first byte picks a read-ahead window so short-buffer paths get exercised.
    let Some((&w, mut input)) = data.split_first() else { return };
    let window = if w == 0 { usize::MAX } else { usize::from(w) * 64 };

    let mut decoder = Decoder::new();
    let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];
    while !input.is_empty() {
        let view = &input[..input.len().min(window)];
        let (consumed, info) = decoder.decode(view, &mut pcm);
        assert!(consumed > 0, "no progress on {} bytes", view.len());
        assert!(consumed <= view.len());
        if let Some(info) = info {
            assert!(info.samples_produced <= MAX_SAMPLES_PER_FRAME / info.channels.num() as usize);
            let out = &pcm[..info.samples_produced * info.channels.num() as usize];
            assert!(out.iter().all(|s| s.is_finite()), "non-finite sample");
        }
        input = &input[consumed..];
    }
});
