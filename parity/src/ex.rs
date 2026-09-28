//! Bindings to minimp3_ex and runners that drive it and nanomp3's port through
//! identical operations, so their observable behavior can be compared.

use std::io::Cursor;
use std::os::raw::{c_int, c_void};

use crate::FrameInfo;
use nanomp3::{Options, Reader, SliceReader};

type ReadCb = unsafe extern "C" fn(buf: *mut c_void, size: usize, user: *mut c_void) -> usize;
type SeekCb = unsafe extern "C" fn(position: u64, user: *mut c_void) -> c_int;
type IterateCb = unsafe extern "C" fn(
    user: *mut c_void,
    frame: *const u8,
    frame_size: c_int,
    free_format_bytes: c_int,
    buf_size: usize,
    offset: u64,
    info: *mut FrameInfo,
) -> c_int;

/// `mp3dec_io_t`.
#[repr(C)]
struct CIo {
    read: ReadCb,
    read_data: *mut c_void,
    seek: SeekCb,
    seek_data: *mut c_void,
}

/// An in-memory file behind minimp3_ex's I/O callbacks, with `fread`/`fseek`
/// semantics (the same as `std::io::Cursor`).
struct MemIo {
    data: Vec<u8>,
    pos: u64,
}

unsafe extern "C" fn mem_read(buf: *mut c_void, size: usize, user: *mut c_void) -> usize {
    // SAFETY: `user` is the MemIo registered with this CIo, and minimp3 passes
    // a buffer of at least `size` bytes.
    let io = unsafe { &mut *(user as *mut MemIo) };
    let start = (io.pos as usize).min(io.data.len());
    let n = size.min(io.data.len() - start);
    unsafe { std::ptr::copy_nonoverlapping(io.data.as_ptr().add(start), buf as *mut u8, n) };
    io.pos += n as u64;
    n
}

unsafe extern "C" fn mem_seek(position: u64, user: *mut c_void) -> c_int {
    // SAFETY: as above.
    let io = unsafe { &mut *(user as *mut MemIo) };
    io.pos = position;
    0
}

/// An in-memory file plus the callback table pointing at it, kept at a stable
/// address while C holds pointers to them.
struct CFile {
    _io: Box<MemIo>,
    cio: Box<CIo>,
}

impl CFile {
    fn new(data: &[u8]) -> Self {
        let mut io = Box::new(MemIo { data: data.to_vec(), pos: 0 });
        let p = &mut *io as *mut MemIo as *mut c_void;
        let cio = Box::new(CIo { read: mem_read, read_data: p, seek: mem_seek, seek_data: p });
        Self { _io: io, cio }
    }

    fn ptr(&mut self) -> *mut CIo {
        &mut *self.cio
    }
}

struct ExFns {
    detect_buf: unsafe extern "C" fn(*const u8, usize) -> c_int,
    detect_cb: unsafe extern "C" fn(*mut CIo) -> c_int,
    load_buf: unsafe extern "C" fn(*const u8, usize, *mut *mut f32, *mut usize, *mut c_int) -> c_int,
    load_cb: unsafe extern "C" fn(*mut CIo, *mut *mut f32, *mut usize, *mut c_int) -> c_int,
    iterate_buf: unsafe extern "C" fn(*const u8, usize, IterateCb, *mut c_void) -> c_int,
    iterate_cb: unsafe extern "C" fn(*mut CIo, IterateCb, *mut c_void) -> c_int,
    ex_new: unsafe extern "C" fn() -> *mut c_void,
    ex_free: unsafe extern "C" fn(*mut c_void),
    ex_state: unsafe extern "C" fn(*const c_void, *mut u64),
    ex_open_buf: unsafe extern "C" fn(*mut c_void, *const u8, usize, c_int) -> c_int,
    ex_open_cb: unsafe extern "C" fn(*mut c_void, *mut CIo, c_int) -> c_int,
    ex_seek: unsafe extern "C" fn(*mut c_void, u64) -> c_int,
    ex_read: unsafe extern "C" fn(*mut c_void, *mut f32, usize) -> usize,
    ex_read_frame: unsafe extern "C" fn(*mut c_void, *mut *mut f32, *mut FrameInfo, usize) -> usize,
    free: unsafe extern "C" fn(*mut c_void),
}

macro_rules! ex_builds {
    ($($name:ident: $detect_buf:ident, $detect_cb:ident, $load_buf:ident, $load_cb:ident, $iterate_buf:ident,
       $iterate_cb:ident, $new:ident, $free_ex:ident, $state:ident, $open_buf:ident, $open_cb:ident,
       $seek:ident, $read:ident, $read_frame:ident, $free:ident;)*) => {
        extern "C" {
            $(
                fn $detect_buf(buf: *const u8, size: usize) -> c_int;
                fn $detect_cb(io: *mut CIo) -> c_int;
                fn $load_buf(buf: *const u8, size: usize, pcm: *mut *mut f32, samples: *mut usize, meta: *mut c_int) -> c_int;
                fn $load_cb(io: *mut CIo, pcm: *mut *mut f32, samples: *mut usize, meta: *mut c_int) -> c_int;
                fn $iterate_buf(buf: *const u8, size: usize, cb: IterateCb, user: *mut c_void) -> c_int;
                fn $iterate_cb(io: *mut CIo, cb: IterateCb, user: *mut c_void) -> c_int;
                fn $new() -> *mut c_void;
                fn $free_ex(d: *mut c_void);
                fn $state(d: *const c_void, out: *mut u64);
                fn $open_buf(d: *mut c_void, buf: *const u8, size: usize, flags: c_int) -> c_int;
                fn $open_cb(d: *mut c_void, io: *mut CIo, flags: c_int) -> c_int;
                fn $seek(d: *mut c_void, position: u64) -> c_int;
                fn $read(d: *mut c_void, buf: *mut f32, samples: usize) -> usize;
                fn $read_frame(d: *mut c_void, buf: *mut *mut f32, info: *mut FrameInfo, max: usize) -> usize;
                fn $free(p: *mut c_void);
            )*
        }
        $(
            static $name: ExFns = ExFns {
                detect_buf: $detect_buf, detect_cb: $detect_cb, load_buf: $load_buf, load_cb: $load_cb,
                iterate_buf: $iterate_buf, iterate_cb: $iterate_cb, ex_new: $new, ex_free: $free_ex,
                ex_state: $state, ex_open_buf: $open_buf, ex_open_cb: $open_cb, ex_seek: $seek,
                ex_read: $read, ex_read_frame: $read_frame, free: $free,
            };
        )*
    };
}

ex_builds! {
    EX_MP3: mp3dec_detect_buf_ex_mp3, shim_detect_cb_ex_mp3, shim_load_buf_ex_mp3, shim_load_cb_ex_mp3,
        mp3dec_iterate_buf_ex_mp3, shim_iterate_cb_ex_mp3, shim_ex_new_ex_mp3, shim_ex_free_ex_mp3,
        shim_ex_state_ex_mp3, mp3dec_ex_open_buf_ex_mp3, mp3dec_ex_open_cb_ex_mp3, mp3dec_ex_seek_ex_mp3,
        mp3dec_ex_read_ex_mp3, mp3dec_ex_read_frame_ex_mp3, shim_free_ex_mp3;
    EX_MP3_TRANS: mp3dec_detect_buf_ex_mp3_trans, shim_detect_cb_ex_mp3_trans, shim_load_buf_ex_mp3_trans,
        shim_load_cb_ex_mp3_trans, mp3dec_iterate_buf_ex_mp3_trans, shim_iterate_cb_ex_mp3_trans,
        shim_ex_new_ex_mp3_trans, shim_ex_free_ex_mp3_trans, shim_ex_state_ex_mp3_trans,
        mp3dec_ex_open_buf_ex_mp3_trans, mp3dec_ex_open_cb_ex_mp3_trans, mp3dec_ex_seek_ex_mp3_trans,
        mp3dec_ex_read_ex_mp3_trans, mp3dec_ex_read_frame_ex_mp3_trans, shim_free_ex_mp3_trans;
    EX_FULL: mp3dec_detect_buf_ex_full, shim_detect_cb_ex_full, shim_load_buf_ex_full, shim_load_cb_ex_full,
        mp3dec_iterate_buf_ex_full, shim_iterate_cb_ex_full, shim_ex_new_ex_full, shim_ex_free_ex_full,
        shim_ex_state_ex_full, mp3dec_ex_open_buf_ex_full, mp3dec_ex_open_cb_ex_full, mp3dec_ex_seek_ex_full,
        mp3dec_ex_read_ex_full, mp3dec_ex_read_frame_ex_full, shim_free_ex_full;
    EX_FULL_TRANS: mp3dec_detect_buf_ex_full_trans, shim_detect_cb_ex_full_trans, shim_load_buf_ex_full_trans,
        shim_load_cb_ex_full_trans, mp3dec_iterate_buf_ex_full_trans, shim_iterate_cb_ex_full_trans,
        shim_ex_new_ex_full_trans, shim_ex_free_ex_full_trans, shim_ex_state_ex_full_trans,
        mp3dec_ex_open_buf_ex_full_trans, mp3dec_ex_open_cb_ex_full_trans, mp3dec_ex_seek_ex_full_trans,
        mp3dec_ex_read_ex_full_trans, mp3dec_ex_read_frame_ex_full_trans, shim_free_ex_full_trans;
}

/// The minimp3_ex build matching nanomp3's configuration.
fn fns(transition: bool) -> &'static ExFns {
    match (cfg!(feature = "layer12"), transition) {
        (false, false) => &EX_MP3,
        (false, true) => &EX_MP3_TRANS,
        (true, false) => &EX_FULL,
        (true, true) => &EX_FULL_TRANS,
    }
}

// ---------------------------------------------------------------- detect

pub fn c_detect(data: &[u8], io: bool) -> bool {
    let f = fns(false);
    // SAFETY: valid buffer / callback table for the duration of the call.
    let r = unsafe {
        if io {
            (f.detect_cb)(CFile::new(data).ptr())
        } else {
            (f.detect_buf)(data.as_ptr(), data.len())
        }
    };
    r == 0
}

pub fn rust_detect(data: &[u8], io: bool, compat: bool) -> bool {
    match (io, compat) {
        (true, true) => nanomp3::__compat::detect_reader(Cursor::new(data)).unwrap(),
        (true, false) => nanomp3::detect_reader(Cursor::new(data)).unwrap(),
        (false, true) => nanomp3::__compat::detect(data),
        (false, false) => nanomp3::detect(data),
    }
}

// ---------------------------------------------------------------- iterate

/// What an iterate callback sees: (offset, frame_size, free_format_bytes,
/// buf_size, hz, channels, layer, bitrate_kbps).
pub type IterFrame = (u64, usize, usize, usize, u32, u8, u8, u32);

unsafe extern "C" fn collect(
    user: *mut c_void,
    _frame: *const u8,
    frame_size: c_int,
    free_format_bytes: c_int,
    buf_size: usize,
    offset: u64,
    info: *mut FrameInfo,
) -> c_int {
    // SAFETY: `user` is the Vec passed below; `info` is valid for the call.
    let (v, i) = unsafe { (&mut *(user as *mut Vec<IterFrame>), &*info) };
    v.push((
        offset,
        frame_size as usize,
        free_format_bytes as usize,
        buf_size,
        i.hz as u32,
        i.channels as u8,
        i.layer as u8,
        i.bitrate_kbps as u32,
    ));
    0
}

pub fn c_iterate(data: &[u8], io: bool) -> Vec<IterFrame> {
    let f = fns(false);
    let mut v: Vec<IterFrame> = Vec::new();
    let user = &mut v as *mut Vec<IterFrame> as *mut c_void;
    // SAFETY: valid buffer / callback table and `user` for the call.
    unsafe {
        if io {
            (f.iterate_cb)(CFile::new(data).ptr(), collect, user);
        } else if !data.is_empty() {
            (f.iterate_buf)(data.as_ptr(), data.len(), collect, user);
        }
    }
    v
}

pub fn rust_iterate(data: &[u8], io: bool, compat: bool) -> Vec<IterFrame> {
    if io {
        nanomp3::__compat::frames_reader(Cursor::new(data), compat).unwrap()
    } else {
        nanomp3::__compat::frames(data, compat)
    }
}

// ---------------------------------------------------------------- load

/// Result of `mp3dec_load_*`: (return code, sample bits, channels, hz, layer,
/// avg_bitrate_kbps).
pub type Loaded = (i32, Vec<u32>, i32, i32, i32, i32);

pub fn c_load(data: &[u8], io: bool, transition: bool) -> Loaded {
    let f = fns(transition);
    let (mut pcm, mut n, mut meta) = (std::ptr::null_mut(), 0usize, [0 as c_int; 4]);
    let mut file = CFile::new(data);
    // SAFETY: valid pointers for the call; C allocates `pcm` (freed below).
    let r = unsafe {
        if io {
            (f.load_cb)(file.ptr(), &mut pcm, &mut n, meta.as_mut_ptr())
        } else {
            (f.load_buf)(data.as_ptr(), data.len(), &mut pcm, &mut n, meta.as_mut_ptr())
        }
    };
    let samples = if pcm.is_null() {
        Vec::new()
    } else {
        // SAFETY: C returned `n` samples at `pcm`.
        let s = unsafe { std::slice::from_raw_parts(pcm, n) }.iter().map(|x| x.to_bits()).collect();
        unsafe { (f.free)(pcm as *mut c_void) };
        s
    };
    (r, samples, meta[0], meta[1], meta[2], meta[3])
}

pub fn rust_load(data: &[u8], io: bool, transition: bool, compat: bool) -> Loaded {
    let opts = Options::default().allow_mono_stereo_transition(transition).minimp3_compat(compat);
    let d = if io {
        nanomp3::decode_all_reader_with::<f32, _>(Cursor::new(data), opts).unwrap()
    } else {
        nanomp3::decode_all_with::<f32>(data, opts)
    };
    (
        if d.format_changed { -5 } else { 0 },
        d.samples.iter().map(|s| s.to_bits()).collect(),
        d.channels.map_or(0, |c| c.num() as i32),
        d.sample_rate as i32,
        d.layer as i32,
        d.avg_bitrate_kbps as i32,
    )
}

// ---------------------------------------------------------------- reader

/// How to open a reader.
#[derive(Debug, Clone, Copy)]
pub struct ExConfig {
    /// Use the Read + Seek reader (`mp3dec_ex_open_cb`).
    pub io: bool,
    /// Seek ops are byte offsets (`MP3D_SEEK_TO_BYTE`).
    pub seek_to_byte: bool,
    pub do_not_scan: bool,
    pub transition: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ExOp {
    /// `mp3dec_ex_read` of this many samples.
    Read(usize),
    /// `mp3dec_ex_read_frame` with this limit.
    ReadFrame(usize),
    /// `mp3dec_ex_seek` (interleaved sample or byte, per the config).
    Seek(u64),
}

/// The observable result of one operation: (return value, output sample bits,
/// [samples, detected_samples, cur_sample, vbr_tag_found, failed]). The first
/// event is the open, whose output holds (channels, hz, layer).
pub type ExEvent = (i64, Vec<u32>, [u64; 5]);

pub fn c_ex(data: &[u8], cfg: ExConfig, ops: &[ExOp]) -> Vec<ExEvent> {
    let f = fns(cfg.transition);
    let flags = if cfg.seek_to_byte { 0 } else { 1 } | if cfg.do_not_scan { 2 } else { 0 } | if cfg.transition { 4 } else { 0 };
    let buf = data.to_vec();
    let mut file = CFile::new(data);
    let mut events = Vec::new();
    // SAFETY: `d` is a zeroed mp3dec_ex_t from C; `buf` and `file` outlive it;
    // output buffers are sized as passed.
    unsafe {
        let d = (f.ex_new)();
        let state = |d| {
            let mut o = [0u64; 8];
            (f.ex_state)(d, o.as_mut_ptr());
            ([o[0], o[1], o[2], o[3], (o[4] != 0) as u64], vec![o[5] as u32, o[6] as u32, o[7] as u32])
        };
        let r = if cfg.io { (f.ex_open_cb)(d, file.ptr(), flags) } else { (f.ex_open_buf)(d, buf.as_ptr(), buf.len(), flags) };
        let (s, info) = state(d);
        events.push((r as i64, info, s));
        for op in ops {
            let (r, pcm) = match *op {
                ExOp::Read(n) => {
                    let mut out = vec![0f32; n];
                    let r = (f.ex_read)(d, out.as_mut_ptr(), n);
                    (r as i64, out[..r].iter().map(|x| x.to_bits()).collect())
                }
                ExOp::ReadFrame(max) => {
                    let mut p = std::ptr::null_mut();
                    let mut info = FrameInfo::default();
                    let r = (f.ex_read_frame)(d, &mut p, &mut info, max);
                    let pcm = if r == 0 { Vec::new() } else { std::slice::from_raw_parts(p, r).iter().map(|x| x.to_bits()).collect() };
                    (r as i64, pcm)
                }
                ExOp::Seek(pos) => ((f.ex_seek)(d, pos) as i64, Vec::new()),
            };
            events.push((r, pcm, state(d).0));
            if matches!(op, ExOp::Seek(_)) && r != 0 {
                // A failed seek leaves minimp3_ex inconsistent: its next read
                // returns memory past its buffer. Stop here.
                break;
            }
        }
        (f.ex_free)(d);
    }
    events
}

pub fn rust_ex(data: &[u8], cfg: ExConfig, ops: &[ExOp], compat: bool) -> Vec<ExEvent> {
    let opts = Options::default()
        .skip_scan(cfg.do_not_scan)
        .allow_mono_stereo_transition(cfg.transition)
        .minimp3_compat(compat);
    macro_rules! run {
        ($reader:expr) => {{
            let mut r = $reader;
            let mut events = Vec::new();
            let state = |raw: &nanomp3::__Raw<'_, f32, _>| {
                let (a, b, c, d, e) = raw.state();
                [a, b, c, d as u64, e as u64]
            };
            {
                let raw = r.__raw();
                let (ch, hz, layer) = raw.info();
                events.push((0, vec![ch as u32, hz, layer as u32], state(&raw)));
            }
            for op in ops {
                let failed_before = state(&r.__raw())[4];
                let (ret, pcm) = match *op {
                    ExOp::Read(n) => {
                        let mut out = vec![0f32; n];
                        let got = r.read(&mut out).unwrap_or(0);
                        (got as i64, out[..got].iter().map(|x| x.to_bits()).collect())
                    }
                    ExOp::ReadFrame(max) => {
                        let mut raw = r.__raw();
                        match raw.read_frame(max) {
                            Some(s) => (s.len() as i64, s.iter().map(|x| x.to_bits()).collect()),
                            None => (0, Vec::new()),
                        }
                    }
                    ExOp::Seek(pos) => (if r.__raw().seek(pos, cfg.seek_to_byte).is_ok() { 0 } else { -3 }, Vec::new()),
                };
                let raw = r.__raw();
                // minimp3_ex leaves its error flag as it was when a seek fails;
                // nanomp3 enters its error state. Report C's view.
                let mut st = state(&raw);
                let seek_failed = matches!(op, ExOp::Seek(_)) && ret != 0;
                if seek_failed {
                    st[4] = failed_before;
                }
                events.push((ret, pcm, st));
                if seek_failed {
                    break; // see c_ex
                }
            }
            events
        }};
    }
    if cfg.io {
        run!(Reader::<_, f32>::with_options(Cursor::new(data), opts).unwrap())
    } else {
        run!(SliceReader::<f32>::with_options(data, opts))
    }
}

/// A deterministic mix of reads and seeks that covers the reader's paths:
/// partial and whole-frame reads, seeks to the start, middle, odd positions,
/// near and past the end, and reading to the end.
///
/// For byte seeks, `audio_end` bounds the targets: seeking past it (into
/// trailing tags) makes minimp3's memory reader read out of bounds.
pub fn standard_ops(total: u64, audio_end: u64, seek_to_byte: bool) -> Vec<ExOp> {
    use ExOp::*;
    let t = if seek_to_byte { audio_end } else if total == 0 { 100_000 } else { total };
    let clamp = |p: u64| if seek_to_byte { p.min(t) } else { p };
    let mut ops = vec![
        Read(1000),
        Read(1),
        Read(4608),
        ReadFrame(usize::MAX),
        ReadFrame(100),
        Seek(0),
        Read(3000),
        Seek(t / 2),
        Read(2000),
        Seek(t / 2 + 1),
        Read(999),
        Seek(t / 3 * 2),
        ReadFrame(usize::MAX),
        Seek(t.saturating_sub(10)),
        Read(100_000),
        Seek(clamp(t + 500)),
        Read(100),
        Seek(1),
    ];
    // Read to the end (bounded), then once more past it.
    ops.extend(std::iter::repeat_n(Read(65_536), 64));
    ops.push(Read(1));
    ops
}
