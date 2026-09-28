//! Test/bench-only bindings to upstream minimp3, used as the reference that
//! nanomp3 must match bit for bit. This crate is never published; `unsafe`
//! here is confined to the FFI boundary.

pub mod ex;

use std::os::raw::{c_int, c_ulong, c_void};

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

type InitFn = unsafe extern "C" fn(*mut Mp3Dec);
type DecodeFn = unsafe extern "C" fn(*mut Mp3Dec, *const u8, c_int, *mut c_void, *mut FrameInfo) -> c_int;
type SizeofFn = unsafe extern "C" fn() -> c_ulong;

macro_rules! builds {
    ($($variant:ident => $init:ident, $decode:ident, $size:ident;)*) => {
        extern "C" {
            $(
                fn $init(dec: *mut Mp3Dec);
                fn $decode(dec: *mut Mp3Dec, mp3: *const u8, len: c_int, pcm: *mut c_void, info: *mut FrameInfo) -> c_int;
                fn $size() -> c_ulong;
            )*
        }
        impl Flavor {
            fn fns(self) -> (InitFn, DecodeFn, SizeofFn) {
                match self { $(Flavor::$variant => ($init, $decode, $size),)* }
            }
        }
    };
}

/// A build of upstream minimp3 (see `build.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `MINIMP3_ONLY_MP3`, float output, no SIMD.
    Mp3F32,
    /// `MINIMP3_ONLY_MP3`, int16 output, no SIMD.
    Mp3S16,
    /// Layers I-III, float output, no SIMD.
    FullF32,
    /// Layers I-III, int16 output, no SIMD.
    FullS16,
    /// `MINIMP3_ONLY_MP3`, float output, upstream SIMD. Not bit-exact; for speed.
    SimdF32,
}

builds! {
    Mp3F32 => mp3dec_init_mp3_f32, mp3dec_decode_frame_mp3_f32, mp3dec_sizeof_mp3_f32;
    Mp3S16 => mp3dec_init_mp3_s16, mp3dec_decode_frame_mp3_s16, mp3dec_sizeof_mp3_s16;
    FullF32 => mp3dec_init_full_f32, mp3dec_decode_frame_full_f32, mp3dec_sizeof_full_f32;
    FullS16 => mp3dec_init_full_s16, mp3dec_decode_frame_full_s16, mp3dec_sizeof_full_s16;
    SimdF32 => mp3dec_init_simd_f32, mp3dec_decode_frame_simd_f32, mp3dec_sizeof_simd_f32;
}

/// Sample formats C minimp3 can produce natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum COutput {
    F32,
    S16,
}

impl Flavor {
    /// The bit-exact C build for nanomp3 as configured (depends on `layer12`).
    pub fn reference(output: COutput) -> Self {
        match (cfg!(feature = "layer12"), output) {
            (false, COutput::F32) => Flavor::Mp3F32,
            (false, COutput::S16) => Flavor::Mp3S16,
            (true, COutput::F32) => Flavor::FullF32,
            (true, COutput::S16) => Flavor::FullS16,
        }
    }

    fn output(self) -> COutput {
        match self {
            Flavor::Mp3S16 | Flavor::FullS16 => COutput::S16,
            _ => COutput::F32,
        }
    }
}

/// Output sample types nanomp3 supports, with an independent reference
/// conversion for the ones minimp3 can't produce.
pub trait PcmSample: nanomp3::Sample + Default + std::fmt::Debug {
    /// The C build that produces this type natively, if any.
    const C_OUTPUT: Option<COutput>;
    fn bits(self) -> u32;
    /// Converts minimp3's `f32` output the way nanomp3 documents it, using
    /// f64 arithmetic and std's rounding so it doesn't share code with nanomp3.
    fn from_c_f32(f: f32) -> Self;
}

/// `f` back in 16-bit units, scaled, rounded half away from zero, clamped.
fn round_clamp(f: f32, scale: f64, lo: f64, hi: f64) -> f64 {
    (f as f64 * 32768.0 * scale).round().clamp(lo, hi)
}

impl PcmSample for f32 {
    const C_OUTPUT: Option<COutput> = Some(COutput::F32);
    fn bits(self) -> u32 {
        self.to_bits()
    }
    fn from_c_f32(f: f32) -> Self {
        f
    }
}
impl PcmSample for i16 {
    const C_OUTPUT: Option<COutput> = Some(COutput::S16);
    fn bits(self) -> u32 {
        self as u16 as u32
    }
    fn from_c_f32(f: f32) -> Self {
        round_clamp(f, 1.0, i16::MIN as f64, i16::MAX as f64) as i16
    }
}
impl PcmSample for i8 {
    const C_OUTPUT: Option<COutput> = None;
    fn bits(self) -> u32 {
        self as u8 as u32
    }
    fn from_c_f32(f: f32) -> Self {
        round_clamp(f, 1.0 / 256.0, i8::MIN as f64, i8::MAX as f64) as i8
    }
}
impl PcmSample for i32 {
    const C_OUTPUT: Option<COutput> = None;
    fn bits(self) -> u32 {
        self as u32
    }
    fn from_c_f32(f: f32) -> Self {
        round_clamp(f, 65536.0, i32::MIN as f64, i32::MAX as f64) as i32
    }
}
impl PcmSample for u8 {
    const C_OUTPUT: Option<COutput> = None;
    fn bits(self) -> u32 {
        self as u32
    }
    fn from_c_f32(f: f32) -> Self {
        i8::from_c_f32(f) as u8 ^ 0x80
    }
}
impl PcmSample for u16 {
    const C_OUTPUT: Option<COutput> = None;
    fn bits(self) -> u32 {
        self as u32
    }
    fn from_c_f32(f: f32) -> Self {
        i16::from_c_f32(f) as u16 ^ 0x8000
    }
}
impl PcmSample for u32 {
    const C_OUTPUT: Option<COutput> = None;
    fn bits(self) -> u32 {
        self
    }
    fn from_c_f32(f: f32) -> Self {
        i32::from_c_f32(f) as u32 ^ 0x8000_0000
    }
}

/// A C minimp3 decoder instance.
pub struct CDecoder {
    dec: Box<Mp3Dec>,
    decode: DecodeFn,
    flavor: Flavor,
}

impl CDecoder {
    pub fn new(flavor: Flavor) -> Self {
        let (init, decode, size) = flavor.fns();
        // SAFETY: Mp3Dec is plain-old-data; all-zero is its documented initial
        // state (mp3dec_init only clears header[0], which zeroing covers).
        let mut dec: Box<Mp3Dec> = unsafe { Box::new(std::mem::zeroed()) };
        // SAFETY: plain C function with no preconditions.
        assert_eq!(unsafe { size() } as usize, std::mem::size_of::<Mp3Dec>(), "mp3dec_t layout mismatch");
        // SAFETY: `dec` is a valid, exclusively owned mp3dec_t.
        unsafe { init(&mut *dec) };
        Self { dec, decode, flavor }
    }

    /// Mirrors `mp3dec_decode_frame`. Returns (samples per channel, info).
    pub fn decode<S: PcmSample>(&mut self, mp3: &[u8], pcm: &mut [S]) -> (usize, FrameInfo) {
        assert_eq!(S::C_OUTPUT, Some(self.flavor.output()), "sample type does not match {:?}", self.flavor);
        assert!(pcm.len() >= nanomp3::MAX_SAMPLES_PER_FRAME);
        let len = c_int::try_from(mp3.len()).expect("input too large for C API");
        let mut info = FrameInfo::default();
        // SAFETY: pointers are valid for the lengths passed, pcm holds at least
        // MINIMP3_MAX_SAMPLES_PER_FRAME samples of the build's sample type, and
        // dec is exclusively borrowed.
        let samples = unsafe {
            (self.decode)(&mut *self.dec, mp3.as_ptr(), len, pcm.as_mut_ptr().cast(), &mut info)
        };
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
    /// Bit patterns of the interleaved PCM that was produced.
    pub pcm: Vec<u32>,
}

/// How to present input to the decoder.
#[derive(Debug, Clone, Copy)]
pub enum Feed {
    /// Pass everything that remains (the whole file is in memory).
    Whole,
    /// Pass at most this many bytes at a time, like a streaming read-ahead buffer.
    Window(usize),
}

fn run<S: PcmSample>(
    mut input: &[u8],
    feed: Feed,
    mut step: impl FnMut(&[u8], &mut [S]) -> (usize, Option<(usize, u8, u32, u32)>),
) -> Vec<Frame> {
    let mut pcm = vec![S::default(); nanomp3::MAX_SAMPLES_PER_FRAME];
    let mut frames = Vec::new();
    while !input.is_empty() {
        let view = match feed {
            Feed::Whole => input,
            Feed::Window(n) => &input[..input.len().min(n)],
        };
        let (consumed, info) = step(view, &mut pcm);
        if consumed == 0 {
            break;
        }
        let n = info.map_or(0, |(s, ch, _, _)| s * ch as usize);
        frames.push(Frame { consumed, info, pcm: pcm[..n].iter().map(|s| s.bits()).collect() });
        input = &input[consumed.min(input.len())..];
    }
    frames
}

/// Decode `input` with C minimp3.
pub fn decode_c<S: PcmSample>(input: &[u8], feed: Feed, flavor: Flavor) -> Vec<Frame> {
    let mut dec = CDecoder::new(flavor);
    run::<S>(input, feed, |mp3, pcm| {
        let (samples, info) = dec.decode(mp3, pcm);
        let out = (samples != 0)
            .then_some((samples, info.channels as u8, info.hz as u32, info.bitrate_kbps as u32));
        (info.frame_bytes as usize, out)
    })
}

/// What nanomp3 must produce: C's native output where minimp3 has the format
/// (for default-mode `i16`, only in compat mode, since the default fixes
/// minimp3's rounding quirk), otherwise C's `f32` output converted.
pub fn decode_reference<S: PcmSample>(input: &[u8], feed: Feed, minimp3_compat: bool) -> Vec<Frame> {
    match S::C_OUTPUT {
        Some(COutput::F32) => decode_c::<S>(input, feed, Flavor::reference(COutput::F32)),
        Some(COutput::S16) if minimp3_compat => decode_c::<S>(input, feed, Flavor::reference(COutput::S16)),
        _ => {
            let mut frames = decode_c::<f32>(input, feed, Flavor::reference(COutput::F32));
            for b in frames.iter_mut().flat_map(|f| &mut f.pcm) {
                *b = S::from_c_f32(f32::from_bits(*b)).bits();
            }
            frames
        }
    }
}

/// Decode `input` with nanomp3.
pub fn decode_rust<S: PcmSample>(input: &[u8], feed: Feed, minimp3_compat: bool) -> Vec<Frame> {
    let mut dec = if minimp3_compat { nanomp3::Decoder::new_minimp3_compat() } else { nanomp3::Decoder::new() };
    run::<S>(input, feed, |mp3, pcm| {
        let (consumed, info) = dec.decode(mp3, pcm);
        (consumed, info.ok().map(|i| (i.samples_produced, i.channels.num(), i.sample_rate, i.bitrate)))
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
        if let Some(i) = a.pcm.iter().zip(&b.pcm).position(|(x, y)| x != y) {
            return Some(format!("frame {idx} sample {i}: C {:#010x} vs Rust {:#010x}", a.pcm[i], b.pcm[i]));
        }
    }
    (c.len() != r.len()).then(|| format!("C produced {} calls, Rust {}", c.len(), r.len()))
}
