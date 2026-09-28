#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub use nanomp3_core::{Channels, Decoder, FrameInfo, Sample, MAX_SAMPLES_PER_FRAME};
