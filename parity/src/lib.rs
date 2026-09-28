//! Test/bench-only bindings to upstream minimp3, used as the reference that
//! nanomp3 must match bit for bit. This crate is never published; `unsafe`
//! here is confined to the FFI boundary.

use std::os::raw::{c_int, c_ulong};

#[repr(C)]
struct Mp3Dec {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: c_int,
    free_format_bytes: c_int,
    header: [u8; 4],
    reserv_buf: [u8; 511],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame_bytes: c_int,
    pub frame_offset: c_int,
    pub channels: c_int,
    pub hz: c_int,
    pub layer: c_int,
    pub bitrate_kbps: c_int,
}

type DecodeFn =
    unsafe extern "C" fn(*mut Mp3Dec, *const u8, c_int, *mut f32, *mut FrameInfo) -> c_int;

extern "C" {
    fn mp3dec_init_scalar(dec: *mut Mp3Dec);
    fn mp3dec_decode_frame_scalar(
        dec: *mut Mp3Dec,
        mp3: *const u8,
        mp3_bytes: c_int,
        pcm: *mut f32,
        info: *mut FrameInfo,
    ) -> c_int;
    fn mp3dec_sizeof_scalar() -> c_ulong;

    fn mp3dec_init_simd(dec: *mut Mp3Dec);
    fn mp3dec_decode_frame_simd(
        dec: *mut Mp3Dec,
        mp3: *const u8,
        mp3_bytes: c_int,
        pcm: *mut f32,
        info: *mut FrameInfo,
    ) -> c_int;
    fn mp3dec_sizeof_simd() -> c_ulong;
}

/// Which build of upstream minimp3 to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `MINIMP3_NO_SIMD`: the exact configuration nanomp3 was translated from.
    Scalar,
    /// Upstream defaults (SSE2/NEON intrinsics). Not bit-exact; used for speed.
    Simd,
}

/// A C minimp3 decoder instance.
pub struct CDecoder {
    dec: Box<Mp3Dec>,
    decode: DecodeFn,
}

impl CDecoder {
    pub fn new(flavor: Flavor) -> Self {
        // SAFETY: Mp3Dec is plain-old-data; all-zero is its documented initial
        // state (mp3dec_init only clears header[0], which zeroing covers).
        let mut dec: Box<Mp3Dec> = unsafe { Box::new(std::mem::zeroed()) };
        let (init, decode, size): (unsafe extern "C" fn(*mut Mp3Dec), DecodeFn, _) = match flavor {
            Flavor::Scalar => (
                mp3dec_init_scalar,
                mp3dec_decode_frame_scalar,
                unsafe { mp3dec_sizeof_scalar() },
            ),
            Flavor::Simd => (mp3dec_init_simd, mp3dec_decode_frame_simd, unsafe {
                mp3dec_sizeof_simd()
            }),
        };
        assert_eq!(size as usize, std::mem::size_of::<Mp3Dec>(), "mp3dec_t layout mismatch");
        // SAFETY: `dec` is a valid, exclusively owned mp3dec_t.
        unsafe { init(&mut *dec) };
        Self { dec, decode }
    }

    /// Mirrors `mp3dec_decode_frame`. Returns (samples per channel, info).
    pub fn decode(&mut self, mp3: &[u8], pcm: &mut [f32]) -> (usize, FrameInfo) {
        assert!(pcm.len() >= nanomp3::MAX_SAMPLES_PER_FRAME);
        let len = c_int::try_from(mp3.len()).expect("input too large for C API");
        let mut info = FrameInfo::default();
        // SAFETY: pointers are valid for the lengths passed, pcm holds at least
        // MINIMP3_MAX_SAMPLES_PER_FRAME floats, and dec is exclusively borrowed.
        let samples =
            unsafe { (self.decode)(&mut *self.dec, mp3.as_ptr(), len, pcm.as_mut_ptr(), &mut info) };
        (samples as usize, info)
    }
}

/// One decode call's observable result, in a form both decoders can produce.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Bytes of input consumed.
    pub consumed: usize,
    /// (samples per channel, channels, hz, kbps) when audio was produced.
    pub info: Option<(usize, u8, u32, u32)>,
    /// Interleaved PCM, only the produced part.
    pub pcm: Vec<f32>,
}

/// How to present input to the decoder.
#[derive(Debug, Clone, Copy)]
pub enum Feed {
    /// Pass everything that remains (the whole file is in memory).
    Whole,
    /// Pass at most this many bytes at a time, like a streaming read-ahead buffer.
    Window(usize),
}

fn run(mut input: &[u8], feed: Feed, mut step: impl FnMut(&[u8], &mut [f32]) -> Frame) -> Vec<Frame> {
    let mut pcm = vec![0f32; nanomp3::MAX_SAMPLES_PER_FRAME];
    let mut frames = Vec::new();
    while !input.is_empty() {
        let view = match feed {
            Feed::Whole => input,
            Feed::Window(n) => &input[..input.len().min(n)],
        };
        let frame = step(view, &mut pcm);
        if frame.consumed == 0 {
            break;
        }
        input = &input[frame.consumed.min(input.len())..];
        frames.push(frame);
    }
    frames
}

/// Decode `input` with C minimp3.
pub fn decode_c(input: &[u8], feed: Feed, flavor: Flavor) -> Vec<Frame> {
    let mut dec = CDecoder::new(flavor);
    run(input, feed, |mp3, pcm| {
        let (samples, info) = dec.decode(mp3, pcm);
        let info_out = (samples != 0).then_some({
            (samples, info.channels as u8, info.hz as u32, info.bitrate_kbps as u32)
        });
        Frame {
            consumed: info.frame_bytes as usize,
            pcm: pcm[..samples * info.channels.max(0) as usize].to_vec(),
            info: info_out,
        }
    })
}

/// Decode `input` with nanomp3.
pub fn decode_rust(input: &[u8], feed: Feed) -> Vec<Frame> {
    let mut dec = nanomp3::Decoder::new();
    run(input, feed, |mp3, pcm| {
        let (consumed, info) = dec.decode(mp3, pcm);
        let n = info.map_or(0, |i| i.samples_produced * i.channels.num() as usize);
        Frame {
            consumed,
            info: info.map(|i| (i.samples_produced, i.channels.num(), i.sample_rate, i.bitrate)),
            pcm: pcm[..n].to_vec(),
        }
    })
}

/// Path to the upstream conformance vectors (git submodule).
pub fn vectors_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("minimp3/vectors")
}

/// Compare two decode runs. Returns a description of the first difference.
pub fn first_difference(c: &[Frame], r: &[Frame]) -> Option<String> {
    for (idx, (a, b)) in c.iter().zip(r).enumerate() {
        if a.consumed != b.consumed || a.info != b.info {
            return Some(format!(
                "frame {idx}: C consumed {} info {:?}, Rust consumed {} info {:?}",
                a.consumed, a.info, b.consumed, b.info
            ));
        }
        if let Some(i) = a.pcm.iter().zip(&b.pcm).position(|(x, y)| x.to_bits() != y.to_bits()) {
            return Some(format!(
                "frame {idx} sample {i}: C {:e} vs Rust {:e}",
                a.pcm[i], b.pcm[i]
            ));
        }
    }
    (c.len() != r.len()).then(|| format!("C produced {} calls, Rust {}", c.len(), r.len()))
}
