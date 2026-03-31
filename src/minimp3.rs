#![allow(
    clippy::all,
    non_camel_case_types,
    non_snake_case,
    unused_assignments
)]

mod tables;
use core::iter;

use tables::*;



#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct mp3dec_frame_info_t {
    pub frame_bytes: usize,
    pub frame_offset: usize,
    pub channels: u32,
    pub hz: i32,
    pub layer: u8,
    pub bitrate_kbps: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct mp3dec_t {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: i32,
    free_format_bytes: usize,
    header: [u8; 4],
    reserv_buf: [u8; 511],
}

impl mp3dec_t {
    pub const fn new() -> Self {
        Self {
            mdct_overlap: [[0.; 288]; 2],
            qmf_state: [0.; 960],
            reserv: 0,
            free_format_bytes: 0,
            header: [0; 4],
            reserv_buf: [0; 511]
        }
    }
}

type mp3d_sample_t = f32;
#[derive(Copy, Clone)]
#[repr(C)]
struct mp3dec_scratch_t {
    grbuf: [[f32; 576]; 2],
    scf: [f32; 40],
    syn: [[f32; 64]; 33],
    ist_pos: [[u8; 39]; 2],
}
#[derive(Copy, Clone)]
#[repr(C)]
struct L3_gr_info_t {
    sfbtab: *const u8,
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
#[derive(Copy, Clone)]
#[repr(C)]
struct bs_t<'a> {
    buf: &'a [u8],
    pos: i32,
    limit: i32
}
fn bs_init(
    data: &[u8],
    bytes: i32
) -> bs_t<'_> {
    bs_t {
        buf: data,
        pos: 0,
        limit: bytes * 8
    }
}

fn get_bits(bs: &mut bs_t, n: i32) -> u32 {
    let mut next: u32 = 0;
    let mut cache: u32 = 0 as i32 as u32;
    let s: u32 = (bs.pos & 7 as i32) as u32;
    let mut shl: i32 = (n as u32).wrapping_add(s) as i32;
    let mut p = &bs.buf[(bs.pos >> 3) as usize..];
    bs.pos += n;
    if bs.pos > bs.limit {
        return 0 as i32 as u32;
    }
    let fresh0 = p;
    p = &p[1..];
    next = (fresh0[0] as i32 & 255 as i32 >> s) as u32;
    loop {
        shl -= 8 as i32;
        if !(shl > 0 as i32) {
            break;
        }
        cache |= next << shl;
        let fresh1 = p;
        p = &p[1..];
        next = fresh1[0] as u32;
    }
    return cache | next >> -shl;
}

fn hdr_valid(h: &[u8]) -> bool {
    h[0] == 0xff &&
        (h[1] & 0xf0 == 0xf0 || h[1] & 0xfe == 0xe2) &&
        h[1] >> 1 & 3 != 0 &&
        h[2] >> 4 != 15 &&
        h[2] >> 2 & 3 != 3
}

fn hdr_compare(
    h1: &[u8],
    h2: &[u8],
) -> bool {
    hdr_valid(h2) &&
        (h1[1] ^ h2[1]) & 0xfe == 0 &&
        (h1[2] ^ h2[2]) & 0xc == 0 &&
        (h1[2] & 0xf0 == 0) == (h2[2] & 0xf0 == 0)
}

fn hdr_bitrate_kbps(h: &[u8]) -> u32 {
    2 * (HDR_BITRATE_KBPS_HALFRATE
        [(h[1] & 0x8 != 0) as usize]
        [((h[1] >> 1 & 3) - 1) as usize]
        [(h[2] >> 4) as usize] as u32)
}

fn hdr_sample_rate_hz(h: &[u8]) -> u32 {
    HDR_SAMPLE_RATE_HZ_G_HZ[(h[2] >> 2 & 3) as usize]
        >> (h[1] & 0x8 == 0) as u32
        >> (h[1] & 0x10 == 0) as u32
}

fn hdr_frame_samples(h: &[u8]) -> u32 {
    if h[1] & 6 == 6 {
        384
    } else {
        1152 >> (h[1] & 14 == 2) as u32
    }
}

fn hdr_frame_bytes(
    h: &[u8],
    free_format_size: usize,
) -> usize {
    let mut frame_bytes = (hdr_frame_samples(h))
        .wrapping_mul(hdr_bitrate_kbps(h))
        .wrapping_mul(125)
        .wrapping_div(hdr_sample_rate_hz(h)) as usize;
    if h[1] & 6 == 6 {
        frame_bytes &= !3;
    }
    if frame_bytes != 0 { frame_bytes } else { free_format_size }
}

fn hdr_padding(h: &[u8]) -> usize {
    if h[2] & 0x2 != 0 {
        if h[1] & 6 == 6 {
            4
        } else {
            1
        }
    } else {
        0
    }
}

fn L3_read_side_info(
    bs: &mut bs_t,
    mut gr: &mut [L3_gr_info_t],
    hdr: &[u8],
) -> i32 {
    let mut tables: u32 = 0;
    let mut scfsi: u32 = 0 as i32 as u32;
    let mut main_data_begin: i32 = 0;
    let mut part_23_sum: i32 = 0 as i32;
    let mut sr_idx: i32 = (hdr[2] as i32
        >> 2 as i32 & 3 as i32)
        + ((hdr[1] as i32 >> 3 as i32
            & 1 as i32)
            + (hdr[1] as i32 >> 4 as i32
                & 1 as i32)) * 3 as i32;
    sr_idx -= (sr_idx != 0 as i32) as i32;
    let mut gr_count: i32 = if hdr[3] & 0xc0 == 0xc0 {
        1
    } else {
        2
    };
    if hdr[1] & 0x8 != 0 {
        gr_count *= 2 as i32;
        main_data_begin = get_bits(bs, 9 as i32) as i32;
        scfsi = get_bits(bs, 7 as i32 + gr_count);
    } else {
        main_data_begin = (get_bits(bs, 8 as i32 + gr_count) >> gr_count)
            as i32;
    }
    loop {
        if hdr[3] & 0xc0 == 0xc0 {
            scfsi <<= 4 as i32;
        }
        gr[0].part_23_length = get_bits(bs, 12 as i32) as u16;
        part_23_sum += gr[0].part_23_length as i32;
        gr[0].big_values = get_bits(bs, 9 as i32) as u16;
        if gr[0].big_values as i32 > 288 as i32 {
            return -(1 as i32);
        }
        gr[0].global_gain = get_bits(bs, 8 as i32) as u8;
        gr[0]
            .scalefac_compress = get_bits(
            bs,
            if hdr[1] & 0x8 != 0 {
                4 as i32
            } else {
                9 as i32
            },
        ) as u16;
        gr[0].sfbtab = (L3_READ_SIDE_INFO_G_SCF_LONG[sr_idx as usize]).as_ptr();
        gr[0].n_long_sfb = 22 as i32 as u8;
        gr[0].n_short_sfb = 0 as i32 as u8;
        if get_bits(bs, 1 as i32) != 0 {
            gr[0].block_type = get_bits(bs, 2 as i32) as u8;
            if gr[0].block_type == 0 {
                return -(1 as i32);
            }
            gr[0].mixed_block_flag = get_bits(bs, 1 as i32) as u8;
            gr[0].region_count[0 as i32 as usize] = 7 as i32 as u8;
            gr[0]
                .region_count[1 as i32 as usize] = 255 as i32 as u8;
            if gr[0].block_type as i32 == 2 as i32 {
                scfsi &= 0xf0f as i32 as u32;
                if gr[0].mixed_block_flag == 0 {
                    gr[0]
                        .region_count[0 as i32
                        as usize] = 8 as i32 as u8;
                    gr[0].sfbtab = (L3_READ_SIDE_INFO_G_SCF_SHORT[sr_idx as usize]).as_ptr();
                    gr[0].n_long_sfb = 0 as i32 as u8;
                    gr[0].n_short_sfb = 39 as i32 as u8;
                } else {
                    gr[0].sfbtab = (L3_READ_SIDE_INFO_G_SCF_MIXED[sr_idx as usize]).as_ptr();
                    gr[0]
                        .n_long_sfb = (if hdr[1] & 0x8 != 0 {
                        8 as i32
                    } else {
                        6 as i32
                    }) as u8;
                    gr[0].n_short_sfb = 30 as i32 as u8;
                }
            }
            tables = get_bits(bs, 10 as i32);
            tables <<= 5 as i32;
            gr[0]
                .subblock_gain[0 as i32
                as usize] = get_bits(bs, 3 as i32) as u8;
            gr[0]
                .subblock_gain[1 as i32
                as usize] = get_bits(bs, 3 as i32) as u8;
            gr[0]
                .subblock_gain[2 as i32
                as usize] = get_bits(bs, 3 as i32) as u8;
        } else {
            gr[0].block_type = 0 as i32 as u8;
            gr[0].mixed_block_flag = 0 as i32 as u8;
            tables = get_bits(bs, 15 as i32);
            gr[0]
                .region_count[0 as i32
                as usize] = get_bits(bs, 4 as i32) as u8;
            gr[0]
                .region_count[1 as i32
                as usize] = get_bits(bs, 3 as i32) as u8;
            gr[0]
                .region_count[2 as i32 as usize] = 255 as i32 as u8;
        }
        gr[0]
            .table_select[0 as i32
            as usize] = (tables >> 10 as i32) as u8;
        gr[0]
            .table_select[1 as i32
            as usize] = (tables >> 5 as i32 & 31 as i32 as u32)
            as u8;
        gr[0]
            .table_select[2 as i32
            as usize] = (tables & 31 as i32 as u32) as u8;
        gr[0]
            .preflag = (if hdr[1] & 0x8 != 0 {
            get_bits(bs, 1 as i32)
        } else {
            (gr[0].scalefac_compress as i32 >= 500 as i32) as i32
                as u32
        }) as u8;
        gr[0].scalefac_scale = get_bits(bs, 1 as i32) as u8;
        gr[0].count1_table = get_bits(bs, 1 as i32) as u8;
        gr[0]
            .scfsi = (scfsi >> 12 as i32 & 15 as i32 as u32)
            as u8;
        scfsi <<= 4 as i32;
        gr = &mut gr[1..];
        gr_count -= 1;
        if !(gr_count != 0) {
            break;
        }
    }
    if part_23_sum + bs.pos > bs.limit + main_data_begin * 8 {
        return -(1 as i32);
    }
    return main_data_begin;
}

fn L3_read_scalefactors(
    scf: &mut [u8],
    ist_pos: &mut [u8],
    scf_size: &[u8],
    scf_count: &[u8],
    bitbuf: &mut bs_t,
    mut scfsi: i32,
) {
    let mut scf_idx = 0usize;
    let mut ist_idx = 0usize;

    for i in 0..4 {
        if i >= scf_count.len() || scf_count[i] == 0 {
            break;
        }

        let cnt = scf_count[i] as usize;

        // Ensure we don't go out of bounds
        let scf_slice = &mut scf[scf_idx..scf_idx + cnt];
        let ist_slice = &mut ist_pos[ist_idx..ist_idx + cnt];

        if scfsi & 8 != 0 {
            // memcpy(scf, ist_pos)
            scf_slice.copy_from_slice(ist_slice);
        } else {
            let bits = scf_size[i] as i32;

            if bits == 0 {
                scf_slice.fill(0);
                ist_slice.fill(0);
            } else {
                let max_scf = if scfsi < 0 {
                    (1 << bits) - 1
                } else {
                    -1
                };

                for k in 0..cnt {
                    let s = get_bits(bitbuf, bits) as i32;

                    ist_slice[k] = if s == max_scf {
                        (-1i32) as u8
                    } else {
                        s as u8
                    };

                    scf_slice[k] = s as u8;
                }
            }
        }

        scf_idx += cnt;
        ist_idx += cnt;
        scfsi *= 2;
    }

    // Final 3 zero values (matches original tail writes)
    if scf.len() >= 3 {
        scf[0] = 0;
        scf[1] = 0;
        scf[2] = 0;
    }
}

fn L3_ldexp_q2(
    mut y: f32,
    mut exp_q2: i32,
) -> f32 {
    let mut e: i32 = 0;
    loop {
        e = if 30 as i32 * 4 as i32 > exp_q2 {
            exp_q2
        } else {
            30 as i32 * 4 as i32
        };
        y
            *= L3_LDEXP_Q2_G_EXPFRAC[(e & 3 as i32) as usize]
                * ((1 as i32) << 30 as i32 >> (e >> 2 as i32))
                    as f32;
        exp_q2 -= e;
        if !(exp_q2 > 0 as i32) {
            break;
        }
    }
    return y;
}

fn L3_decode_scalefactors(
    hdr: &[u8],
    ist_pos: &mut [u8],
    bs: &mut bs_t,
    gr: &L3_gr_info_t,
    scf: &mut [f32],
    ch: u32,
) {
    let mut scf_partition: &[u8] = &L3_DECODE_SCALEFACTORS_G_SCM_PARTITIONS
        [(gr.n_short_sfb != 0) as usize + (gr.n_long_sfb == 0) as usize];

    let mut scf_size = [0u8; 4];
    let mut iscf = [0u8; 40];

    let scf_shift = gr.scalefac_scale as i32 + 1;
    let mut scfsi = gr.scfsi as i32;

    // --- decode scalefactor sizes ---
    if hdr[1] & 0x8 != 0 {
        let part = L3_DECODE_SCALEFACTORS_G_SCFC_DECODE[gr.scalefac_compress as usize] as i32;
        scf_size[0] = (part >> 2) as u8;
        scf_size[1] = scf_size[0];
        scf_size[2] = (part & 3) as u8;
        scf_size[3] = scf_size[2];
    } else {
        let ist = ((hdr[3] & 0x10 != 0) && ch != 0) as i32;
        let mut sfc = (gr.scalefac_compress as i32) >> ist;
        let mut k = ist * 3 * 4;

        while sfc >= 0 {
            let mut modprod = 1;
            for i in (0..4).rev() {
                let m = L3_DECODE_SCALEFACTORS_G_MOD[(k + i) as usize] as i32;
                scf_size[i as usize] = ((sfc / modprod) % m) as u8;
                modprod *= m;
            }
            sfc -= modprod;
            k += 4;
        }

        scf_partition = &scf_partition[k as usize..];
        scfsi = -16;
    }

    // --- read scalefactors ---
    L3_read_scalefactors(
        &mut iscf,
        ist_pos,
        &mut scf_size,
        scf_partition,
        bs,
        scfsi,
    );

    // --- apply short block gain ---
    if gr.n_short_sfb != 0 {
        let sh = 3 - scf_shift;
        let base = gr.n_long_sfb as usize;

        for i in (0..gr.n_short_sfb as usize).step_by(3) {
            iscf[base + i + 0] =
                (iscf[base + i + 0] as i32 + ((gr.subblock_gain[0] as i32) << sh)) as u8;
            iscf[base + i + 1] =
                (iscf[base + i + 1] as i32 + ((gr.subblock_gain[1] as i32) << sh)) as u8;
            iscf[base + i + 2] =
                (iscf[base + i + 2] as i32 + ((gr.subblock_gain[2] as i32) << sh)) as u8;
        }
    } else if gr.preflag != 0 {
        for i in 0..10 {
            iscf[11 + i] =
                (iscf[11 + i] as i32 + L3_DECODE_SCALEFACTORS_G_PREAMP[i] as i32) as u8;
        }
    }

    // --- compute gain ---
    let gain_exp = gr.global_gain as i32
        - 4
        - 210
        - if hdr[3] & 0xe0 == 0x60 { 2 } else { 0 };

    let base = (255 - 4 - 210 + 3) & !3;

    let gain = L3_ldexp_q2(
        (1 << (base / 4)) as f32,
        base - gain_exp,
    );

    // --- apply scalefactors ---
    let total = (gr.n_long_sfb + gr.n_short_sfb) as usize;
    for i in 0..total {
        scf[i] = L3_ldexp_q2(gain, (iscf[i] as i32) << scf_shift);
    }
}

fn L3_pow_43(mut x: i32) -> f32 {
    let mut frac: f32 = 0.;
    let mut sign: i32 = 0;
    let mut mult: i32 = 256 as i32;
    if x < 129 as i32 {
        return G_POW43[(16 as i32 + x) as usize];
    }
    if x < 1024 as i32 {
        mult = 16 as i32;
        x <<= 3 as i32;
    }
    sign = 2 as i32 * x & 64 as i32;
    frac = ((x & 63 as i32) - sign) as f32
        / ((x & !(63 as i32)) + sign) as f32;
    return G_POW43[(16 as i32 + (x + sign >> 6 as i32)) as usize]
        * (1.0f32
            + frac
                * (4.0f32 / 3 as i32 as f32
                    + frac * (2.0f32 / 9 as i32 as f32)))
        * mult as f32;
}

fn L3_stereo_top_band(
    mut right: &[f32],
    sfb: &[u8],
    nbands: i32,
    max_band: &mut [i32; 3],
) {
    // initialize
    max_band.fill(-1);

    for i in 0..(nbands as usize) {
        if i >= sfb.len() {
            break;
        }

        let band_len = sfb[i] as usize;

        let mut k = 0;
        while k + 1 < band_len && k + 1 < right.len() {
            if right[k] != 0.0 || right[k + 1] != 0.0 {
                max_band[i % 3] = i as i32;
                break;
            }
            k += 2;
        }

        if band_len > right.len() {
            break;
        }

        right = &right[band_len..];
    }
}

fn L3_intensity_stereo(
    left: &mut [f32],
    ist_pos: &mut [u8],
    gr: &[L3_gr_info_t],
    hdr: &[u8],
) {
    let mut max_band = [-1i32; 3];

    let n_sfb = (gr[0].n_long_sfb + gr[0].n_short_sfb) as usize;
    let max_blocks = if gr[0].n_short_sfb != 0 { 3 } else { 1 };

    let sfb = unsafe {
        // still needed unless you change sfbtab type
        core::slice::from_raw_parts(gr[0].sfbtab, n_sfb)
    };

    // right channel is second half (must exist)
    if left.len() < 576 {
        return;
    }

    let right = &left[576..];

    L3_stereo_top_band(
        right,
        sfb,
        n_sfb as i32,
        &mut max_band,
    );

    // normalize max_band if long blocks present
    if gr[0].n_long_sfb != 0 {
        let m = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [m, m, m];
    }

    for i in 0..max_blocks {
        let default_pos = if hdr[1] & 0x8 != 0 { 3 } else { 0 };

        let itop = n_sfb as i32 - max_blocks + i as i32;
        let prev = itop - max_blocks;

        let new_val = if (i < max_band.len().try_into().unwrap()) && max_band[<i32 as TryInto<usize>>::try_into(i).unwrap()] >= prev {
            default_pos
        } else {
            ist_pos.get(prev as usize).copied().unwrap_or(0) as i32
        };

        if let Some(slot) = ist_pos.get_mut(itop as usize) {
            *slot = new_val as u8;
        }
    }

    L3_stereo_process(
        left,
        ist_pos,
        sfb,
        hdr,
        &max_band,
        gr[1].scalefac_compress as i32 & 1,
    );
}

fn L3_stereo_process(
    mut left: &mut [f32],
    ist_pos: &[u8],
    sfb: &[u8],
    hdr: &[u8],
    max_band: &[i32; 3],
    mpeg2_sh: i32,
) {
    let max_pos = if hdr[1] & 0x8 != 0 { 7 } else { 64 };

    let mut i = 0usize;

    while i < sfb.len() && sfb[i] != 0 {
        let band_len = sfb[i] as usize;
        let ipos = ist_pos.get(i).copied().unwrap_or(0) as u32;

        if i as i32 > max_band[i % 3] && ipos < max_pos {
            let s = if hdr[3] & 0x20 != 0 {
                1.41421356f32
            } else {
                1.0
            };

            let (mut kl, mut kr);

            if hdr[1] & 0x8 != 0 {
                let idx = (2 * ipos) as usize;
                kl = L3_STEREO_PROCESS_G_PAN.get(idx).copied().unwrap_or(0.0);
                kr = L3_STEREO_PROCESS_G_PAN.get(idx + 1).copied().unwrap_or(0.0);
            } else {
                kl = 1.0;
                kr = L3_ldexp_q2(
                    1.0,
                    (((ipos + 1) >> 1) << mpeg2_sh) as i32,
                );

                if ipos & 1 != 0 {
                    kl = kr;
                    kr = 1.0;
                }
            }

            L3_intensity_stereo_band(
                left,
                band_len,
                kl * s,
                kr * s,
            );
        } else if hdr[3] & 0x20 != 0 {
            L3_midside_stereo(left, band_len);
        }

        if band_len > left.len() {
            break;
        }

        left = &mut left[band_len..];
        i += 1;
    }
}

#[derive(Copy, Clone)]
struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    fn new(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    fn read_bits(&mut self, n: u32) -> u32 {
        let mut out = 0;

        for _ in 0..n {
            let byte = self.pos / 8;
            let bit = 7 - (self.pos % 8);

            let val = if byte < self.buf.len() {
                (self.buf[byte] >> bit) & 1
            } else {
                0
            };

            out = (out << 1) | val as u32;
            self.pos += 1;
        }

        out
    }

    fn peek_bits(&self, n: u32) -> u32 {
        let mut tmp = self.clone();
        tmp.read_bits(n)
    }
}

fn L3_huffman(
    dst: &mut [f32],
    bs: &mut bs_t,
    gr_info: &L3_gr_info_t,
    scf: &[f32],
    layer3gr_limit: i32,
) {
    let mut br = BitReader::new(&bs.buf, bs.pos as usize);

    let mut dst_idx = 0usize;
    let mut big_val_cnt = gr_info.big_values as i32;

    let sfb = unsafe {
        core::slice::from_raw_parts(gr_info.sfbtab, 64)
    };

    let mut sfb_idx = 0usize;
    let mut scf_idx = 0usize;

    let mut ireg = 0usize;

    // --- BIG VALUES ---
    while big_val_cnt > 0 && ireg < 3 {
        let tab_num = gr_info.table_select[ireg] as usize;
        let mut sfb_cnt = gr_info.region_count[ireg] as i32;
        ireg += 1;

        let codebook =
            &L3_HUFFMAN_TABS[L3_HUFFMAN_TABINDEX[tab_num] as usize..];

        let linbits = L3_HUFFMAN_G_LINBITS[tab_num] as i32;

        while big_val_cnt > 0 && sfb_cnt >= 0 {
            if sfb_idx >= sfb.len() || scf_idx >= scf.len() {
                break;
            }

            let np = (sfb[sfb_idx] as i32) / 2;
            let pairs = big_val_cnt.min(np);

            let one = scf[scf_idx];
            scf_idx += 1;
            sfb_idx += 1;

            for _ in 0..pairs {
                // simplified safe decode
                let mut leaf = codebook
                    .get(br.peek_bits(5) as usize)
                    .copied()
                    .unwrap_or(0) as i32;

                let mut w = 5;

                while leaf < 0 {
                    br.read_bits(w as u32);
                    w = leaf & 7;

                    let idx =
                        (br.peek_bits(w as u32) as i32 - (leaf >> 3)) as usize;

                    leaf = *codebook.get(idx).unwrap_or(&0) as i32;
                }

                br.read_bits((leaf >> 8) as u32);

                for _ in 0..2 {
                    if dst_idx >= dst.len() {
                        break;
                    }

                    let mut val = leaf & 0xF;

                    if val == 15 {
                        val += br.read_bits(linbits as u32) as i32;
                        let sign = if br.read_bits(1) != 0 { -1.0 } else { 1.0 };
                        dst[dst_idx] = one * L3_pow_43(val) * sign;
                    } else {
                        let sign = if br.read_bits(1) != 0 { -1.0 } else { 1.0 };
                        dst[dst_idx] =
                            G_POW43[val as usize] * one * sign;
                    }

                    dst_idx += 1;
                    leaf >>= 4;
                }
            }

            big_val_cnt -= np;
            sfb_cnt -= 1;
        }
    }

    // --- COUNT1 REGION ---
    let table: &[u8] = if gr_info.count1_table != 0 {
        &L3_HUFFMAN_TAB33
    } else {
        &L3_HUFFMAN_TAB32
    };

    let mut np = 1 - big_val_cnt;

    while (br.pos as i32) < layer3gr_limit && dst_idx + 4 <= dst.len() {
        let mut leaf = table
            .get(br.peek_bits(4) as usize)
            .copied()
            .unwrap_or(0) as i32;

        if leaf & 8 == 0 {
            let idx =
                ((leaf >> 3) + br.read_bits((leaf & 3) as u32) as i32) as usize;
            leaf = *table.get(idx).unwrap_or(&0) as i32;
        }

        br.read_bits((leaf & 7) as u32);

        for i in 0..4 {
            if leaf & (8 >> i) != 0 {
                let sign = if br.read_bits(1) != 0 { -1.0 } else { 1.0 };
                dst[dst_idx + i] = sign;
            } else {
                dst[dst_idx + i] = 0.0;
            }
        }

        dst_idx += 4;

        np -= 1;
        if np <= 0 {
            break;
        }
    }

    bs.pos = layer3gr_limit;
}

fn L3_midside_stereo(
    left: &mut [f32],
    n: usize,
) {
    let (left, right) = left.split_at_mut(576);
    for (l, r) in iter::zip(left, right).take(n as usize) {
        let a = *l;
        let b = *r;
        *l = a + b;
        *r = a - b;
    }
}

fn L3_intensity_stereo_band(
    left: &mut [f32],
    n: usize,
    kl: f32,
    kr: f32,
) {
    for i in 0..n {
        left[i + 576] = left[i] * kr;
        left[i] = left[i] * kl;
    }
}



fn L3_reorder(
    grbuf: &mut [f32],
    scratch: &mut [f32],
    sfb: &[u8],
) {
    let mut src_idx = 0usize;
    let mut dst_idx = 0usize;
    let mut sfb_idx = 0usize;

    // process bands
    while sfb_idx < sfb.len() {
        let len = sfb[sfb_idx] as usize;
        if len == 0 {
            break;
        }

        // ensure we have enough sfb entries for next step
        if sfb_idx + 3 > sfb.len() {
            break;
        }

        for i in 0..len {
            let base = src_idx + i;

            let get = |idx: usize| -> f32 {
                grbuf.get(idx).copied().unwrap_or(0.0)
            };

            if dst_idx + 3 > scratch.len() {
                return;
            }

            scratch[dst_idx]     = get(base + 0 * len);
            scratch[dst_idx + 1] = get(base + 1 * len);
            scratch[dst_idx + 2] = get(base + 2 * len);

            dst_idx += 3;
        }

        src_idx += len;         // from loop (src = src.offset(1))
        src_idx += 2 * len;     // final jump
        sfb_idx += 3;
    }

    // copy scratch → grbuf
    let count = dst_idx.min(grbuf.len()).min(scratch.len());

    grbuf[..count].copy_from_slice(&scratch[..count]);
}
fn L3_antialias(
    mut grbuf: &mut [f32],
    nbands: i32,
) {
    for _ in 0..nbands {
        let mut i: i32 = 0 as i32;
        while i < 8 as i32 {
            let u: f32 = grbuf[(18 as i32 + i) as usize];
            let d: f32 = grbuf[(17 as i32 - i) as usize];
            grbuf[(18 as i32 + i) as usize] = u * L3_ANTIALIAS_G_AA[0 as i32 as usize][i as usize]
                - d * L3_ANTIALIAS_G_AA[1 as i32 as usize][i as usize];
            grbuf[(17 as i32 - i) as usize] = u * L3_ANTIALIAS_G_AA[1 as i32 as usize][i as usize]
                + d * L3_ANTIALIAS_G_AA[0 as i32 as usize][i as usize];
            i += 1;
        }
        grbuf = &mut grbuf[18..];
    }
}

fn L3_dct3_9(y: &mut [f32]) {
    let mut s0: f32 = 0.;
    let mut s1: f32 = 0.;
    let mut s2: f32 = 0.;
    let mut s3: f32 = 0.;
    let mut s4: f32 = 0.;
    let mut s5: f32 = 0.;
    let mut s6: f32 = 0.;
    let mut s7: f32 = 0.;
    let mut s8: f32 = 0.;
    let mut t0: f32 = 0.;
    let mut t2: f32 = 0.;
    let mut t4: f32 = 0.;
    s0 = y[0];
    s2 = y[2];
    s4 = y[4];
    s6 = y[6];
    s8 = y[8];
    t0 = s0 + s6 * 0.5f32;
    s0 -= s6;
    t4 = (s4 + s2) * 0.93969262f32;
    t2 = (s8 + s2) * 0.76604444f32;
    s6 = (s4 - s8) * 0.17364818f32;
    s4 += s8 - s2;
    s2 = s0 - s4 * 0.5f32;
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;
    s1 = y[1];
    s3 = y[3];
    s5 = y[5];
    s7 = y[7];
    s3 *= 0.86602540f32;
    t0 = (s5 + s1) * 0.98480775f32;
    t4 = (s5 - s7) * 0.34202014f32;
    t2 = (s1 + s7) * 0.64278761f32;
    s1 = (s1 - s5 - s7) * 0.86602540f32;
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

fn L3_imdct36(
    mut grbuf: &mut [f32],
    mut overlap: &mut [f32],
    window: &[f32],
    nbands: i32,
) {
    let nbands = nbands as usize;

    for _ in 0..nbands {
        if grbuf.len() < 18 || overlap.len() < 9 || window.len() < 18 {
            return;
        }

        let mut co = [0.0f32; 9];
        let mut si = [0.0f32; 9];

        // --- input reordering ---
        co[0] = -grbuf[0];
        si[0] = grbuf[17];

        for i in 0..4 {
            si[8 - 2 * i] = grbuf[4 * i + 1] - grbuf[4 * i + 2];
            co[1 + 2 * i] = grbuf[4 * i + 1] + grbuf[4 * i + 2];

            si[7 - 2 * i] = grbuf[4 * i + 4] - grbuf[4 * i + 3];
            co[2 + 2 * i] = -(grbuf[4 * i + 3] + grbuf[4 * i + 4]);
        }

        // --- transforms ---
        L3_dct3_9(&mut co);
        L3_dct3_9(&mut si);

        // sign flips
        si[1] = -si[1];
        si[3] = -si[3];
        si[5] = -si[5];
        si[7] = -si[7];

        // --- window + overlap ---
        for i in 0..9 {
            let ovl = overlap[i];

            let sum =
                co[i] * L3_IMDCT36_G_TWID9[9 + i]
                + si[i] * L3_IMDCT36_G_TWID9[i];

            overlap[i] =
                co[i] * L3_IMDCT36_G_TWID9[i]
                - si[i] * L3_IMDCT36_G_TWID9[9 + i];

            grbuf[i] =
                ovl * window[i]
                - sum * window[9 + i];

            grbuf[17 - i] =
                ovl * window[9 + i]
                + sum * window[i];
        }

        // advance to next band
        grbuf = &mut grbuf[18..];
        overlap = &mut overlap[9..];
    }
}

fn L3_idct3(
    x0: f32,
    x1: f32,
    x2: f32,
    dst: &mut [f32],
) {
    let m1: f32 = x1 * 0.86602540f32;
    let a1: f32 = x0 - x2 * 0.5f32;
    dst[1] = x0 + x2;
    dst[0] = a1 + m1;
    dst[2] = a1 - m1;
}

fn L3_change_sign(mut grbuf: &mut [f32]) {
    let mut b = 0u32;
    let mut i = 0usize;
    grbuf = &mut grbuf[18..];
    while b < 32 {
        i = 1;
        while i < 18 {
            grbuf[i] = -grbuf[i];
            i += 2;
        }
        b += 2;
        if b < 32 { grbuf = &mut grbuf[36..]; }
    }
}


fn L3_imdct12(
    x: &[f32],
    dst: &mut [f32],
    overlap: &mut [f32],
) {
    if x.len() < 15 || dst.len() < 6 || overlap.len() < 3 {
        return;
    }

    let mut co = [0.0f32; 3];
    let mut si = [0.0f32; 3];

    L3_idct3(
        -x[0],
        x[6] + x[3],
        x[12] + x[9],
        &mut co,
    );

    L3_idct3(
        x[15],
        x[12] - x[9],
        x[6] - x[3],
        &mut si,
    );

    si[1] = -si[1];

    for i in 0..3 {
        let ovl = overlap[i];

        let sum =
            co[i] * L3_IMDCT12_G_TWID3[3 + i]
            + si[i] * L3_IMDCT12_G_TWID3[i];

        overlap[i] =
            co[i] * L3_IMDCT12_G_TWID3[i]
            - si[i] * L3_IMDCT12_G_TWID3[3 + i];

        dst[i] =
            ovl * L3_IMDCT12_G_TWID3[2 - i]
            - sum * L3_IMDCT12_G_TWID3[5 - i];

        dst[5 - i] =
            ovl * L3_IMDCT12_G_TWID3[5 - i]
            + sum * L3_IMDCT12_G_TWID3[2 - i];
    }
}

fn L3_imdct_short(
    mut grbuf: &mut [f32],
    mut overlap: &mut [f32],
    mut nbands: i32,
) {
    while nbands > 0 {
        if grbuf.len() < 18 || overlap.len() < 12 {
            break;
        }

        let mut tmp = [0.0f32; 18];
        tmp.copy_from_slice(&grbuf[..18]);

        // overlap → grbuf (first 6)
        grbuf[..6].copy_from_slice(&overlap[..6]);

        // Split grbuf into disjoint parts
        let (gr_head, gr_tail) = grbuf.split_at_mut(18);
        let (_gr_first6, gr_rest) = gr_head.split_at_mut(6);    // gr_head[..6]
        let (gr_6, gr_12) = gr_rest.split_at_mut(6);           // gr_head[6..12], gr_head[12..18]

        // Split overlap into disjoint parts
        let (ov_head, ov_rest) = overlap.split_at_mut(6);      // overlap[..6]
        let (ov_mid, _) = ov_rest.split_at_mut(6);            // overlap[6..12]

        // IMDCT calls (all borrows now disjoint)
        L3_imdct12(&tmp, gr_6, ov_mid);
        L3_imdct12(&tmp[1..], gr_12, ov_mid);
        L3_imdct12(&tmp[2..], ov_head, ov_mid);

        nbands -= 1;

        // Advance slices safely
        grbuf = gr_tail;
        overlap = &mut overlap[9..];
    }
}

fn L3_imdct_gr(
    grbuf: &mut [f32],
    overlap: &mut [f32],
    block_type: u32,
    n_long_bands: u32,
) {
    let n_long_bands = n_long_bands as usize;

    let mut grbuf = grbuf;
    let mut overlap = overlap;

    // --- long blocks ---
    if n_long_bands != 0 {
        let gr_needed = 18 * n_long_bands;
        let ov_needed = 9 * n_long_bands;

        if grbuf.len() < gr_needed || overlap.len() < ov_needed {
            return;
        }

        L3_imdct36(
            &mut grbuf[..gr_needed],
            &mut overlap[..ov_needed],
            &L3_IMDCT_GR_G_MDCT_WINDOW[0],
            n_long_bands as i32,
        );

        // advance slices instead of pointer offset
        grbuf = &mut grbuf[gr_needed..];
        overlap = &mut overlap[ov_needed..];
    }

    // --- remaining bands ---
    let remaining = 32usize.saturating_sub(n_long_bands);

    if remaining == 0 {
        return;
    }

    if block_type == 2 {
        L3_imdct_short(
            grbuf,
            overlap,
            remaining as i32,
        );
    } else {
        let win_idx = if block_type == 3 { 1 } else { 0 };

        L3_imdct36(
            grbuf,
            overlap,
            &L3_IMDCT_GR_G_MDCT_WINDOW[win_idx],
            remaining as i32,
        );
    }
}
fn L3_save_reservoir(
    h: &mut mp3dec_t,
    s_bs: &mut bs_t
) {
    let mut pos: i32 = ((s_bs.pos + 7 as i32) as u32)
        .wrapping_div(8 as u32) as i32;
    let mut remains: i32 = (s_bs.limit as u32)
        .wrapping_div(8 as u32)
        .wrapping_sub(pos as u32) as i32;
    if remains > 511 as i32 {
        pos += remains - 511 as i32;
        remains = 511 as i32;
    }
    if remains > 0 as i32 {
        h.reserv_buf[..remains as usize].copy_from_slice(&s_bs.buf[pos as usize..(pos+remains) as usize]);
    }
    (*h).reserv = remains;
}

fn L3_restore_reservoir<'a>(
    h: &mut mp3dec_t,
    bs: &mut bs_t,
    s_maindata: &'a mut [u8; 2815],
    s_bs: &mut bs_t<'a>,
    main_data_begin: i32,
) -> i32 {
    let frame_bytes: i32 = (bs.limit - bs.pos) / 8 as i32;
    let bytes_have: i32 = if (*h).reserv > main_data_begin {
        main_data_begin
    } else {
        (*h).reserv
    };

    {
        let off = if (0 as i32) < (*h).reserv - main_data_begin {
            (*h).reserv - main_data_begin
        } else {
            0 as i32
        };
        let cnt = if (*h).reserv > main_data_begin { main_data_begin } else { (*h).reserv };
        s_maindata[..cnt as usize].copy_from_slice(&h.reserv_buf[off as usize..off as usize + cnt as usize]);
    }

    s_maindata[bytes_have as usize..bytes_have as usize + frame_bytes as usize]
        .copy_from_slice(&bs.buf[(bs.pos / 8) as usize..(bs.pos / 8) as usize + frame_bytes as usize]);

    *s_bs = bs_init(&s_maindata[..], bytes_have + frame_bytes);
    return ((*h).reserv >= main_data_begin) as i32;
}
fn L3_decode(
    h: &mut mp3dec_t,
    s: &mut mp3dec_scratch_t,
    s_bs: &mut bs_t,
    mut gr_info: &mut [L3_gr_info_t],
    nch: u32,
) {
    let nch = nch as usize;

    // --- per-channel decode ---
    for ch in 0..nch {
        if ch >= gr_info.len() {
            return;
        }

        let gi = &gr_info[ch];

        let layer3gr_limit = s_bs.pos + gi.part_23_length as i32;

        L3_decode_scalefactors(
            &h.header,
            &mut s.ist_pos[ch],
            s_bs,
            gi,
            &mut s.scf,
            ch as u32,
        );

        L3_huffman(
            &mut s.grbuf[ch],
            s_bs,
            gi,
            &mut s.scf,
            layer3gr_limit,
        );
    }

    // --- stereo processing ---
    if h.header.get(3).copied().unwrap_or(0) & 0x10 != 0 {
        L3_intensity_stereo(
            s.grbuf.as_flattened_mut(),
            &mut s.ist_pos[1],
            gr_info,
            &h.header,
        );
    } else if h.header.get(3).copied().unwrap_or(0) & 0xe0 == 0x60 {
        L3_midside_stereo(
            s.grbuf.as_flattened_mut(),
            576,
        );
    }

    // --- per-channel post-processing ---
    for ch in 0..nch {
        if gr_info.is_empty() {
            return;
        }

        let gi = &gr_info[0];

        let mut aa_bands = 31;

        let header1 = h.header.get(1).copied().unwrap_or(0) as i32;
        let header2 = h.header.get(2).copied().unwrap_or(0) as i32;

        let n_long_bands =
            (if gi.mixed_block_flag != 0 { 2 } else { 0 })
                << (((header2 >> 2 & 3)
                    + (((header1 >> 3 & 1) + (header1 >> 4 & 1)) * 3)
                    == 2) as i32);

        if gi.n_short_sfb != 0 {
            aa_bands = n_long_bands - 1;

            let start = (n_long_bands * 18) as usize;

            if start < s.grbuf[ch].len() {
                // Create a safe slice from the raw pointer
                let sfb_slice = unsafe { core::slice::from_raw_parts(gi.sfbtab, (gi.n_long_sfb + gi.n_short_sfb) as usize) };
                L3_reorder(
                    &mut s.grbuf[ch][start..],
                    s.syn.as_flattened_mut(),
                    sfb_slice,
                );
            }
        }

        L3_antialias(&mut s.grbuf[ch], aa_bands);

        L3_imdct_gr(
            &mut s.grbuf[ch],
            &mut h.mdct_overlap[ch],
            gi.block_type as u32,
            n_long_bands as u32,
        );

        L3_change_sign(&mut s.grbuf[ch]);

        gr_info = &mut gr_info[1..];
    }
}

fn mp3d_DCT_II(grbuf: &mut [f32], n: u32) {
    let n = n as usize;

    for k in 0..n {
        let mut t = [[0.0f32; 8]; 4];

        // --- first stage ---
        for i in 0..8 {
            let base = k;

            let get = |idx: usize| -> f32 {
                grbuf.get(base + idx * 18).copied().unwrap_or(0.0)
            };

            let x0 = get(i);
            let x1 = get(15 - i);
            let x2 = get(16 + i);
            let x3 = get(31 - i);

            let t0 = x0 + x3;
            let t1 = x1 + x2;
            let t2 = (x1 - x2) * MP3D_DCT_II_G_SEC[3 * i];
            let t3 = (x0 - x3) * MP3D_DCT_II_G_SEC[3 * i + 1];

            t[0][i] = t0 + t1;
            t[1][i] = (t0 - t1) * MP3D_DCT_II_G_SEC[3 * i + 2];
            t[2][i] = t3 + t2;
            t[3][i] = (t3 - t2) * MP3D_DCT_II_G_SEC[3 * i + 2];
        }

        // --- second stage ---
        for row in 0..4 {
            let mut x = t[row];

            let mut xt = x[0] - x[7];
            x[0] += x[7];
            x[7] = x[1] - x[6];
            x[1] += x[6];
            x[6] = x[2] - x[5];
            x[2] += x[5];
            x[5] = x[3] - x[4];
            x[3] += x[4];

            let x4 = x[0] - x[3];
            x[0] += x[3];
            let x3 = x[1] - x[2];
            x[1] += x[2];

            t[row][0] = x[0] + x[1];
            t[row][4] = (x[0] - x[1]) * 0.70710677;

            let mut x5 = x[5] + x[6];
            let x6 = (x[6] + x[7]) * 0.70710677;
            let mut x7 = x[7] + xt;
            let x3 = (x3 + x4) * 0.70710677;

            x5 -= x7 * 0.198912367;
            x7 += x5 * 0.382683432;
            x5 -= x7 * 0.198912367;

            let x0 = xt - x6;
            xt += x6;

            t[row][1] = (xt + x7) * 0.50979561;
            t[row][2] = (x4 + x3) * 0.54119611;
            t[row][3] = (x0 - x5) * 0.60134488;
            t[row][5] = (x0 + x5) * 0.89997619;
            t[row][6] = (x4 - x3) * 1.30656302;
            t[row][7] = (xt - x7) * 2.56291556;
        }

        // --- write back ---
        let mut y_base = k;

        for i in 0..7 {
            let set = |buf: &mut [f32], idx: usize, val: f32| {
                if let Some(x) = buf.get_mut(idx) {
                    *x = val;
                }
            };

            set(grbuf, y_base + 0 * 18, t[0][i]);
            set(
                grbuf,
                y_base + 1 * 18,
                t[2][i] + t[3][i] + t[3][i + 1],
            );
            set(
                grbuf,
                y_base + 2 * 18,
                t[1][i] + t[1][i + 1],
            );
            set(
                grbuf,
                y_base + 3 * 18,
                t[2][i + 1] + t[3][i] + t[3][i + 1],
            );

            y_base += 4 * 18;
        }

        let set = |buf: &mut [f32], idx: usize, val: f32| {
            if let Some(x) = buf.get_mut(idx) {
                *x = val;
            }
        };

        set(grbuf, y_base + 0 * 18, t[0][7]);
        set(grbuf, y_base + 1 * 18, t[2][7] + t[3][7]);
        set(grbuf, y_base + 2 * 18, t[1][7]);
        set(grbuf, y_base + 3 * 18, t[3][7]);
    }
}
fn mp3d_scale_pcm(sample: f32) -> f32 {
    sample * (1f32/32768f32)
}

fn mp3d_synth_pair(
    pcm: &mut [mp3d_sample_t],
    nch: u32,
    z: &[f32],
) {
    let nch = nch as usize;

    // We access up to 14 * 64 + 2, so validate length
    if z.len() < 14 * 64 + 2 {
        return;
    }

    let get = |idx: usize| -> f32 {
        z.get(idx).copied().unwrap_or(0.0)
    };

    // --- first sample ---
    let mut a =
        (get(14 * 64) - get(0)) * 29.0
        + (get(1 * 64) + get(13 * 64)) * 213.0
        + (get(12 * 64) - get(2 * 64)) * 459.0
        + (get(3 * 64) + get(11 * 64)) * 2037.0
        + (get(10 * 64) - get(4 * 64)) * 5153.0
        + (get(5 * 64) + get(9 * 64)) * 6574.0
        + (get(8 * 64) - get(6 * 64)) * 37489.0
        + get(7 * 64) * 75038.0;

    if let Some(x) = pcm.get_mut(0) {
        *x = mp3d_scale_pcm(a);
    }

    // --- second sample (z offset by +2) ---
    let get2 = |idx: usize| -> f32 {
        z.get(idx + 2).copied().unwrap_or(0.0)
    };

    a =
        get2(14 * 64) * 104.0
        + get2(12 * 64) * 1567.0
        + get2(10 * 64) * 9727.0
        + get2(8 * 64) * 64019.0
        + get2(6 * 64) * -9975.0
        + get2(4 * 64) * -45.0
        + get2(2 * 64) * 146.0
        + get2(0) * -5.0;

    let out_idx = 16 * nch;

    if let Some(x) = pcm.get_mut(out_idx) {
        *x = mp3d_scale_pcm(a);
    }
}

fn mp3d_synth(
    xl: &[f32],
    dstl: &mut [mp3d_sample_t],
    nch: u32,
    lins: &mut [f32],
) {
    let nch = nch as usize;
    if nch == 0 {
        return;
    }

    // Required sizes
    if xl.len() < 576 * nch || lins.len() < (15 + 32) * 64 {
        return;
    }

    // Split input for the last channel safely
    let xr_offset = 576 * (nch - 1);
    let xr = &xl[xr_offset..];

    let zlin_offset = 15 * 64;
    if zlin_offset >= lins.len() {
        return;
    }

    let (_, zlin) = lins.split_at_mut(zlin_offset); // zlin is the tail slice

    let dstr_off = nch - 1;

    // Safe get/set helpers
    let safe_get = |s: &[f32], idx: usize| -> f32 { s.get(idx).copied().unwrap_or(0.0) };
    let safe_set = |s: &mut [f32], idx: usize, val: f32| {
        if let Some(x) = s.get_mut(idx) {
            *x = val;
        }
    };

    // Initial writes into zlin
    safe_set(zlin, 4 * 15, safe_get(xl, 18 * 16));
    safe_set(zlin, 4 * 15 + 1, safe_get(xr, 18 * 16));
    safe_set(zlin, 4 * 15 + 2, safe_get(xl, 0));
    safe_set(zlin, 4 * 15 + 3, safe_get(xr, 0));

    safe_set(zlin, 4 * 31, safe_get(xl, 1 + 18 * 16));
    safe_set(zlin, 4 * 31 + 1, safe_get(xr, 1 + 18 * 16));
    safe_set(zlin, 4 * 31 + 2, safe_get(xl, 1));
    safe_set(zlin, 4 * 31 + 3, safe_get(xr, 1));

    // synth_pair calls safely using only zlin
    let mut call_pair = |offset: usize, zlin_off: usize| {
        if offset < dstl.len() && zlin_off < zlin.len() {
            mp3d_synth_pair(&mut dstl[offset..], nch as u32, &mut zlin[zlin_off..]);
        }
    };

    call_pair(dstr_off, 4 * 15 + 1);
    call_pair(dstr_off + 32 * nch, 4 * 15 + 64 + 1);
    call_pair(0, 4 * 15);
    call_pair(32 * nch, 4 * 15 + 64);

    // Main loop: write into zlin safely
    let mut w_iter = MP3D_SYNTH_G_WIN.iter();

    for i in (0..=14).rev() {
        let mut a = [0.0f32; 4];
        let mut b = [0.0f32; 4];
        let idx = i as usize;

        // Write zlin values
        safe_set(zlin, 4 * idx, safe_get(xl, 18 * (31 - idx)));
        safe_set(zlin, 4 * idx + 1, safe_get(xr, 18 * (31 - idx)));
        safe_set(zlin, 4 * idx + 2, safe_get(xl, 1 + 18 * (31 - idx)));
        safe_set(zlin, 4 * idx + 3, safe_get(xr, 1 + 18 * (31 - idx)));

        safe_set(zlin, 4 * (idx + 16), safe_get(xl, 1 + 18 * (1 + idx)));
        safe_set(zlin, 4 * (idx + 16) + 1, safe_get(xr, 1 + 18 * (1 + idx)));

        if idx >= 16 {
            safe_set(zlin, 4 * (idx - 16) + 2, safe_get(xl, 18 * (1 + idx)));
            safe_set(zlin, 4 * (idx - 16) + 3, safe_get(xr, 18 * (1 + idx)));
        }

        // 8 MAC blocks
        for k in 0..8 {
            let w0 = *w_iter.next().unwrap_or(&0.0);
            let w1 = *w_iter.next().unwrap_or(&0.0);

            for j in 0..4 {
                let vz_base = 4 * idx + j + k * 64;
                let vy_base = 4 * idx + j + (15 - k) * 64;

                let vz = safe_get(zlin, vz_base);
                let vy = safe_get(zlin, vy_base);

                b[j] += vz * w1 + vy * w0;

                if k % 2 == 0 {
                    a[j] += vz * w0 - vy * w1;
                } else {
                    a[j] += vy * w1 - vz * w0;
                }
            }
        }

        let write = |dst: &mut [mp3d_sample_t], idx: usize, val: f32| {
            if let Some(x) = dst.get_mut(idx) {
                *x = mp3d_scale_pcm(val);
            }
        };

        // Write outputs safely
        write(dstl, dstr_off + (15 - idx) * nch, a[1]);
        write(dstl, dstr_off + (17 + idx) * nch, b[1]);
        write(dstl, (15 - idx) * nch, a[0]);
        write(dstl, (17 + idx) * nch, b[0]);

        write(dstl, dstr_off + (47 - idx) * nch, a[3]);
        write(dstl, dstr_off + (49 + idx) * nch, b[3]);
        write(dstl, (47 - idx) * nch, a[2]);
        write(dstl, (49 + idx) * nch, b[2]);
    }
}

fn mp3d_synth_granule(
    qmf_state: &mut [f32],
    grbuf: &mut [f32],
    nbands: u32,
    nch: u32,
    pcm: &mut [mp3d_sample_t],
    lins: &mut [f32],
) {
    let nch = nch as usize;
    let nbands = nbands as usize;

    // --- Validate expected sizes (critical for safety) ---
    let qmf_len = 15 * 64;

    if qmf_state.len() < qmf_len || lins.len() < (nbands + 15) * 64 {
        return;
    }

    if grbuf.len() < 576 * nch {
        return;
    }

    // --- DCT per channel ---
    for ch in 0..nch {
        let start = 576 * ch;
        let end = start + 576;

        if let Some(mut slice) = grbuf.get_mut(start..end) {
            mp3d_DCT_II(&mut slice, nbands as u32);
        } else {
            return;
        }
    }

    // --- copy qmf_state → lins ---
    lins[..qmf_len].copy_from_slice(&qmf_state[..qmf_len]);

    // --- synthesis ---
    for i in (0..nbands).step_by(2) {
        let mut grbuf_ptr = match grbuf.get_mut(i..) {
            Some(s) => s,
            None => return,
        };

        let pcm_offset = 32 * nch * i;
        if pcm_offset >= pcm.len() {
            return;
        }

        let pcm_slice = &mut pcm[pcm_offset..];

        let lins_offset = i * 64;
        if lins_offset >= lins.len() {
            return;
        }

        let mut lins_ptr = &mut lins[lins_offset..];

        mp3d_synth(
            &mut grbuf_ptr,
            pcm_slice,
            nch as u32,
            &mut lins_ptr,
        );
    }

    // --- copy lins → qmf_state ---
    let tail_offset = nbands * 64;

    if tail_offset + qmf_len <= lins.len() {
        qmf_state.copy_from_slice(&lins[tail_offset..tail_offset + qmf_len]);
    }
}

fn mp3d_match_frame(
    hdr: &[u8],
    frame_bytes: usize,
) -> bool {
    let mut i: usize = 0;
    let mut nmatch: i32 = 0;
    nmatch = 0 as i32;
    while nmatch < 10 as i32 {
        i += hdr_frame_bytes(&hdr[i..], frame_bytes) + hdr_padding(&hdr[i..]);
        if i + 4 > hdr.len() {
            return nmatch > 0;
        }
        if !hdr_compare(hdr, &hdr[i..]) {
            return false;
        }
        nmatch += 1;
    }
    true
}

fn mp3d_find_frame(
    mut mp3: &[u8],
    free_format_bytes: &mut usize,
    ptr_frame_bytes: &mut usize,
) -> usize {
    let mp3_bytes = mp3.len();
    let mut i: usize = 0;
    let mut k: usize = 0;
    while i < mp3_bytes - 4 {
        if hdr_valid(mp3) {
            let mut frame_bytes = hdr_frame_bytes(mp3, *free_format_bytes);
            let mut frame_and_padding = frame_bytes + hdr_padding(mp3);
            k = 4;
            while frame_bytes == 0 && k < 2304 && i + 2 * k < mp3_bytes - 4
            {
                if hdr_compare(mp3, &mp3[k..]) {
                    let fb = k - hdr_padding(mp3);
                    let nextfb = fb + hdr_padding(&mp3[k..]);
                    if !(i + k + nextfb + 4 > mp3_bytes || !hdr_compare(mp3, &mp3[k+nextfb..])) {
                        frame_and_padding = k;
                        frame_bytes = fb;
                        *free_format_bytes = fb;
                    }
                }
                k += 1;
            }
            if frame_bytes != 0 && i + frame_and_padding <= mp3_bytes
                && mp3d_match_frame(mp3, frame_bytes)
                || i == 0 && frame_and_padding == mp3_bytes
            {
                *ptr_frame_bytes = frame_and_padding;
                return i;
            }
            *free_format_bytes = 0;
        }
        i += 1;
        mp3 = &mp3[1..];
    }
    *ptr_frame_bytes = 0;
    mp3_bytes
}

fn mp3dec_init(dec: &mut mp3dec_t) {
    dec.header[0] = 0;
}

pub fn mp3dec_decode_frame(
    dec: &mut mp3dec_t,
    mp3: &[u8],
    pcm: &mut [mp3d_sample_t],
    info: &mut mp3dec_frame_info_t,
) -> i32 {
    let mut i: usize = 0;
    let mut igr = 0u32;
    let mut frame_size: usize = 0;
    let mut success: i32 = 1;

    let mut scratch = mp3dec_scratch_t {
        grbuf: [[0.0; 576]; 2],
        scf: [0.0; 40],
        syn: [[0.0; 64]; 33],
        ist_pos: [[0; 39]; 2],
    };

    let mut scratch_maindata = [0u8; 2815];

    let mut scratch_bs = bs_t {
        buf: &[],
        pos: 0,
        limit: 0,
    };

    // Explicit initializer (no Default)
    let base_gr_info = L3_gr_info_t {
        sfbtab: core::ptr::null(),
        part_23_length: 0,
        big_values: 0,
        scalefac_compress: 0,
        global_gain: 0,
        block_type: 0,
        mixed_block_flag: 0,
        n_long_sfb: 0,
        n_short_sfb: 0,
        table_select: [0; 3],
        region_count: [0; 3],
        subblock_gain: [0; 3],
        preflag: 0,
        scalefac_scale: 0,
        count1_table: 0,
        scfsi: 0,
    };

    let mut scratch_gr_info = [
        base_gr_info,
        base_gr_info,
        base_gr_info,
        base_gr_info,
    ];

    // --- Header validation ---
    if mp3.len() > 4
        && dec.header.get(0) == Some(&0xff)
        && hdr_compare(&dec.header, mp3)
    {
        frame_size = hdr_frame_bytes(mp3, dec.free_format_bytes) + hdr_padding(mp3);

        if frame_size != mp3.len() {
            let valid_next = frame_size + 4 <= mp3.len()
                && mp3.get(frame_size..)
                    .map(|s| hdr_compare(mp3, s))
                    .unwrap_or(false);

            if !valid_next {
                frame_size = 0;
            }
        }
    }

    // --- Find frame ---
    if frame_size == 0 {
        *dec = mp3dec_t::new();

        i = mp3d_find_frame(mp3, &mut dec.free_format_bytes, &mut frame_size);

        if frame_size == 0 || i + frame_size > mp3.len() {
            info.frame_bytes = i;
            return 0;
        }
    }

    let hdr = match mp3.get(i..) {
        Some(h) if h.len() >= 4 => h,
        _ => {
            info.frame_bytes = i;
            return 0;
        }
    };

    dec.header.copy_from_slice(&hdr[..4]);

    info.frame_bytes = i + frame_size;
    info.frame_offset = i;
    info.channels = if hdr[3] & 0xc0 == 0xc0 { 1 } else { 2 };
    info.hz = hdr_sample_rate_hz(hdr) as i32;
    info.layer = 4 - ((hdr[1] >> 1) & 3);
    info.bitrate_kbps = hdr_bitrate_kbps(hdr) as i32;

    if pcm.is_empty() {
        return hdr_frame_samples(hdr) as i32;
    }

    let frame_data = match hdr.get(4..) {
        Some(d) => d,
        None => return 0,
    };

    let mut bs_frame = bs_init(frame_data, (frame_size - 4) as i32);

    if hdr[1] & 1 == 0 {
        get_bits(&mut bs_frame, 16);
    }

    let mut pcm_offset = 0;

    if info.layer == 3 {
        let main_data_begin =
            L3_read_side_info(&mut bs_frame, &mut scratch_gr_info, hdr);

        if main_data_begin < 0 || bs_frame.pos > bs_frame.limit {
            mp3dec_init(dec);
            return 0;
        }

        success = L3_restore_reservoir(
            dec,
            &mut bs_frame,
            &mut scratch_maindata,
            &mut scratch_bs,
            main_data_begin,
        );

        if success != 0 {
            let granules = if hdr[1] & 0x8 != 0 { 2 } else { 1 };

            while igr < granules {
                scratch.grbuf.as_flattened_mut().fill(0.0);

                let start = (igr * info.channels as u32) as usize;

                L3_decode(
                    dec,
                    &mut scratch,
                    &mut scratch_bs,
                    &mut scratch_gr_info[start..],
                    info.channels,
                );

                let needed = 576 * info.channels as usize;

                if pcm_offset + needed > pcm.len() {
                    return 0; // prevent overflow
                }

                let pcm_slice = &mut pcm[pcm_offset..pcm_offset + needed];

                mp3d_synth_granule(
                    &mut dec.qmf_state,
                    scratch.grbuf.as_flattened_mut(),
                    18,
                    info.channels,
                    pcm_slice,
                    scratch.syn.as_flattened_mut(),
                );

                pcm_offset += needed;
                igr += 1;
            }
        }

        L3_save_reservoir(dec, &mut scratch_bs);
    } else {
        return 0;
    }

    (success as u32)
        .wrapping_mul(hdr_frame_samples(&dec.header)) as i32
}