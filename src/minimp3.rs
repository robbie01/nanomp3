//! Safe Rust port of the Layer III decoder from [minimp3](https://github.com/lieff/minimp3).
//!
//! The code deliberately mirrors the structure of `minimp3.h` (function names
//! are snake_case versions of the C ones) so the two can be read side by side.
//! Every floating-point expression is evaluated in the same order as in C, which
//! keeps the output bit-identical to upstream built with `MINIMP3_ONLY_MP3`,
//! `MINIMP3_FLOAT_OUTPUT` and `MINIMP3_NO_SIMD`; `parity/` checks this.

// Float literals are spelled exactly as in minimp3.h. Rounding them or swapping
// in `core::f32::consts` could change results and break bit-exactness.
#![allow(clippy::excessive_precision, clippy::approx_constant)]
// Index loops mirror the C and keep parallel arrays visibly in step.
#![allow(clippy::needless_range_loop)]

mod lanes;
mod tables;
use lanes::{Lanes, F4};
use tables::*;

const HDR_SIZE: usize = 4;
const MAX_FREE_FORMAT_FRAME_SIZE: usize = 2304;
const MAX_FRAME_SYNC_MATCHES: usize = 10;
const MAX_BITRESERVOIR_BYTES: usize = 511;
const MAX_L3_FRAME_PAYLOAD_BYTES: usize = MAX_FREE_FORMAT_FRAME_SIZE;
const SHORT_BLOCK_TYPE: u8 = 2;
const STOP_BLOCK_TYPE: u8 = 3;
/// `MAX_SCFI` in C: `(255 + BITS_DEQUANTIZER_OUT*4 - 210 + 3) & ~3` with `BITS_DEQUANTIZER_OUT = -1`.
const MAX_SCFI: i32 = (255 - 4 - 210 + 3) & !3;

/// A PCM output sample format.
pub trait Sample: Copy + sealed::Sealed {
    #[doc(hidden)]
    fn scale_pcm(sample: f32) -> Self;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for i16 {}
}

impl Sample for f32 {
    #[inline(always)]
    fn scale_pcm(sample: f32) -> f32 {
        sample * (1.0 / 32768.0)
    }
}

impl Sample for i16 {
    #[inline(always)]
    fn scale_pcm(sample: f32) -> i16 {
        if sample >= 32766.5 {
            return 32767;
        }
        if sample <= -32767.5 {
            return -32768;
        }
        let s = (sample + 0.5) as i16;
        // Round away from zero, to be compliant.
        s - (s < 0) as i16
    }
}

/// Information about a decoded frame (`mp3dec_frame_info_t`).
#[derive(Copy, Clone, Default, Debug)]
pub struct FrameInfo {
    pub frame_bytes: usize,
    pub frame_offset: usize,
    pub channels: u8,
    pub hz: u32,
    pub layer: u8,
    pub bitrate_kbps: u32,
}

/// Persistent decoder state (`mp3dec_t`).
#[derive(Clone)]
struct State {
    mdct_overlap: [[f32; 9 * 32]; 2],
    qmf_state: [f32; 15 * 2 * 32],
    reserv: i32,
    free_format_bytes: usize,
    header: [u8; 4],
    reserv_buf: [u8; MAX_BITRESERVOIR_BYTES],
}

impl State {
    const fn new() -> Self {
        Self {
            mdct_overlap: [[0.; 9 * 32]; 2],
            qmf_state: [0.; 15 * 2 * 32],
            reserv: 0,
            free_format_bytes: 0,
            header: [0; 4],
            reserv_buf: [0; MAX_BITRESERVOIR_BYTES],
        }
    }
}

/// Per-frame working memory (`mp3dec_scratch_t`). Unlike C, it lives in the
/// decoder rather than on the stack so it doesn't have to be re-initialized for
/// every frame; only the parts C might read before writing are cleared.
#[derive(Clone)]
struct Scratch {
    grbuf: [[f32; 576]; 2],
    scf: [f32; 40],
    syn: [[f32; 4]; SYN_ROWS],
    ist_pos: [[u8; 39]; 2],
}

/// The decoder: persistent state plus scratch memory.
#[derive(Clone)]
pub struct Mp3Dec {
    st: State,
    scratch: Scratch,
    maindata: [u8; MAX_BITRESERVOIR_BYTES + MAX_L3_FRAME_PAYLOAD_BYTES],
}

impl Mp3Dec {
    pub const fn new() -> Self {
        Self {
            st: State::new(),
            scratch: Scratch {
                grbuf: [[0.; 576]; 2],
                scf: [0.; 40],
                syn: [[0.; 4]; SYN_ROWS],
                ist_pos: [[0; 39]; 2],
            },
            maindata: [0; MAX_BITRESERVOIR_BYTES + MAX_L3_FRAME_PAYLOAD_BYTES],
        }
    }
}

#[derive(Copy, Clone, Default)]
struct GrInfo {
    sfbtab: &'static [u8],
    part_23_length: u16,
    big_values: u16,
    scalefac_compress: u16,
    global_gain: u8,
    block_type: u8,
    mixed_block_flag: u8,
    n_long_sfb: u8,
    n_short_sfb: u8,
    table_select: [u8; 3],
    region_count: [u8; 3],
    subblock_gain: [u8; 3],
    preflag: u8,
    scalefac_scale: u8,
    count1_table: u8,
    scfsi: u8,
}

/// Bit reader (`bs_t`). Positions are signed like in C: the limit of a
/// malformed tiny frame can be negative, and `pos` keeps advancing past the
/// limit so callers can detect overruns.
struct Bs<'a> {
    buf: &'a [u8],
    pos: i32,
    limit: i32,
}

impl<'a> Bs<'a> {
    fn new(buf: &'a [u8], bytes: i32) -> Self {
        Self { buf, pos: 0, limit: bytes * 8 }
    }

    fn get_bits(&mut self, n: u32) -> u32 {
        let s = (self.pos & 7) as u32;
        let mut p = (self.pos >> 3) as usize;
        self.pos += n as i32;
        if self.pos > self.limit {
            return 0;
        }
        // In bounds: every byte read lies below `limit`, and `limit` never
        // exceeds the buffer.
        let mut shl = (n + s) as i32;
        let mut next = u32::from(self.buf[p] & (255 >> s));
        p += 1;
        let mut cache = 0;
        loop {
            shl -= 8;
            if shl <= 0 {
                break;
            }
            cache |= next << shl;
            next = u32::from(self.buf[p]);
            p += 1;
        }
        cache | (next >> -shl)
    }
}

/// A 4-byte frame header.
#[derive(Copy, Clone)]
struct Header([u8; 4]);

impl Header {
    fn read(buf: &[u8]) -> Self {
        Self([buf[0], buf[1], buf[2], buf[3]])
    }
    fn is_mono(self) -> bool {
        self.0[3] & 0xC0 == 0xC0
    }
    fn is_ms_stereo(self) -> bool {
        self.0[3] & 0xE0 == 0x60
    }
    fn is_free_format(self) -> bool {
        self.0[2] & 0xF0 == 0
    }
    fn is_crc(self) -> bool {
        self.0[1] & 1 == 0
    }
    fn test_padding(self) -> bool {
        self.0[2] & 0x2 != 0
    }
    fn test_mpeg1(self) -> bool {
        self.0[1] & 0x8 != 0
    }
    fn test_not_mpeg25(self) -> bool {
        self.0[1] & 0x10 != 0
    }
    fn test_i_stereo(self) -> bool {
        self.0[3] & 0x10 != 0
    }
    fn test_ms_stereo(self) -> bool {
        self.0[3] & 0x20 != 0
    }
    fn get_layer(self) -> u8 {
        (self.0[1] >> 1) & 3
    }
    fn get_bitrate(self) -> u8 {
        self.0[2] >> 4
    }
    fn get_sample_rate(self) -> u8 {
        (self.0[2] >> 2) & 3
    }
    fn get_my_sample_rate(self) -> usize {
        let h1 = self.0[1];
        (self.get_sample_rate() + (((h1 >> 3) & 1) + ((h1 >> 4) & 1)) * 3) as usize
    }
    fn is_frame_576(self) -> bool {
        self.0[1] & 14 == 2
    }
    fn is_layer_1(self) -> bool {
        self.0[1] & 6 == 6
    }

    fn valid(self) -> bool {
        let h = self.0;
        h[0] == 0xff
            && (h[1] & 0xF0 == 0xf0 || h[1] & 0xFE == 0xe2)
            && self.get_layer() != 0
            && self.get_bitrate() != 15
            && self.get_sample_rate() != 3
    }

    /// `hdr_compare(self, other)`: is `other` a valid header for the same stream?
    fn compare(self, other: Header) -> bool {
        let (h1, h2) = (self.0, other.0);
        other.valid()
            && (h1[1] ^ h2[1]) & 0xFE == 0
            && (h1[2] ^ h2[2]) & 0x0C == 0
            && self.is_free_format() == other.is_free_format()
    }

    fn bitrate_kbps(self) -> u32 {
        2 * u32::from(
            HDR_BITRATE_KBPS_HALFRATE[self.test_mpeg1() as usize][self.get_layer() as usize - 1]
                [self.get_bitrate() as usize],
        )
    }

    fn sample_rate_hz(self) -> u32 {
        HDR_SAMPLE_RATE_HZ_G_HZ[self.get_sample_rate() as usize]
            >> !self.test_mpeg1() as u32
            >> !self.test_not_mpeg25() as u32
    }

    fn frame_samples(self) -> u32 {
        if self.is_layer_1() {
            384
        } else {
            1152 >> self.is_frame_576() as u32
        }
    }

    fn frame_bytes(self, free_format_size: usize) -> usize {
        let mut frame_bytes =
            (self.frame_samples() * self.bitrate_kbps() * 125 / self.sample_rate_hz()) as usize;
        if self.is_layer_1() {
            frame_bytes &= !3; // slot align
        }
        if frame_bytes != 0 {
            frame_bytes
        } else {
            free_format_size
        }
    }

    fn padding(self) -> usize {
        if self.test_padding() {
            if self.is_layer_1() {
                4
            } else {
                1
            }
        } else {
            0
        }
    }
}

fn l3_read_side_info(bs: &mut Bs, gr: &mut [GrInfo; 4], hdr: Header) -> i32 {
    let mut scfsi: u32 = 0;
    let mut part_23_sum: i32 = 0;
    let mut sr_idx = hdr.get_my_sample_rate();
    sr_idx -= (sr_idx != 0) as usize;
    let mut gr_count = if hdr.is_mono() { 1 } else { 2 };
    let main_data_begin;
    if hdr.test_mpeg1() {
        gr_count *= 2;
        main_data_begin = bs.get_bits(9) as i32;
        scfsi = bs.get_bits(7 + gr_count);
    } else {
        main_data_begin = (bs.get_bits(8 + gr_count) >> gr_count) as i32;
    }

    for gr in &mut gr[..gr_count as usize] {
        if hdr.is_mono() {
            scfsi <<= 4;
        }
        gr.part_23_length = bs.get_bits(12) as u16;
        part_23_sum += i32::from(gr.part_23_length);
        gr.big_values = bs.get_bits(9) as u16;
        if gr.big_values > 288 {
            return -1;
        }
        gr.global_gain = bs.get_bits(8) as u8;
        gr.scalefac_compress = bs.get_bits(if hdr.test_mpeg1() { 4 } else { 9 }) as u16;
        gr.sfbtab = &L3_READ_SIDE_INFO_G_SCF_LONG[sr_idx];
        gr.n_long_sfb = 22;
        gr.n_short_sfb = 0;
        let tables;
        if bs.get_bits(1) != 0 {
            gr.block_type = bs.get_bits(2) as u8;
            if gr.block_type == 0 {
                return -1;
            }
            gr.mixed_block_flag = bs.get_bits(1) as u8;
            gr.region_count[0] = 7;
            gr.region_count[1] = 255;
            if gr.block_type == SHORT_BLOCK_TYPE {
                scfsi &= 0x0F0F;
                if gr.mixed_block_flag == 0 {
                    gr.region_count[0] = 8;
                    gr.sfbtab = &L3_READ_SIDE_INFO_G_SCF_SHORT[sr_idx];
                    gr.n_long_sfb = 0;
                    gr.n_short_sfb = 39;
                } else {
                    gr.sfbtab = &L3_READ_SIDE_INFO_G_SCF_MIXED[sr_idx];
                    gr.n_long_sfb = if hdr.test_mpeg1() { 8 } else { 6 };
                    gr.n_short_sfb = 30;
                }
            }
            tables = bs.get_bits(10) << 5;
            gr.subblock_gain[0] = bs.get_bits(3) as u8;
            gr.subblock_gain[1] = bs.get_bits(3) as u8;
            gr.subblock_gain[2] = bs.get_bits(3) as u8;
        } else {
            gr.block_type = 0;
            gr.mixed_block_flag = 0;
            tables = bs.get_bits(15);
            gr.region_count[0] = bs.get_bits(4) as u8;
            gr.region_count[1] = bs.get_bits(3) as u8;
            gr.region_count[2] = 255;
        }
        gr.table_select[0] = (tables >> 10) as u8;
        gr.table_select[1] = ((tables >> 5) & 31) as u8;
        gr.table_select[2] = (tables & 31) as u8;
        gr.preflag = if hdr.test_mpeg1() {
            bs.get_bits(1) as u8
        } else {
            (gr.scalefac_compress >= 500) as u8
        };
        gr.scalefac_scale = bs.get_bits(1) as u8;
        gr.count1_table = bs.get_bits(1) as u8;
        gr.scfsi = ((scfsi >> 12) & 15) as u8;
        scfsi <<= 4;
    }

    if part_23_sum + bs.pos > bs.limit + main_data_begin * 8 {
        return -1;
    }
    main_data_begin
}

fn l3_read_scalefactors(
    scf: &mut [u8; 40],
    ist_pos: &mut [u8; 39],
    scf_size: &[u8; 4],
    scf_count: &[u8],
    bitbuf: &mut Bs,
    mut scfsi: i32,
) {
    let mut pos = 0;
    for i in 0..4 {
        let cnt = scf_count[i] as usize;
        if cnt == 0 {
            break;
        }
        let scf = &mut scf[pos..pos + cnt];
        let ist_pos = &mut ist_pos[pos..pos + cnt];
        if scfsi & 8 != 0 {
            scf.copy_from_slice(ist_pos);
        } else {
            let bits = u32::from(scf_size[i]);
            if bits == 0 {
                scf.fill(0);
                ist_pos.fill(0);
            } else {
                let max_scf = if scfsi < 0 { (1 << bits) - 1 } else { -1 };
                for (scf, ist_pos) in scf.iter_mut().zip(ist_pos) {
                    let s = bitbuf.get_bits(bits) as i32;
                    *ist_pos = if s == max_scf { -1i32 as u8 } else { s as u8 };
                    *scf = s as u8;
                }
            }
        }
        pos += cnt;
        scfsi *= 2;
    }
    scf[pos..pos + 3].fill(0);
}

fn l3_ldexp_q2(mut y: f32, mut exp_q2: i32) -> f32 {
    loop {
        let e = (30 * 4).min(exp_q2);
        y *= L3_LDEXP_Q2_G_EXPFRAC[(e & 3) as usize] * ((1 << 30 >> (e >> 2)) as f32);
        exp_q2 -= e;
        if exp_q2 <= 0 {
            return y;
        }
    }
}

fn l3_decode_scalefactors(
    hdr: Header,
    ist_pos: &mut [u8; 39],
    bs: &mut Bs,
    gr: &GrInfo,
    scf: &mut [f32; 40],
    ch: usize,
) {
    let mut scf_partition: &[u8] = &L3_DECODE_SCALEFACTORS_G_SCM_PARTITIONS
        [(gr.n_short_sfb != 0) as usize + (gr.n_long_sfb == 0) as usize];
    let mut scf_size = [0u8; 4];
    let mut iscf = [0u8; 40];
    let scf_shift = u32::from(gr.scalefac_scale) + 1;
    let mut scfsi = i32::from(gr.scfsi);

    if hdr.test_mpeg1() {
        let part = L3_DECODE_SCALEFACTORS_G_SCFC_DECODE[gr.scalefac_compress as usize];
        scf_size[0] = part >> 2;
        scf_size[1] = scf_size[0];
        scf_size[2] = part & 3;
        scf_size[3] = scf_size[2];
    } else {
        let ist = (hdr.test_i_stereo() && ch != 0) as usize;
        let mut sfc = i32::from(gr.scalefac_compress) >> ist;
        let mut k = ist * 3 * 4;
        while sfc >= 0 {
            let mut modprod = 1;
            for i in (0..4).rev() {
                let m = i32::from(L3_DECODE_SCALEFACTORS_G_MOD[k + i]);
                scf_size[i] = (sfc / modprod % m) as u8;
                modprod *= m;
            }
            sfc -= modprod;
            k += 4;
        }
        scf_partition = &scf_partition[k..];
        scfsi = -16;
    }
    l3_read_scalefactors(&mut iscf, ist_pos, &scf_size, scf_partition, bs, scfsi);

    let n_long = gr.n_long_sfb as usize;
    let n_short = gr.n_short_sfb as usize;
    if n_short != 0 {
        let sh = 3 - scf_shift;
        for i in (0..n_short).step_by(3) {
            for j in 0..3 {
                let v = &mut iscf[n_long + i + j];
                *v = v.wrapping_add(gr.subblock_gain[j] << sh);
            }
        }
    } else if gr.preflag != 0 {
        for (v, &p) in iscf[11..21].iter_mut().zip(&L3_DECODE_SCALEFACTORS_G_PREAMP) {
            *v = v.wrapping_add(p);
        }
    }

    let gain_exp = i32::from(gr.global_gain) - 4 - 210 - if hdr.is_ms_stereo() { 2 } else { 0 };
    let gain = l3_ldexp_q2((1 << (MAX_SCFI / 4)) as f32, MAX_SCFI - gain_exp);
    for (scf, &iscf) in scf.iter_mut().zip(&iscf).take(n_long + n_short) {
        *scf = l3_ldexp_q2(gain, i32::from(iscf) << scf_shift);
    }
}

fn l3_pow_43(mut x: i32) -> f32 {
    let mut mult = 256;
    if x < 129 {
        return G_POW43[(16 + x) as usize];
    }
    if x < 1024 {
        mult = 16;
        x <<= 3;
    }
    let sign = (2 * x) & 64;
    let frac = ((x & 63) - sign) as f32 / ((x & !63) + sign) as f32;
    G_POW43[(16 + ((x + sign) >> 6)) as usize]
        * (1.0 + frac * ((4.0 / 3.0) + frac * (2.0 / 9.0)))
        * mult as f32
}

/// The Huffman decoder's 32-bit look-ahead cache. Bytes past the end of the
/// buffer read as zero: a corrupt `big_values` can make the decoder run past
/// the granule's data, which C tolerates by reading neighbouring memory.
struct BitCache<'a> {
    buf: &'a [u8],
    next: usize,
    cache: u32,
    sh: i32,
}

impl BitCache<'_> {
    #[inline(always)]
    fn byte(&self, i: usize) -> u32 {
        self.buf.get(i).copied().map_or(0, u32::from)
    }

    #[inline(always)]
    fn peek(&self, n: i32) -> u32 {
        self.cache >> (32 - n)
    }

    #[inline(always)]
    fn flush(&mut self, n: i32) {
        self.cache <<= n;
        self.sh += n;
    }

    #[inline(always)]
    fn check(&mut self) {
        while self.sh >= 0 {
            self.cache |= self.byte(self.next) << self.sh;
            self.next += 1;
            self.sh -= 8;
        }
    }

    /// Current bit position (`BSPOS` in C).
    fn pos(&self) -> i32 {
        self.next as i32 * 8 - 24 + self.sh
    }
}

fn l3_huffman(dst: &mut [f32; 576], bs: &mut Bs, gr_info: &GrInfo, scf: &[f32; 40], layer3gr_limit: i32) {
    let mut one = 0.0f32;
    let mut ireg = 0;
    let mut big_val_cnt = i32::from(gr_info.big_values);
    let sfbtab = gr_info.sfbtab;
    let mut sfb = 0;
    let mut scf_idx = 0;
    let mut d = 0;

    let start = (bs.pos / 8) as usize;
    let mut br = BitCache { buf: bs.buf, next: start + 4, cache: 0, sh: (bs.pos & 7) - 8 };
    br.cache = (((br.byte(start) * 256 + br.byte(start + 1)) * 256 + br.byte(start + 2)) * 256
        + br.byte(start + 3))
        << (bs.pos & 7);

    while big_val_cnt > 0 {
        let tab_num = gr_info.table_select[ireg] as usize;
        let mut sfb_cnt = i32::from(gr_info.region_count[ireg]);
        ireg += 1;
        let codebook = &L3_HUFFMAN_TABS[L3_HUFFMAN_TABINDEX[tab_num] as usize..];
        let linbits = i32::from(L3_HUFFMAN_G_LINBITS[tab_num]);
        loop {
            let np = i32::from(sfbtab[sfb]) / 2;
            sfb += 1;
            let mut pairs_to_decode = big_val_cnt.min(np);
            one = scf[scf_idx];
            scf_idx += 1;
            loop {
                let mut w = 5;
                let mut leaf = i32::from(codebook[br.peek(w) as usize]);
                while leaf < 0 {
                    br.flush(w);
                    w = leaf & 7;
                    leaf = i32::from(codebook[br.peek(w).wrapping_sub((leaf >> 3) as u32) as usize]);
                }
                br.flush(leaf >> 8);

                for _ in 0..2 {
                    let mut lsb = leaf & 0x0F;
                    if lsb == 15 && linbits != 0 {
                        lsb += br.peek(linbits) as i32;
                        br.flush(linbits);
                        br.check();
                        dst[d] = one * l3_pow_43(lsb) * if (br.cache as i32) < 0 { -1.0 } else { 1.0 };
                    } else {
                        dst[d] = G_POW43[(16 + lsb) as usize - 16 * (br.cache >> 31) as usize] * one;
                    }
                    br.flush((lsb != 0) as i32);
                    d += 1;
                    leaf >>= 4;
                }
                br.check();
                pairs_to_decode -= 1;
                if pairs_to_decode == 0 {
                    break;
                }
            }
            big_val_cnt -= np;
            if big_val_cnt <= 0 {
                break;
            }
            sfb_cnt -= 1;
            if sfb_cnt < 0 {
                break;
            }
        }
    }

    let codebook_count1: &[u8] =
        if gr_info.count1_table != 0 { &L3_HUFFMAN_TAB33 } else { &L3_HUFFMAN_TAB32 };
    let mut np = 1 - big_val_cnt;
    'count1: loop {
        let mut leaf = i32::from(codebook_count1[br.peek(4) as usize]);
        if leaf & 8 == 0 {
            leaf = i32::from(
                codebook_count1[(leaf >> 3) as usize + (br.cache << 4 >> (32 - (leaf & 3))) as usize],
            );
        }
        br.flush(leaf & 7);
        if br.pos() > layer3gr_limit {
            break;
        }
        for half in 0..2 {
            np -= 1;
            if np == 0 {
                np = i32::from(sfbtab[sfb]) / 2;
                sfb += 1;
                if np == 0 {
                    break 'count1;
                }
                one = scf[scf_idx];
                scf_idx += 1;
            }
            for s in 2 * half..2 * half + 2 {
                if leaf & (128 >> s) != 0 {
                    dst[d + s] = if (br.cache as i32) < 0 { -one } else { one };
                    br.flush(1);
                }
            }
        }
        br.check();
        d += 4;
    }
    bs.pos = layer3gr_limit;
}

fn l3_midside_stereo(buf: &mut [f32; 1152], start: usize, n: usize) {
    let (left, right) = buf.split_at_mut(576);
    for (l, r) in left[start..start + n].iter_mut().zip(&mut right[start..start + n]) {
        let a = *l;
        let b = *r;
        *l = a + b;
        *r = a - b;
    }
}

fn l3_intensity_stereo_band(buf: &mut [f32; 1152], start: usize, n: usize, kl: f32, kr: f32) {
    let (left, right) = buf.split_at_mut(576);
    for (l, r) in left[start..start + n].iter_mut().zip(&mut right[start..start + n]) {
        *r = *l * kr;
        *l *= kl;
    }
}

fn l3_stereo_top_band(right: &[f32], sfb: &[u8], nbands: usize, max_band: &mut [i32; 3]) {
    *max_band = [-1; 3];
    let mut off = 0;
    for i in 0..nbands {
        let n = sfb[i] as usize;
        let band = &right[off..off + n];
        if band.chunks_exact(2).any(|p| p[0] != 0.0 || p[1] != 0.0) {
            max_band[i % 3] = i as i32;
        }
        off += n;
    }
}

fn l3_stereo_process(
    buf: &mut [f32; 1152],
    ist_pos: &[u8; 39],
    sfb: &[u8],
    hdr: Header,
    max_band: &[i32; 3],
    mpeg2_sh: u32,
) {
    let max_pos = if hdr.test_mpeg1() { 7 } else { 64 };
    let mut off = 0;
    for (i, &n) in sfb.iter().take_while(|&&n| n != 0).enumerate() {
        let n = n as usize;
        let ipos = u32::from(ist_pos[i]);
        if i as i32 > max_band[i % 3] && ipos < max_pos {
            let s = if hdr.test_ms_stereo() { 1.41421356 } else { 1.0 };
            let (kl, kr);
            if hdr.test_mpeg1() {
                kl = L3_STEREO_PROCESS_G_PAN[2 * ipos as usize];
                kr = L3_STEREO_PROCESS_G_PAN[2 * ipos as usize + 1];
            } else {
                let k = l3_ldexp_q2(1.0, (((ipos + 1) >> 1) << mpeg2_sh) as i32);
                (kl, kr) = if ipos & 1 != 0 { (k, 1.0) } else { (1.0, k) };
            }
            l3_intensity_stereo_band(buf, off, n, kl * s, kr * s);
        } else if hdr.test_ms_stereo() {
            l3_midside_stereo(buf, off, n);
        }
        off += n;
    }
}

fn l3_intensity_stereo(buf: &mut [f32; 1152], ist_pos: &mut [u8; 39], gr: &[GrInfo], hdr: Header) {
    let mut max_band = [0i32; 3];
    let n_sfb = gr[0].n_long_sfb as usize + gr[0].n_short_sfb as usize;
    let max_blocks = if gr[0].n_short_sfb != 0 { 3 } else { 1 };

    l3_stereo_top_band(&buf[576..], gr[0].sfbtab, n_sfb, &mut max_band);
    if gr[0].n_long_sfb != 0 {
        max_band = [max_band[0].max(max_band[1]).max(max_band[2]); 3];
    }
    for i in 0..max_blocks {
        let default_pos = if hdr.test_mpeg1() { 3 } else { 0 };
        let itop = n_sfb - max_blocks + i;
        let prev = itop - max_blocks;
        ist_pos[itop] = if max_band[i] >= prev as i32 { default_pos } else { ist_pos[prev] };
    }
    l3_stereo_process(buf, ist_pos, gr[0].sfbtab, hdr, &max_band, u32::from(gr[1].scalefac_compress & 1));
}

fn l3_reorder(grbuf: &mut [f32], scratch: &mut [f32], sfb: &[u8]) {
    let mut src = 0;
    let mut dst = 0;
    for &len in sfb.iter().step_by(3).take_while(|&&len| len != 0) {
        let len = len as usize;
        for i in 0..len {
            scratch[dst] = grbuf[src + i];
            scratch[dst + 1] = grbuf[src + i + len];
            scratch[dst + 2] = grbuf[src + i + 2 * len];
            dst += 3;
        }
        src += 3 * len;
    }
    grbuf[..dst].copy_from_slice(&scratch[..dst]);
}

fn l3_antialias(grbuf: &mut [f32; 576], nbands: usize) {
    let aa = |k: usize, i: usize| F4::load(&L3_ANTIALIAS_G_AA[k][i..]);
    for b in 0..nbands {
        let (lo, hi) = grbuf[18 * b..18 * b + 36].split_at_mut(18);
        for i in [0, 4] {
            // hi[i..i+4] against lo[17-i..=14-i] (reversed).
            let u = F4::load(&hi[i..]);
            let d = F4::load(&lo[14 - i..]).rev();
            (u * aa(0, i) - d * aa(1, i)).store(&mut hi[i..]);
            (u * aa(1, i) + d * aa(0, i)).rev().store(&mut lo[14 - i..]);
        }
    }
}

#[inline(always)]
fn l3_dct3_9<T: Lanes>(y: &mut [T; 9]) {
    let c = T::splat;
    let mut s0 = y[0];
    let mut s2 = y[2];
    let mut s4 = y[4];
    let mut s6 = y[6];
    let mut s8 = y[8];
    let mut t0 = s0 + s6 * c(0.5);
    s0 -= s6;
    let mut t4 = (s4 + s2) * c(0.93969262);
    let mut t2 = (s8 + s2) * c(0.76604444);
    s6 = (s4 - s8) * c(0.17364818);
    s4 += s8 - s2;

    s2 = s0 - s4 * c(0.5);
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;

    let mut s1 = y[1];
    let mut s3 = y[3];
    let mut s5 = y[5];
    let mut s7 = y[7];

    s3 = s3 * c(0.86602540);
    t0 = (s5 + s1) * c(0.98480775);
    t4 = (s5 - s7) * c(0.34202014);
    t2 = (s1 + s7) * c(0.64278761);
    s1 = (s1 - s5 - s7) * c(0.86602540);

    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;

    y[0] = s4 - s7;
    y[1] = s2 + s1;
    y[2] = s0 - s3;
    y[3] = s8 + s5;
    y[5] = s8 - s5;
    y[6] = s0 + s3;
    y[7] = s2 - s1;
    y[8] = s4 + s7;
}

/// IMDCT-36 of `T::N` adjacent bands at once (one band per lane).
#[inline(always)]
fn l3_imdct36_bands<T: Lanes>(grbuf: &mut [f32], overlap: &mut [f32], window: &[f32; 18]) {
    let c = T::splat;
    let g = |grbuf: &[f32], o: usize| T::gather(&grbuf[o..], 18);
    let mut co = [c(0.0); 9];
    let mut si = [c(0.0); 9];
    co[0] = -g(grbuf, 0);
    si[0] = g(grbuf, 17);
    for i in 0..4 {
        si[8 - 2 * i] = g(grbuf, 4 * i + 1) - g(grbuf, 4 * i + 2);
        co[1 + 2 * i] = g(grbuf, 4 * i + 1) + g(grbuf, 4 * i + 2);
        si[7 - 2 * i] = g(grbuf, 4 * i + 4) - g(grbuf, 4 * i + 3);
        co[2 + 2 * i] = -(g(grbuf, 4 * i + 3) + g(grbuf, 4 * i + 4));
    }
    l3_dct3_9(&mut co);
    l3_dct3_9(&mut si);

    si[1] = -si[1];
    si[3] = -si[3];
    si[5] = -si[5];
    si[7] = -si[7];

    for i in 0..9 {
        let ovl = T::gather(&overlap[i..], 9);
        let sum = co[i] * c(L3_IMDCT36_G_TWID9[9 + i]) + si[i] * c(L3_IMDCT36_G_TWID9[i]);
        (co[i] * c(L3_IMDCT36_G_TWID9[i]) - si[i] * c(L3_IMDCT36_G_TWID9[9 + i])).scatter(&mut overlap[i..], 9);
        (ovl * c(window[i]) - sum * c(window[9 + i])).scatter(&mut grbuf[i..], 18);
        (ovl * c(window[9 + i]) + sum * c(window[i])).scatter(&mut grbuf[17 - i..], 18);
    }
}

fn l3_imdct36(grbuf: &mut [f32], overlap: &mut [f32], window: &[f32; 18], nbands: usize) {
    let mut j = 0;
    while j + 4 <= nbands {
        l3_imdct36_bands::<F4>(&mut grbuf[18 * j..], &mut overlap[9 * j..], window);
        j += 4;
    }
    while j < nbands {
        l3_imdct36_bands::<f32>(&mut grbuf[18 * j..], &mut overlap[9 * j..], window);
        j += 1;
    }
}

fn l3_idct3(x0: f32, x1: f32, x2: f32, dst: &mut [f32; 3]) {
    let m1 = x1 * 0.86602540;
    let a1 = x0 - x2 * 0.5;
    dst[1] = x0 + x2;
    dst[0] = a1 + m1;
    dst[2] = a1 - m1;
}

fn l3_imdct12(x: &[f32], dst: &mut [f32], overlap: &mut [f32]) {
    let mut co = [0f32; 3];
    let mut si = [0f32; 3];
    l3_idct3(-x[0], x[6] + x[3], x[12] + x[9], &mut co);
    l3_idct3(x[15], x[12] - x[9], x[6] - x[3], &mut si);
    si[1] = -si[1];

    for i in 0..3 {
        let ovl = overlap[i];
        let sum = co[i] * L3_IMDCT12_G_TWID3[3 + i] + si[i] * L3_IMDCT12_G_TWID3[i];
        overlap[i] = co[i] * L3_IMDCT12_G_TWID3[i] - si[i] * L3_IMDCT12_G_TWID3[3 + i];
        dst[i] = ovl * L3_IMDCT12_G_TWID3[2 - i] - sum * L3_IMDCT12_G_TWID3[5 - i];
        dst[5 - i] = ovl * L3_IMDCT12_G_TWID3[5 - i] + sum * L3_IMDCT12_G_TWID3[2 - i];
    }
}

fn l3_imdct_short(grbuf: &mut [f32], overlap: &mut [f32], nbands: usize) {
    for (grbuf, overlap) in grbuf.chunks_exact_mut(18).zip(overlap.chunks_exact_mut(9)).take(nbands) {
        let mut tmp = [0f32; 18];
        tmp.copy_from_slice(grbuf);
        grbuf[..6].copy_from_slice(&overlap[..6]);
        let (ov_out, ov_state) = overlap.split_at_mut(6);
        l3_imdct12(&tmp, &mut grbuf[6..12], ov_state);
        l3_imdct12(&tmp[1..], &mut grbuf[12..18], ov_state);
        l3_imdct12(&tmp[2..], ov_out, ov_state);
    }
}

fn l3_change_sign(grbuf: &mut [f32; 576]) {
    for band in grbuf.chunks_exact_mut(18).skip(1).step_by(2) {
        for x in band.iter_mut().skip(1).step_by(2) {
            *x = -*x;
        }
    }
}

fn l3_imdct_gr(grbuf: &mut [f32; 576], overlap: &mut [f32; 288], block_type: u8, n_long_bands: usize) {
    let (long_buf, rest_buf) = grbuf.split_at_mut(18 * n_long_bands);
    let (long_ovl, rest_ovl) = overlap.split_at_mut(9 * n_long_bands);
    if n_long_bands != 0 {
        l3_imdct36(long_buf, long_ovl, &L3_IMDCT_GR_G_MDCT_WINDOW[0], n_long_bands);
    }
    if block_type == SHORT_BLOCK_TYPE {
        l3_imdct_short(rest_buf, rest_ovl, 32 - n_long_bands);
    } else {
        let window = &L3_IMDCT_GR_G_MDCT_WINDOW[(block_type == STOP_BLOCK_TYPE) as usize];
        l3_imdct36(rest_buf, rest_ovl, window, 32 - n_long_bands);
    }
}

fn l3_save_reservoir(h: &mut State, bs: &Bs) {
    let mut pos = ((bs.pos + 7) as u32 / 8) as i32;
    let mut remains = (bs.limit as u32 / 8).wrapping_sub(pos as u32) as i32;
    if remains > MAX_BITRESERVOIR_BYTES as i32 {
        pos += remains - MAX_BITRESERVOIR_BYTES as i32;
        remains = MAX_BITRESERVOIR_BYTES as i32;
    }
    if remains > 0 {
        let (pos, remains) = (pos as usize, remains as usize);
        h.reserv_buf[..remains].copy_from_slice(&bs.buf[pos..pos + remains]);
    }
    h.reserv = remains;
}

fn l3_restore_reservoir<'a>(
    h: &State,
    bs: &Bs,
    maindata: &'a mut [u8; MAX_BITRESERVOIR_BYTES + MAX_L3_FRAME_PAYLOAD_BYTES],
    main_data_begin: i32,
) -> (Bs<'a>, bool) {
    let frame_bytes = ((bs.limit - bs.pos) / 8) as usize;
    let bytes_have = h.reserv.min(main_data_begin) as usize;
    let reserv_off = (h.reserv - main_data_begin).max(0) as usize;
    maindata[..bytes_have].copy_from_slice(&h.reserv_buf[reserv_off..reserv_off + bytes_have]);
    let frame_start = (bs.pos / 8) as usize;
    maindata[bytes_have..bytes_have + frame_bytes]
        .copy_from_slice(&bs.buf[frame_start..frame_start + frame_bytes]);
    // Bounding the reader to the valid bytes makes Huffman over-reads on corrupt
    // streams see zeros without having to clear the buffer.
    let len = bytes_have + frame_bytes;
    (Bs::new(&maindata[..len], len as i32), h.reserv >= main_data_begin)
}

fn l3_decode(h: &mut State, s: &mut Scratch, bs: &mut Bs, gr_info: &[GrInfo], nch: usize) {
    let hdr = Header(h.header);
    for ch in 0..nch {
        let layer3gr_limit = bs.pos + i32::from(gr_info[ch].part_23_length);
        l3_decode_scalefactors(hdr, &mut s.ist_pos[ch], bs, &gr_info[ch], &mut s.scf, ch);
        l3_huffman(&mut s.grbuf[ch], bs, &gr_info[ch], &s.scf, layer3gr_limit);
    }

    let stereo: &mut [f32; 1152] = s.grbuf.as_flattened_mut().try_into().unwrap();
    if hdr.test_i_stereo() {
        l3_intensity_stereo(stereo, &mut s.ist_pos[1], gr_info, hdr);
    } else if hdr.is_ms_stereo() {
        l3_midside_stereo(stereo, 0, 576);
    }

    for ch in 0..nch {
        let gr = &gr_info[ch];
        let mut aa_bands = 31;
        let n_long_bands: usize =
            (if gr.mixed_block_flag != 0 { 2 } else { 0 }) << (hdr.get_my_sample_rate() == 2) as u32;

        if gr.n_short_sfb != 0 {
            aa_bands = n_long_bands.saturating_sub(1);
            l3_reorder(
                &mut s.grbuf[ch][n_long_bands * 18..],
                s.syn.as_flattened_mut(),
                &gr.sfbtab[gr.n_long_sfb as usize..],
            );
        }

        l3_antialias(&mut s.grbuf[ch], aa_bands);
        l3_imdct_gr(&mut s.grbuf[ch], &mut h.mdct_overlap[ch], gr.block_type, n_long_bands);
        l3_change_sign(&mut s.grbuf[ch]);
    }
}

/// DCT-II over columns `k..k + T::N` of the 18x32 subband matrix.
#[inline(always)]
fn mp3d_dct_ii_cols<T: Lanes>(grbuf: &mut [f32; 576], k: usize) {
    let c = T::splat;
    let mut t = [[c(0.0); 8]; 4];
    for i in 0..8 {
        let x0 = T::load(&grbuf[k + i * 18..]);
        let x1 = T::load(&grbuf[k + (15 - i) * 18..]);
        let x2 = T::load(&grbuf[k + (16 + i) * 18..]);
        let x3 = T::load(&grbuf[k + (31 - i) * 18..]);
        let t0 = x0 + x3;
        let t1 = x1 + x2;
        let t2 = (x1 - x2) * c(MP3D_DCT_II_G_SEC[3 * i]);
        let t3 = (x0 - x3) * c(MP3D_DCT_II_G_SEC[3 * i + 1]);
        t[0][i] = t0 + t1;
        t[1][i] = (t0 - t1) * c(MP3D_DCT_II_G_SEC[3 * i + 2]);
        t[2][i] = t3 + t2;
        t[3][i] = (t3 - t2) * c(MP3D_DCT_II_G_SEC[3 * i + 2]);
    }
    for x in &mut t {
        let [mut x0, mut x1, mut x2, mut x3, mut x4, mut x5, mut x6, mut x7] = *x;
        let mut xt = x0 - x7;
        x0 += x7;
        x7 = x1 - x6;
        x1 += x6;
        x6 = x2 - x5;
        x2 += x5;
        x5 = x3 - x4;
        x3 += x4;
        x4 = x0 - x3;
        x0 += x3;
        x3 = x1 - x2;
        x1 += x2;
        x[0] = x0 + x1;
        x[4] = (x0 - x1) * c(0.70710677);
        x5 += x6;
        x6 = (x6 + x7) * c(0.70710677);
        x7 += xt;
        x3 = (x3 + x4) * c(0.70710677);
        x5 -= x7 * c(0.198912367); // rotate by PI/8
        x7 += x5 * c(0.382683432);
        x5 -= x7 * c(0.198912367);
        x0 = xt - x6;
        xt += x6;
        x[1] = (xt + x7) * c(0.50979561);
        x[2] = (x4 + x3) * c(0.54119611);
        x[3] = (x0 - x5) * c(0.60134488);
        x[5] = (x0 + x5) * c(0.89997619);
        x[6] = (x4 - x3) * c(1.30656302);
        x[7] = (xt - x7) * c(2.56291556);
    }
    for i in 0..7 {
        let y = k + i * 4 * 18;
        t[0][i].store(&mut grbuf[y..]);
        (t[2][i] + t[3][i] + t[3][i + 1]).store(&mut grbuf[y + 18..]);
        (t[1][i] + t[1][i + 1]).store(&mut grbuf[y + 2 * 18..]);
        (t[2][i + 1] + t[3][i] + t[3][i + 1]).store(&mut grbuf[y + 3 * 18..]);
    }
    let y = k + 7 * 4 * 18;
    t[0][7].store(&mut grbuf[y..]);
    (t[2][7] + t[3][7]).store(&mut grbuf[y + 18..]);
    t[1][7].store(&mut grbuf[y + 2 * 18..]);
    t[3][7].store(&mut grbuf[y + 3 * 18..]);
}

fn mp3d_dct_ii(grbuf: &mut [f32; 576], n: usize) {
    let mut k = 0;
    while k + 4 <= n {
        mp3d_dct_ii_cols::<F4>(grbuf, k);
        k += 4;
    }
    while k < n {
        mp3d_dct_ii_cols::<f32>(grbuf, k);
        k += 1;
    }
}

/// `mp3d_synth_pair`: the two samples C writes to `pcm[0]` and `pcm[16*nch]`,
/// read from `lane` (and `lane + 2`) of every 16th row starting at `row`.
#[inline(always)]
fn mp3d_synth_pair(z: &[[f32; 4]; SYNTH_ROWS], row: usize, lane: usize) -> (f32, f32) {
    let x = |m: usize| z[row + 16 * m][lane];
    let mut a;
    a = (x(14) - x(0)) * 29.0;
    a += (x(1) + x(13)) * 213.0;
    a += (x(12) - x(2)) * 459.0;
    a += (x(3) + x(11)) * 2037.0;
    a += (x(10) - x(4)) * 5153.0;
    a += (x(5) + x(9)) * 6574.0;
    a += (x(8) - x(6)) * 37489.0;
    a += x(7) * 75038.0;
    let first = a;

    let x = |m: usize| z[row + 16 * m][lane + 2];
    a = x(14) * 104.0;
    a += x(12) * 1567.0;
    a += x(10) * 9727.0;
    a += x(8) * 64019.0;
    a += x(6) * -9975.0;
    a += x(4) * -45.0;
    a += x(2) * 146.0;
    a += x(0) * -5.0;
    (first, a)
}

// The polyphase filterbank history (`lins` in C) is viewed as rows of 4 floats:
// [left even, right even, left odd, right odd]. In C terms, row `r` lane `l` is
// `lins[4*r + l]`, and `zlin = lins + 15*64` starts at row `ZLIN`.
const ZLIN: usize = 15 * 16;
/// Rows of `lins` touched by one call to `mp3d_synth`.
const SYNTH_ROWS: usize = ZLIN + 2 * 16;

fn mp3d_synth<S: Sample, const NCH: usize>(
    xl: &[f32],
    xr: &[f32],
    dst: &mut [S],
    lins: &mut [[f32; 4]; SYNTH_ROWS],
) {
    let xl: &[f32; 560] = xl[..560].try_into().unwrap();
    let xr: &[f32; 560] = xr[..560].try_into().unwrap();
    let dst = &mut dst[..64 * NCH];
    let r = NCH - 1;

    lins[ZLIN + 15] = [xl[18 * 16], xr[18 * 16], xl[0], xr[0]];
    lins[ZLIN + 31] = [xl[1 + 18 * 16], xr[1 + 18 * 16], xl[1], xr[1]];

    // For mono, C also computes "right" samples into the same slots and then
    // overwrites them with the left ones; skipping them gives identical output.
    let mut pair = |at: usize, row: usize, lane: usize| {
        let (a, b) = mp3d_synth_pair(lins, row, lane);
        dst[at] = S::scale_pcm(a);
        dst[at + 16 * NCH] = S::scale_pcm(b);
    };
    if NCH == 2 {
        pair(r, 15, 1);
        pair(r + 32 * NCH, 31, 1);
    }
    pair(0, 15, 0);
    pair(32 * NCH, 31, 0);

    for i in (0..15).rev() {
        lins[ZLIN + i] = [xl[18 * (31 - i)], xr[18 * (31 - i)], xl[1 + 18 * (31 - i)], xr[1 + 18 * (31 - i)]];
        lins[ZLIN + 16 + i][0] = xl[1 + 18 * (1 + i)];
        lins[ZLIN + 16 + i][1] = xr[1 + 18 * (1 + i)];
        lins[ZLIN - 16 + i][2] = xl[18 * (1 + i)];
        lins[ZLIN - 16 + i][3] = xr[18 * (1 + i)];

        let w: &[f32; 16] = MP3D_SYNTH_G_WIN[(14 - i) * 16..][..16].try_into().unwrap();
        let vz = |k: usize| F4::load(&lins[ZLIN + i - 16 * k]);
        let vy = |k: usize| F4::load(&lins[i + 16 * k]);
        let w0 = |k: usize| F4::splat(w[2 * k]);
        let w1 = |k: usize| F4::splat(w[2 * k + 1]);
        let mut b = vz(0) * w1(0) + vy(0) * w0(0);
        let mut a = vz(0) * w0(0) - vy(0) * w1(0);
        for k in [1, 3, 5, 7] {
            b += vz(k) * w1(k) + vy(k) * w0(k);
            a += vy(k) * w1(k) - vz(k) * w0(k);
            if k < 7 {
                let k = k + 1;
                b += vz(k) * w1(k) + vy(k) * w0(k);
                a += vz(k) * w0(k) - vy(k) * w1(k);
            }
        }
        let (a, b) = (a.to_array(), b.to_array());

        if NCH == 2 {
            dst[r + (15 - i) * NCH] = S::scale_pcm(a[1]);
            dst[r + (17 + i) * NCH] = S::scale_pcm(b[1]);
        }
        dst[(15 - i) * NCH] = S::scale_pcm(a[0]);
        dst[(17 + i) * NCH] = S::scale_pcm(b[0]);
        if NCH == 2 {
            dst[r + (47 - i) * NCH] = S::scale_pcm(a[3]);
            dst[r + (49 + i) * NCH] = S::scale_pcm(b[3]);
        }
        dst[(47 - i) * NCH] = S::scale_pcm(a[2]);
        dst[(49 + i) * NCH] = S::scale_pcm(b[2]);
    }
}

fn mp3d_synth_all<S: Sample, const NCH: usize>(
    grbuf: &[f32],
    nbands: usize,
    pcm: &mut [S],
    lins: &mut [[f32; 4]; SYN_ROWS],
) {
    for i in (0..nbands).step_by(2) {
        mp3d_synth::<S, NCH>(
            &grbuf[i..],
            &grbuf[576 * (NCH - 1) + i..],
            &mut pcm[32 * NCH * i..],
            (&mut lins[16 * i..16 * i + SYNTH_ROWS]).try_into().unwrap(),
        );
    }
}

/// Rows in the whole synthesis buffer (`syn` in C: `(18 + 15) * 2 * 32` floats).
const SYN_ROWS: usize = (18 + 15) * 2 * 32 / 4;

fn mp3d_synth_granule<S: Sample>(
    qmf_state: &mut [f32; 960],
    grbuf: &mut [[f32; 576]; 2],
    nbands: usize,
    nch: usize,
    pcm: &mut [S],
    lins: &mut [[f32; 4]; SYN_ROWS],
) {
    for ch in &mut grbuf[..nch] {
        mp3d_dct_ii(ch, nbands);
    }

    lins.as_flattened_mut()[..15 * 64].copy_from_slice(qmf_state);

    let grbuf = grbuf.as_flattened();
    if nch == 1 {
        mp3d_synth_all::<S, 1>(grbuf, nbands, pcm, lins);
    } else {
        mp3d_synth_all::<S, 2>(grbuf, nbands, pcm, lins);
    }

    let tail = &lins.as_flattened()[nbands * 64..nbands * 64 + 15 * 64];
    if nch == 1 {
        // Standard (not MINIMP3_NONSTANDARD_BUT_LOGICAL) behavior: a mono frame
        // only advances the left channel's filterbank history, so a later
        // switch to stereo starts the right channel from its old state.
        for (q, l) in qmf_state.chunks_exact_mut(2).zip(tail.chunks_exact(2)) {
            q[0] = l[0];
        }
    } else {
        qmf_state.copy_from_slice(tail);
    }
}

fn mp3d_match_frame(hdr: &[u8], frame_bytes: usize) -> bool {
    let first = Header::read(hdr);
    let mut i = 0;
    for nmatch in 0..MAX_FRAME_SYNC_MATCHES {
        let h = Header::read(&hdr[i..]);
        i += h.frame_bytes(frame_bytes) + h.padding();
        if i + HDR_SIZE > hdr.len() {
            return nmatch > 0;
        }
        if !first.compare(Header::read(&hdr[i..])) {
            return false;
        }
    }
    true
}

/// Returns `(offset, frame_bytes)`; `frame_bytes` is 0 when no frame was found.
fn mp3d_find_frame(mp3: &[u8], free_format_bytes: &mut usize) -> (usize, usize) {
    let mp3_bytes = mp3.len();
    let mut i = 0;
    while i + HDR_SIZE < mp3_bytes {
        let buf = &mp3[i..];
        let h = Header::read(buf);
        if h.valid() {
            let mut frame_bytes = h.frame_bytes(*free_format_bytes);
            let mut frame_and_padding = frame_bytes + h.padding();

            let mut k = HDR_SIZE;
            while frame_bytes == 0 && k < MAX_FREE_FORMAT_FRAME_SIZE && i + 2 * k + HDR_SIZE < mp3_bytes {
                let next = Header::read(&buf[k..]);
                if h.compare(next) {
                    let fb = k - h.padding();
                    let nextfb = fb + next.padding();
                    if i + k + nextfb + HDR_SIZE <= mp3_bytes && h.compare(Header::read(&buf[k + nextfb..])) {
                        frame_and_padding = k;
                        frame_bytes = fb;
                        *free_format_bytes = fb;
                    }
                }
                k += 1;
            }

            if (frame_bytes != 0 && i + frame_and_padding <= mp3_bytes && mp3d_match_frame(buf, frame_bytes))
                || (i == 0 && frame_and_padding == mp3_bytes)
            {
                return (i, frame_and_padding);
            }
            *free_format_bytes = 0;
        }
        i += 1;
    }
    (mp3_bytes, 0)
}

/// `mp3dec_decode_frame`. With `pcm == None`, only parses the frame header and
/// returns the number of samples the frame would produce (per channel).
pub fn mp3dec_decode_frame<S: Sample>(
    dec: &mut Mp3Dec,
    mp3: &[u8],
    pcm: Option<&mut [S]>,
    info: &mut FrameInfo,
) -> usize {
    let Mp3Dec { st: dec, scratch, maindata } = dec;
    let mp3_bytes = mp3.len();
    let mut i = 0;
    let mut frame_size = 0;

    if mp3_bytes > 4 && dec.header[0] == 0xff && Header(dec.header).compare(Header::read(mp3)) {
        let h = Header::read(mp3);
        frame_size = h.frame_bytes(dec.free_format_bytes) + h.padding();
        if frame_size != mp3_bytes
            && (frame_size + HDR_SIZE > mp3_bytes || !h.compare(Header::read(&mp3[frame_size..])))
        {
            frame_size = 0;
        }
    }
    if frame_size == 0 {
        *dec = State::new();
        (i, frame_size) = mp3d_find_frame(mp3, &mut dec.free_format_bytes);
        if frame_size == 0 || i + frame_size > mp3_bytes {
            info.frame_bytes = i;
            return 0;
        }
    }

    let hdr = Header::read(&mp3[i..]);
    dec.header = hdr.0;
    info.frame_bytes = i + frame_size;
    info.frame_offset = i;
    info.channels = if hdr.is_mono() { 1 } else { 2 };
    info.hz = hdr.sample_rate_hz();
    info.layer = 4 - hdr.get_layer();
    info.bitrate_kbps = hdr.bitrate_kbps();

    let Some(pcm) = pcm else {
        return hdr.frame_samples() as usize;
    };

    let frame = mp3.get(i + HDR_SIZE..i + frame_size).unwrap_or(&[]);
    let mut bs_frame = Bs::new(frame, frame_size as i32 - HDR_SIZE as i32);
    if hdr.is_crc() {
        bs_frame.get_bits(16);
    }

    if info.layer != 3 {
        return 0;
    }

    let nch = info.channels as usize;
    let mut gr_info = [GrInfo::default(); 4];
    let main_data_begin = l3_read_side_info(&mut bs_frame, &mut gr_info, hdr);
    if main_data_begin < 0 || bs_frame.pos > bs_frame.limit {
        dec.header[0] = 0; // mp3dec_init
        return 0;
    }

    let (mut bs, success) = l3_restore_reservoir(dec, &bs_frame, maindata, main_data_begin);
    if success {
        // C leaves these uninitialized and corrupt streams can read them before
        // writing; clearing them keeps results deterministic.
        scratch.scf = [0.; 40];
        scratch.ist_pos = [[0; 39]; 2];
        let granules = if hdr.test_mpeg1() { 2 } else { 1 };
        for (igr, pcm) in pcm.chunks_exact_mut(576 * nch).take(granules).enumerate() {
            scratch.grbuf = [[0.; 576]; 2];
            l3_decode(dec, scratch, &mut bs, &gr_info[igr * nch..], nch);
            mp3d_synth_granule(&mut dec.qmf_state, &mut scratch.grbuf, 18, nch, pcm, &mut scratch.syn);
        }
    }
    l3_save_reservoir(dec, &bs);
    success as usize * Header(dec.header).frame_samples() as usize
}
