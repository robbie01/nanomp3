//! Arbitrary input must never panic, and the decoder must always make progress.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3::{DecodeError, Decoder, MAX_SAMPLES_PER_FRAME};

fuzz_target!(|data: &[u8]| {
    // The first byte picks a read-ahead window so short-buffer paths get exercised.
    let Some((&w, mut input)) = data.split_first() else { return };
    let window = if w == 0 { usize::MAX } else { usize::from(w) * 64 };

    let mut decoder = Decoder::new();
    let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];
    while !input.is_empty() {
        let view = &input[..input.len().min(window)];
        let (consumed, result) = decoder.decode(view, &mut pcm);
        assert!(consumed > 0, "no progress on {} bytes", view.len());
        assert!(consumed <= view.len());
        match result {
            // Documented: NoFrame means the whole input was skipped.
            Err(DecodeError::NoFrame) => assert_eq!(consumed, view.len(), "NoFrame left input unconsumed"),
            Err(e) => {
                let info = e.frame_info().expect("frame errors carry frame info");
                assert_eq!(info.samples_produced, 0);
                assert!((1..=3).contains(&info.layer));
            }
            Ok(_) => {}
        }
        if let Ok(info) = result {
            assert!(info.samples_produced > 0);
            assert!((1..=3).contains(&info.layer));
            assert!(info.samples_produced <= MAX_SAMPLES_PER_FRAME / info.channels.num() as usize);
            let out = &pcm[..info.samples_produced * info.channels.num() as usize];
            assert!(out.iter().all(|s| s.is_finite()), "non-finite sample");
        }
        input = &input[consumed..];
    }
});
