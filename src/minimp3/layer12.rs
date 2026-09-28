//! MPEG Layer I and Layer II decoding (the `#ifndef MINIMP3_ONLY_MP3` parts of
//! minimp3.h). Enabled by the `layer12` feature.

use super::{mp3d_synth_granule, Bs, Header, Sample, Scratch, State};

const MODE_MONO: u8 = 3;
const MODE_JOINT_STEREO: u8 = 1;

struct ScaleInfo {
    scf: [f32; 3 * 64],
    total_bands: usize,
    stereo_bands: usize,
    bitalloc: [u8; 64],
    scfcod: [u8; 64],
}

struct SubbandAlloc {
    tab_offset: u8,
    code_tab_width: u8,
    band_count: u8,
}

const fn alloc(tab_offset: u8, code_tab_width: u8, band_count: u8) -> SubbandAlloc {
    SubbandAlloc { tab_offset, code_tab_width, band_count }
}

static G_ALLOC_L1: [SubbandAlloc; 1] = [alloc(76, 4, 32)];
static G_ALLOC_L2M2: [SubbandAlloc; 3] = [alloc(60, 4, 4), alloc(44, 3, 7), alloc(44, 2, 19)];
static G_ALLOC_L2M1: [SubbandAlloc; 4] = [alloc(0, 4, 3), alloc(16, 4, 8), alloc(32, 3, 12), alloc(40, 2, 7)];
static G_ALLOC_L2M1_LOWRATE: [SubbandAlloc; 2] = [alloc(44, 4, 2), alloc(44, 3, 10)];

#[rustfmt::skip]
static G_BITALLOC_CODE_TAB: [u8; 92] = [
    0,17, 3, 4, 5,6,7, 8,9,10,11,12,13,14,15,16,
    0,17,18, 3,19,4,5, 6,7, 8, 9,10,11,12,13,16,
    0,17,18, 3,19,4,5,16,
    0,17,18,16,
    0,17,18,19, 4,5,6, 7,8, 9,10,11,12,13,14,15,
    0,17,18, 3,19,4,5, 6,7, 8, 9,10,11,12,13,14,
    0, 2, 3, 4, 5,6,7, 8,9,10,11,12,13,14,15,16,
];

/// `DQ(x)` from C: three dequantizer steps per bit allocation, divided by `x`.
const fn dq(x: f32) -> [f32; 3] {
    [9.53674316e-07 / x, 7.56931807e-07 / x, 6.00777173e-07 / x]
}

static G_DEQ_L12: [[f32; 3]; 18] = [
    dq(3.),
    dq(7.),
    dq(15.),
    dq(31.),
    dq(63.),
    dq(127.),
    dq(255.),
    dq(511.),
    dq(1023.),
    dq(2047.),
    dq(4095.),
    dq(8191.),
    dq(16383.),
    dq(32767.),
    dq(65535.),
    dq(3.),
    dq(5.),
    dq(9.),
];

fn l12_subband_alloc_table(hdr: Header, sci: &mut ScaleInfo) -> &'static [SubbandAlloc] {
    let mode = hdr.0[3] >> 6;
    let stereo_bands = if mode == MODE_MONO {
        0
    } else if mode == MODE_JOINT_STEREO {
        ((((hdr.0[3] >> 4) & 3) as usize) << 2) + 4
    } else {
        32
    };

    let (alloc, nbands): (&'static [SubbandAlloc], usize) = if hdr.is_layer_1() {
        (&G_ALLOC_L1, 32)
    } else if !hdr.test_mpeg1() {
        (&G_ALLOC_L2M2, 30)
    } else {
        let sample_rate_idx = hdr.get_sample_rate();
        let mut kbps = hdr.bitrate_kbps() >> (mode != MODE_MONO) as u32;
        if kbps == 0 {
            kbps = 192; // free-format
        }
        if kbps < 56 {
            (&G_ALLOC_L2M1_LOWRATE, if sample_rate_idx == 2 { 12 } else { 8 })
        } else if kbps >= 96 && sample_rate_idx != 1 {
            (&G_ALLOC_L2M1, 30)
        } else {
            (&G_ALLOC_L2M1, 27)
        }
    };

    sci.total_bands = nbands;
    sci.stereo_bands = stereo_bands.min(nbands);
    alloc
}

fn l12_read_scalefactors(bs: &mut Bs, sci: &mut ScaleInfo, bands: usize) {
    for i in 0..bands {
        let mut s = 0.0;
        let ba = sci.bitalloc[i] as usize;
        let mask = if ba != 0 { 4 + ((19 >> sci.scfcod[i]) & 3) } else { 0 };
        for (j, m) in [4, 2, 1].into_iter().enumerate() {
            if mask & m != 0 {
                let b = bs.get_bits(6) as usize;
                s = G_DEQ_L12[ba - 2][b % 3] * (1 << 21 >> (b / 3)) as f32;
            }
            sci.scf[3 * i + j] = s;
        }
    }
}

fn l12_read_scale_info(hdr: Header, bs: &mut Bs, sci: &mut ScaleInfo) {
    let mut subband_alloc = l12_subband_alloc_table(hdr, sci).iter();
    let mut k = 0;
    let mut ba_bits = 0;
    let mut ba_code_tab: &[u8] = &G_BITALLOC_CODE_TAB;

    for i in 0..sci.total_bands {
        if i == k {
            let a = subband_alloc.next().unwrap();
            k += a.band_count as usize;
            ba_bits = u32::from(a.code_tab_width);
            ba_code_tab = &G_BITALLOC_CODE_TAB[a.tab_offset as usize..];
        }
        let mut ba = ba_code_tab[bs.get_bits(ba_bits) as usize];
        sci.bitalloc[2 * i] = ba;
        if i < sci.stereo_bands {
            ba = ba_code_tab[bs.get_bits(ba_bits) as usize];
        }
        sci.bitalloc[2 * i + 1] = if sci.stereo_bands != 0 { ba } else { 0 };
    }

    for i in 0..2 * sci.total_bands {
        sci.scfcod[i] = if sci.bitalloc[i] == 0 {
            6
        } else if hdr.is_layer_1() {
            2
        } else {
            bs.get_bits(2) as u8
        };
    }

    l12_read_scalefactors(bs, sci, sci.total_bands * 2);

    for i in sci.stereo_bands..sci.total_bands {
        sci.bitalloc[2 * i + 1] = 0;
    }
}

/// Dequantizes one group of samples into `grbuf[base..]`; returns how many
/// samples per subband it produced.
fn l12_dequantize_granule(
    grbuf: &mut [f32; 1152],
    base: usize,
    bs: &mut Bs,
    sci: &ScaleInfo,
    group_size: usize,
) -> usize {
    for j in 0..4 {
        for i in 0..2 * sci.total_bands {
            // C walks `dst` alternately +576 and -558: left band, right band,
            // next left band, ...
            let dst = &mut grbuf[base + group_size * j + (i / 2) * 18 + (i % 2) * 576..][..group_size];
            let ba = u32::from(sci.bitalloc[i]);
            if ba == 0 {
                continue;
            }
            if ba < 17 {
                let half = (1 << (ba - 1)) - 1;
                for d in dst {
                    *d = (bs.get_bits(ba) as i32 - half) as f32;
                }
            } else {
                let m = (2 << (ba - 17)) + 1; // 3, 5, 9
                let mut code = bs.get_bits(m + 2 - (m >> 3)); // 5, 7, 10 bits
                for d in dst {
                    *d = (code % m).wrapping_sub(m / 2) as i32 as f32;
                    code /= m;
                }
            }
        }
    }
    group_size * 4
}

fn l12_apply_scf_384(sci: &ScaleInfo, igr: usize, grbuf: &mut [f32; 1152]) {
    let (left, right) = grbuf.split_at_mut(576);
    let (from, to) = (sci.stereo_bands * 18, sci.total_bands * 18);
    right[from..to].copy_from_slice(&left[from..to]);
    for b in 0..sci.total_bands {
        let scf = &sci.scf[6 * b + igr..];
        for k in 0..12 {
            left[18 * b + k] *= scf[0];
            right[18 * b + k] *= scf[3];
        }
    }
}

/// The Layer I/II branch of `mp3dec_decode_frame`.
pub(super) fn decode_frame<S: Sample>(
    st: &mut State,
    scratch: &mut Scratch,
    hdr: Header,
    bs: &mut Bs,
    pcm: &mut [S],
    nch: usize,
    compat: bool,
) -> usize {
    let mut sci = ScaleInfo {
        scf: [0.; 3 * 64],
        total_bands: 0,
        stereo_bands: 0,
        bitalloc: [0; 64],
        scfcod: [0; 64],
    };
    l12_read_scale_info(hdr, bs, &mut sci);

    let group_size = if hdr.is_layer_1() { 1 } else { 3 };
    scratch.grbuf = [[0.; 576]; 2];
    let mut i = 0;
    let mut out = 0;
    for igr in 0..3 {
        let grbuf: &mut [f32; 1152] = scratch.grbuf.as_flattened_mut().try_into().unwrap();
        i += l12_dequantize_granule(grbuf, i, bs, &sci, group_size);
        if i == 12 {
            i = 0;
            l12_apply_scf_384(&sci, igr, grbuf);
            mp3d_synth_granule(&mut st.qmf_state, &mut scratch.grbuf, 12, nch, &mut pcm[out..], &mut scratch.syn, compat);
            scratch.grbuf = [[0.; 576]; 2];
            out += 384 * nch;
        }
        if bs.pos > bs.limit {
            st.header[0] = 0; // mp3dec_init
            return 0;
        }
    }
    hdr.frame_samples() as usize
}
