#![allow(
    clippy::all,
    non_camel_case_types,
    non_snake_case,
    unused_assignments
)]

mod tables;
use tables::*;

#[inline(always)]
unsafe fn memcpy(dst: *mut (), src: *const (), count: usize) {
    core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, count);
}

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
    bs: bs_t,
    maindata: [u8; 2815],
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
struct bs_t {
    buf: *const u8,
    pos: i32,
    limit: i32,
}
fn bs_init(
    data: *const u8,
    bytes: i32,
) -> bs_t {
    bs_t {
        buf: data,
        pos: 0,
        limit: bytes * 8
    }
}

unsafe fn get_bits(bs: &mut bs_t, n: i32) -> u32 {
    let mut next: u32 = 0;
    let mut cache: u32 = 0 as i32 as u32;
    let s: u32 = (bs.pos & 7 as i32) as u32;
    let mut shl: i32 = (n as u32).wrapping_add(s) as i32;
    let mut p: *const u8 = (bs.buf)
        .offset((bs.pos >> 3 as i32) as isize);
    bs.pos += n;
    if bs.pos > bs.limit {
        return 0 as i32 as u32;
    }
    let fresh0 = p;
    p = p.offset(1);
    next = (*fresh0 as i32 & 255 as i32 >> s) as u32;
    loop {
        shl -= 8 as i32;
        if !(shl > 0 as i32) {
            break;
        }
        cache |= next << shl;
        let fresh1 = p;
        p = p.offset(1);
        next = *fresh1 as u32;
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

unsafe fn L3_read_side_info(
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
    if part_23_sum + bs.pos > bs.limit + main_data_begin * 8 as i32 {
        return -(1 as i32);
    }
    return main_data_begin;
}
unsafe fn L3_read_scalefactors(
    mut scf: *mut u8,
    mut ist_pos: *mut u8,
    scf_size: *const u8,
    scf_count: *const u8,
    bitbuf: &mut bs_t,
    mut scfsi: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    i = 0 as i32;
    while i < 4 as i32 && *scf_count.offset(i as isize) as i32 != 0 {
        let cnt: i32 = *scf_count.offset(i as isize) as i32;
        if scfsi & 8 as i32 != 0 {
            memcpy(
                scf as *mut (),
                ist_pos as *const (),
                cnt as usize,
            );
        } else {
            let bits: i32 = *scf_size.offset(i as isize) as i32;
            if bits == 0 {
                core::ptr::write_bytes(scf, 0, cnt as usize);
                core::ptr::write_bytes(ist_pos, 0, cnt as usize);
            } else {
                let max_scf: i32 = if scfsi < 0 as i32 {
                    ((1 as i32) << bits) - 1 as i32
                } else {
                    -(1 as i32)
                };
                k = 0 as i32;
                while k < cnt {
                    let s: i32 = get_bits(bitbuf, bits) as i32;
                    *ist_pos
                        .offset(
                            k as isize,
                        ) = (if s == max_scf { -(1 as i32) } else { s })
                        as u8;
                    *scf.offset(k as isize) = s as u8;
                    k += 1;
                }
            }
        }
        ist_pos = ist_pos.offset(cnt as isize);
        scf = scf.offset(cnt as isize);
        i += 1;
        scfsi *= 2 as i32;
    }
    let ref mut fresh2 = *scf.offset(2 as i32 as isize);
    *fresh2 = 0 as i32 as u8;
    let ref mut fresh3 = *scf.offset(1 as i32 as isize);
    *fresh3 = *fresh2;
    *scf.offset(0 as i32 as isize) = *fresh3;
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
unsafe fn L3_decode_scalefactors(
    hdr: *const u8,
    ist_pos: *mut u8,
    bs: &mut bs_t,
    gr: &L3_gr_info_t,
    scf: *mut f32,
    ch: u32,
) {
    let mut scf_partition: *const u8 = (L3_DECODE_SCALEFACTORS_G_SCM_PARTITIONS[(((*gr).n_short_sfb != 0)
        as i32 + ((*gr).n_long_sfb == 0) as i32) as usize])
        .as_ptr();
    let mut scf_size: [u8; 4] = [0; 4];
    let mut iscf: [u8; 40] = [0; 40];
    let mut i: i32 = 0;
    let scf_shift: i32 = (*gr).scalefac_scale as i32
        + 1 as i32;
    let mut gain_exp: i32 = 0;
    let mut scfsi: i32 = (*gr).scfsi as i32;
    let mut gain: f32 = 0.;
    if *hdr.offset(1 as i32 as isize) as i32 & 0x8 as i32 != 0 {
        let part: i32 = L3_DECODE_SCALEFACTORS_G_SCFC_DECODE[(*gr).scalefac_compress as usize]
            as i32;
        scf_size[0 as i32 as usize] = (part >> 2 as i32) as u8;
        scf_size[1 as i32 as usize] = scf_size[0 as i32 as usize];
        scf_size[2 as i32 as usize] = (part & 3 as i32) as u8;
        scf_size[3 as i32 as usize] = scf_size[2 as i32 as usize];
    } else {
        let mut k: i32 = 0;
        let mut modprod: i32 = 0;
        let mut sfc: i32 = 0;
        let ist: i32 = (*hdr.offset(3 as i32 as isize) as i32
            & 0x10 as i32 != 0 && ch != 0) as i32;
        sfc = (*gr).scalefac_compress as i32 >> ist;
        k = ist * 3 as i32 * 4 as i32;
        while sfc >= 0 as i32 {
            modprod = 1 as i32;
            i = 3 as i32;
            while i >= 0 as i32 {
                scf_size[i
                    as usize] = (sfc / modprod % L3_DECODE_SCALEFACTORS_G_MOD[(k + i) as usize] as i32)
                    as u8;
                modprod *= L3_DECODE_SCALEFACTORS_G_MOD[(k + i) as usize] as i32;
                i -= 1;
            }
            sfc -= modprod;
            k += 4 as i32;
        }
        scf_partition = scf_partition.offset(k as isize);
        scfsi = -(16 as i32);
    }
    L3_read_scalefactors(
        iscf.as_mut_ptr(),
        ist_pos,
        scf_size.as_mut_ptr(),
        scf_partition,
        bs,
        scfsi,
    );
    if (*gr).n_short_sfb != 0 {
        let sh: i32 = 3 as i32 - scf_shift;
        i = 0 as i32;
        while i < (*gr).n_short_sfb as i32 {
            iscf[((*gr).n_long_sfb as i32 + i + 0 as i32)
                as usize] = (iscf[((*gr).n_long_sfb as i32 + i
                + 0 as i32) as usize] as i32
                + (((*gr).subblock_gain[0 as i32 as usize] as i32)
                    << sh)) as u8;
            iscf[((*gr).n_long_sfb as i32 + i + 1 as i32)
                as usize] = (iscf[((*gr).n_long_sfb as i32 + i
                + 1 as i32) as usize] as i32
                + (((*gr).subblock_gain[1 as i32 as usize] as i32)
                    << sh)) as u8;
            iscf[((*gr).n_long_sfb as i32 + i + 2 as i32)
                as usize] = (iscf[((*gr).n_long_sfb as i32 + i
                + 2 as i32) as usize] as i32
                + (((*gr).subblock_gain[2 as i32 as usize] as i32)
                    << sh)) as u8;
            i += 3 as i32;
        }
    } else if (*gr).preflag != 0 {
        i = 0 as i32;
        while i < 10 as i32 {
            iscf[(11 as i32 + i)
                as usize] = (iscf[(11 as i32 + i) as usize] as i32
                + L3_DECODE_SCALEFACTORS_G_PREAMP[i as usize] as i32) as u8;
            i += 1;
        }
    }
    gain_exp = (*gr).global_gain as i32 + -(1 as i32) * 4 as i32
        - 210 as i32
        - (if *hdr.offset(3 as i32 as isize) as i32 & 0xe0 as i32
            == 0x60 as i32
        {
            2 as i32
        } else {
            0 as i32
        });
    gain = L3_ldexp_q2(
        ((1 as i32)
            << (255 as i32 + -(1 as i32) * 4 as i32
                - 210 as i32 + 3 as i32 & !(3 as i32))
                / 4 as i32) as f32,
        (255 as i32 + -(1 as i32) * 4 as i32 - 210 as i32
            + 3 as i32 & !(3 as i32)) - gain_exp,
    );
    i = 0 as i32;
    while i < (*gr).n_long_sfb as i32 + (*gr).n_short_sfb as i32 {
        *scf
            .offset(
                i as isize,
            ) = L3_ldexp_q2(gain, (iscf[i as usize] as i32) << scf_shift);
        i += 1;
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
unsafe fn L3_huffman(
    mut dst: *mut f32,
    bs: &mut bs_t,
    gr_info: &L3_gr_info_t,
    mut scf: *const f32,
    layer3gr_limit: i32,
) {
    let mut one: f32 = 0.0f32;
    let mut ireg: i32 = 0 as i32;
    let mut big_val_cnt: i32 = (*gr_info).big_values as i32;
    let mut sfb: *const u8 = (*gr_info).sfbtab;
    let mut bs_next_ptr: *const u8 = (bs.buf)
        .offset((bs.pos / 8 as i32) as isize);
    let mut bs_cache: u32 = (*bs_next_ptr.offset(0 as i32 as isize)
        as u32)
        .wrapping_mul(256 as u32)
        .wrapping_add(*bs_next_ptr.offset(1 as i32 as isize) as u32)
        .wrapping_mul(256 as u32)
        .wrapping_add(*bs_next_ptr.offset(2 as i32 as isize) as u32)
        .wrapping_mul(256 as u32)
        .wrapping_add(*bs_next_ptr.offset(3 as i32 as isize) as u32)
        << (bs.pos & 7 as i32);
    let mut pairs_to_decode: i32 = 0;
    let mut np: i32 = 0;
    let mut bs_sh: i32 = (bs.pos & 7 as i32) - 8 as i32;
    bs_next_ptr = bs_next_ptr.offset(4 as i32 as isize);
    while big_val_cnt > 0 as i32 {
        let tab_num: i32 = (*gr_info).table_select[ireg as usize]
            as i32;
        let fresh4 = ireg;
        ireg = ireg + 1;
        let mut sfb_cnt: i32 = (*gr_info).region_count[fresh4 as usize]
            as i32;
        let codebook: *const i16 = L3_HUFFMAN_TABS
            .as_ptr()
            .offset(L3_HUFFMAN_TABINDEX[tab_num as usize] as i32 as isize);
        let linbits: i32 = L3_HUFFMAN_G_LINBITS[tab_num as usize] as i32;
        if linbits != 0 {
            loop {
                let fresh5 = sfb;
                sfb = sfb.offset(1);
                np = *fresh5 as i32 / 2 as i32;
                pairs_to_decode = if big_val_cnt > np { np } else { big_val_cnt };
                let fresh6 = scf;
                scf = scf.offset(1);
                one = *fresh6;
                loop {
                    let mut j: i32 = 0;
                    let mut w: i32 = 5 as i32;
                    let mut leaf: i32 = *codebook
                        .offset((bs_cache >> 32 as i32 - w) as isize)
                        as i32;
                    while leaf < 0 as i32 {
                        bs_cache <<= w;
                        bs_sh += w;
                        w = leaf & 7 as i32;
                        leaf = *codebook
                            .offset(
                                (bs_cache >> 32 as i32 - w)
                                    .wrapping_sub((leaf >> 3 as i32) as u32)
                                    as isize,
                            ) as i32;
                    }
                    bs_cache <<= leaf >> 8 as i32;
                    bs_sh += leaf >> 8 as i32;
                    j = 0 as i32;
                    while j < 2 as i32 {
                        let mut lsb: i32 = leaf & 0xf as i32;
                        if lsb == 15 as i32 {
                            lsb = (lsb as u32)
                                .wrapping_add(bs_cache >> 32 as i32 - linbits)
                                as i32 as i32;
                            bs_cache <<= linbits;
                            bs_sh += linbits;
                            while bs_sh >= 0 as i32 {
                                let fresh7 = bs_next_ptr;
                                bs_next_ptr = bs_next_ptr.offset(1);
                                bs_cache |= (*fresh7 as u32) << bs_sh;
                                bs_sh -= 8 as i32;
                            }
                            *dst = one * L3_pow_43(lsb)
                                * (if (bs_cache as i32) < 0 as i32 {
                                    -(1 as i32)
                                } else {
                                    1 as i32
                                }) as f32;
                        } else {
                            *dst = G_POW43[((16 as i32 + lsb) as u32)
                                .wrapping_sub(
                                    16 as i32 as u32
                                        * (bs_cache >> 31 as i32),
                                ) as usize] * one;
                        }
                        bs_cache
                            <<= if lsb != 0 {
                                1 as i32
                            } else {
                                0 as i32
                            };
                        bs_sh
                            += if lsb != 0 {
                                1 as i32
                            } else {
                                0 as i32
                            };
                        j += 1;
                        dst = dst.offset(1);
                        leaf >>= 4 as i32;
                    }
                    while bs_sh >= 0 as i32 {
                        let fresh8 = bs_next_ptr;
                        bs_next_ptr = bs_next_ptr.offset(1);
                        bs_cache |= (*fresh8 as u32) << bs_sh;
                        bs_sh -= 8 as i32;
                    }
                    pairs_to_decode -= 1;
                    if !(pairs_to_decode != 0) {
                        break;
                    }
                }
                big_val_cnt -= np;
                if !(big_val_cnt > 0 as i32
                    && {
                        sfb_cnt -= 1;
                        sfb_cnt >= 0 as i32
                    })
                {
                    break;
                }
            }
        } else {
            loop {
                let fresh9 = sfb;
                sfb = sfb.offset(1);
                np = *fresh9 as i32 / 2 as i32;
                pairs_to_decode = if big_val_cnt > np { np } else { big_val_cnt };
                let fresh10 = scf;
                scf = scf.offset(1);
                one = *fresh10;
                loop {
                    let mut j_0: i32 = 0;
                    let mut w_0: i32 = 5 as i32;
                    let mut leaf_0: i32 = *codebook
                        .offset((bs_cache >> 32 as i32 - w_0) as isize)
                        as i32;
                    while leaf_0 < 0 as i32 {
                        bs_cache <<= w_0;
                        bs_sh += w_0;
                        w_0 = leaf_0 & 7 as i32;
                        leaf_0 = *codebook
                            .offset(
                                (bs_cache >> 32 as i32 - w_0)
                                    .wrapping_sub((leaf_0 >> 3 as i32) as u32)
                                    as isize,
                            ) as i32;
                    }
                    bs_cache <<= leaf_0 >> 8 as i32;
                    bs_sh += leaf_0 >> 8 as i32;
                    j_0 = 0 as i32;
                    while j_0 < 2 as i32 {
                        let lsb_0: i32 = leaf_0 & 0xf as i32;
                        *dst = G_POW43[((16 as i32 + lsb_0) as u32)
                            .wrapping_sub(
                                16 as i32 as u32
                                    * (bs_cache >> 31 as i32),
                            ) as usize] * one;
                        bs_cache
                            <<= if lsb_0 != 0 {
                                1 as i32
                            } else {
                                0 as i32
                            };
                        bs_sh
                            += if lsb_0 != 0 {
                                1 as i32
                            } else {
                                0 as i32
                            };
                        j_0 += 1;
                        dst = dst.offset(1);
                        leaf_0 >>= 4 as i32;
                    }
                    while bs_sh >= 0 as i32 {
                        let fresh11 = bs_next_ptr;
                        bs_next_ptr = bs_next_ptr.offset(1);
                        bs_cache |= (*fresh11 as u32) << bs_sh;
                        bs_sh -= 8 as i32;
                    }
                    pairs_to_decode -= 1;
                    if !(pairs_to_decode != 0) {
                        break;
                    }
                }
                big_val_cnt -= np;
                if !(big_val_cnt > 0 as i32
                    && {
                        sfb_cnt -= 1;
                        sfb_cnt >= 0 as i32
                    })
                {
                    break;
                }
            }
        }
    }
    np = 1 as i32 - big_val_cnt;
    loop {
        let codebook_count1: *const u8 = if (*gr_info).count1_table
            as i32 != 0
        {
            L3_HUFFMAN_TAB33.as_ptr()
        } else {
            L3_HUFFMAN_TAB32.as_ptr()
        };
        let mut leaf_1: i32 = *codebook_count1
            .offset((bs_cache >> 32 as i32 - 4 as i32) as isize)
            as i32;
        if leaf_1 & 8 as i32 == 0 {
            leaf_1 = *codebook_count1
                .offset(
                    ((leaf_1 >> 3 as i32) as u32)
                        .wrapping_add(
                            bs_cache << 4 as i32
                                >> 32 as i32 - (leaf_1 & 3 as i32),
                        ) as isize,
                ) as i32;
        }
        bs_cache <<= leaf_1 & 7 as i32;
        bs_sh += leaf_1 & 7 as i32;
        if bs_next_ptr.offset_from(bs.buf) as isize
            * 8 as i32 as isize - 24 as i32 as isize
            + bs_sh as isize > layer3gr_limit as isize
        {
            break;
        }
        np -= 1;
        if np == 0 {
            let fresh12 = sfb;
            sfb = sfb.offset(1);
            np = *fresh12 as i32 / 2 as i32;
            if np == 0 {
                break;
            }
            let fresh13 = scf;
            scf = scf.offset(1);
            one = *fresh13;
        }
        if leaf_1 & 128 as i32 >> 0 as i32 != 0 {
            *dst
                .offset(
                    0 as i32 as isize,
                ) = if (bs_cache as i32) < 0 as i32 { -one } else { one };
            bs_cache <<= 1 as i32;
            bs_sh += 1 as i32;
        }
        if leaf_1 & 128 as i32 >> 1 as i32 != 0 {
            *dst
                .offset(
                    1 as i32 as isize,
                ) = if (bs_cache as i32) < 0 as i32 { -one } else { one };
            bs_cache <<= 1 as i32;
            bs_sh += 1 as i32;
        }
        np -= 1;
        if np == 0 {
            let fresh14 = sfb;
            sfb = sfb.offset(1);
            np = *fresh14 as i32 / 2 as i32;
            if np == 0 {
                break;
            }
            let fresh15 = scf;
            scf = scf.offset(1);
            one = *fresh15;
        }
        if leaf_1 & 128 as i32 >> 2 as i32 != 0 {
            *dst
                .offset(
                    2 as i32 as isize,
                ) = if (bs_cache as i32) < 0 as i32 { -one } else { one };
            bs_cache <<= 1 as i32;
            bs_sh += 1 as i32;
        }
        if leaf_1 & 128 as i32 >> 3 as i32 != 0 {
            *dst
                .offset(
                    3 as i32 as isize,
                ) = if (bs_cache as i32) < 0 as i32 { -one } else { one };
            bs_cache <<= 1 as i32;
            bs_sh += 1 as i32;
        }
        while bs_sh >= 0 as i32 {
            let fresh16 = bs_next_ptr;
            bs_next_ptr = bs_next_ptr.offset(1);
            bs_cache |= (*fresh16 as u32) << bs_sh;
            bs_sh -= 8 as i32;
        }
        dst = dst.offset(4 as i32 as isize);
    }
    bs.pos = layer3gr_limit;
}
unsafe fn L3_midside_stereo(
    left: *mut f32,
    n: i32,
) {
    let mut i: i32 = 0 as i32;
    let right: *mut f32 = left.offset(576 as i32 as isize);
    while i < n {
        let a: f32 = *left.offset(i as isize);
        let b: f32 = *right.offset(i as isize);
        *left.offset(i as isize) = a + b;
        *right.offset(i as isize) = a - b;
        i += 1;
    }
}
unsafe fn L3_intensity_stereo_band(
    left: *mut f32,
    n: i32,
    kl: f32,
    kr: f32,
) {
    let mut i: i32 = 0;
    i = 0 as i32;
    while i < n {
        *left.offset((i + 576 as i32) as isize) = *left.offset(i as isize) * kr;
        *left.offset(i as isize) = *left.offset(i as isize) * kl;
        i += 1;
    }
}
unsafe fn L3_stereo_top_band(
    mut right: *const f32,
    sfb: *const u8,
    nbands: i32,
    max_band: *mut i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let ref mut fresh17 = *max_band.offset(2 as i32 as isize);
    *fresh17 = -(1 as i32);
    let ref mut fresh18 = *max_band.offset(1 as i32 as isize);
    *fresh18 = *fresh17;
    *max_band.offset(0 as i32 as isize) = *fresh18;
    i = 0 as i32;
    while i < nbands {
        k = 0 as i32;
        while k < *sfb.offset(i as isize) as i32 {
            if *right.offset(k as isize) != 0 as i32 as f32
                || *right.offset((k + 1 as i32) as isize)
                    != 0 as i32 as f32
            {
                *max_band.offset((i % 3 as i32) as isize) = i;
                break;
            } else {
                k += 2 as i32;
            }
        }
        right = right.offset(*sfb.offset(i as isize) as i32 as isize);
        i += 1;
    }
}
unsafe fn L3_stereo_process(
    mut left: *mut f32,
    ist_pos: *const u8,
    sfb: *const u8,
    hdr: *const u8,
    max_band: *mut i32,
    mpeg2_sh: i32,
) {
    let mut i: u32 = 0;
    let max_pos: u32 = (if *hdr.offset(1 as i32 as isize)
        as i32 & 0x8 as i32 != 0
    {
        7 as i32
    } else {
        64 as i32
    }) as u32;
    i = 0 as i32 as u32;
    while *sfb.offset(i as isize) != 0 {
        let ipos: u32 = *ist_pos.offset(i as isize) as u32;
        if i as i32
            > *max_band.offset(i.wrapping_rem(3 as i32 as u32) as isize)
            && ipos < max_pos
        {
            let mut kl: f32 = 0.;
            let mut kr: f32 = 0.;
            let s: f32 = if *hdr.offset(3 as i32 as isize)
                as i32 & 0x20 as i32 != 0
            {
                1.41421356f32
            } else {
                1 as i32 as f32
            };
            if *hdr.offset(1 as i32 as isize) as i32 & 0x8 as i32
                != 0
            {
                kl = L3_STEREO_PROCESS_G_PAN[(2 as i32 as u32).wrapping_mul(ipos)
                    as usize];
                kr = L3_STEREO_PROCESS_G_PAN[(2 as i32 as u32)
                    .wrapping_mul(ipos)
                    .wrapping_add(1 as i32 as u32) as usize];
            } else {
                kl = 1 as i32 as f32;
                kr = L3_ldexp_q2(
                    1 as i32 as f32,
                    ((ipos.wrapping_add(1 as i32 as u32)
                        >> 1 as i32) << mpeg2_sh) as i32,
                );
                if ipos & 1 as i32 as u32 != 0 {
                    kl = kr;
                    kr = 1 as i32 as f32;
                }
            }
            L3_intensity_stereo_band(
                left,
                *sfb.offset(i as isize) as i32,
                kl * s,
                kr * s,
            );
        } else if *hdr.offset(3 as i32 as isize) as i32
            & 0x20 as i32 != 0
        {
            L3_midside_stereo(left, *sfb.offset(i as isize) as i32);
        }
        left = left.offset(*sfb.offset(i as isize) as i32 as isize);
        i = i.wrapping_add(1);
    }
}
unsafe fn L3_intensity_stereo(
    left: *mut f32,
    ist_pos: *mut u8,
    gr: &[L3_gr_info_t],
    hdr: *const u8,
) {
    let mut max_band: [i32; 3] = [0; 3];
    let n_sfb: i32 = gr[0].n_long_sfb as i32
        + gr[0].n_short_sfb as i32;
    let mut i: i32 = 0;
    let max_blocks: i32 = if gr[0].n_short_sfb as i32 != 0 {
        3 as i32
    } else {
        1 as i32
    };
    L3_stereo_top_band(
        left.offset(576 as i32 as isize),
        gr[0].sfbtab,
        n_sfb,
        max_band.as_mut_ptr(),
    );
    if gr[0].n_long_sfb != 0 {
        max_band[2 as i32
            as usize] = if (if max_band[0 as i32 as usize]
            < max_band[1 as i32 as usize]
        {
            max_band[1 as i32 as usize]
        } else {
            max_band[0 as i32 as usize]
        }) < max_band[2 as i32 as usize]
        {
            max_band[2 as i32 as usize]
        } else if max_band[0 as i32 as usize]
            < max_band[1 as i32 as usize]
        {
            max_band[1 as i32 as usize]
        } else {
            max_band[0 as i32 as usize]
        };
        max_band[1 as i32 as usize] = max_band[2 as i32 as usize];
        max_band[0 as i32 as usize] = max_band[1 as i32 as usize];
    }
    i = 0 as i32;
    while i < max_blocks {
        let default_pos: i32 = if *hdr.offset(1 as i32 as isize)
            as i32 & 0x8 as i32 != 0
        {
            3 as i32
        } else {
            0 as i32
        };
        let itop: i32 = n_sfb - max_blocks + i;
        let prev: i32 = itop - max_blocks;
        *ist_pos
            .offset(
                itop as isize,
            ) = (if max_band[i as usize] >= prev {
            default_pos
        } else {
            *ist_pos.offset(prev as isize) as i32
        }) as u8;
        i += 1;
    }
    L3_stereo_process(
        left,
        ist_pos,
        gr[0].sfbtab,
        hdr,
        max_band.as_mut_ptr(),
        gr[1].scalefac_compress as i32
            & 1 as i32,
    );
}
unsafe fn L3_reorder(
    grbuf: *mut f32,
    scratch: *mut f32,
    mut sfb: *const u8,
) {
    let mut i: i32 = 0;
    let mut len: i32 = 0;
    let mut src: *mut f32 = grbuf;
    let mut dst: *mut f32 = scratch;
    loop {
        len = *sfb as i32;
        if !(0 as i32 != len) {
            break;
        }
        i = 0 as i32;
        while i < len {
            let fresh19 = dst;
            dst = dst.offset(1);
            *fresh19 = *src.offset((0 as i32 * len) as isize);
            let fresh20 = dst;
            dst = dst.offset(1);
            *fresh20 = *src.offset((1 as i32 * len) as isize);
            let fresh21 = dst;
            dst = dst.offset(1);
            *fresh21 = *src.offset((2 as i32 * len) as isize);
            i += 1;
            src = src.offset(1);
        }
        sfb = sfb.offset(3 as i32 as isize);
        src = src.offset((2 as i32 * len) as isize);
    }
    memcpy(
        grbuf as *mut (),
        scratch as *const (),
        (dst.offset_from(scratch) as isize as usize)
            .wrapping_mul(::core::mem::size_of::<f32>() as usize),
    );
}
unsafe fn L3_antialias(
    mut grbuf: *mut f32,
    mut nbands: i32,
) {
    while nbands > 0 as i32 {
        let mut i: i32 = 0 as i32;
        while i < 8 as i32 {
            let u: f32 = *grbuf.offset((18 as i32 + i) as isize);
            let d: f32 = *grbuf.offset((17 as i32 - i) as isize);
            *grbuf
                .offset(
                    (18 as i32 + i) as isize,
                ) = u * L3_ANTIALIAS_G_AA[0 as i32 as usize][i as usize]
                - d * L3_ANTIALIAS_G_AA[1 as i32 as usize][i as usize];
            *grbuf
                .offset(
                    (17 as i32 - i) as isize,
                ) = u * L3_ANTIALIAS_G_AA[1 as i32 as usize][i as usize]
                + d * L3_ANTIALIAS_G_AA[0 as i32 as usize][i as usize];
            i += 1;
        }
        nbands -= 1;
        grbuf = grbuf.offset(18 as i32 as isize);
    }
}
unsafe fn L3_dct3_9(y: *mut f32) {
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
    s0 = *y.offset(0 as i32 as isize);
    s2 = *y.offset(2 as i32 as isize);
    s4 = *y.offset(4 as i32 as isize);
    s6 = *y.offset(6 as i32 as isize);
    s8 = *y.offset(8 as i32 as isize);
    t0 = s0 + s6 * 0.5f32;
    s0 -= s6;
    t4 = (s4 + s2) * 0.93969262f32;
    t2 = (s8 + s2) * 0.76604444f32;
    s6 = (s4 - s8) * 0.17364818f32;
    s4 += s8 - s2;
    s2 = s0 - s4 * 0.5f32;
    *y.offset(4 as i32 as isize) = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;
    s1 = *y.offset(1 as i32 as isize);
    s3 = *y.offset(3 as i32 as isize);
    s5 = *y.offset(5 as i32 as isize);
    s7 = *y.offset(7 as i32 as isize);
    s3 *= 0.86602540f32;
    t0 = (s5 + s1) * 0.98480775f32;
    t4 = (s5 - s7) * 0.34202014f32;
    t2 = (s1 + s7) * 0.64278761f32;
    s1 = (s1 - s5 - s7) * 0.86602540f32;
    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;
    *y.offset(0 as i32 as isize) = s4 - s7;
    *y.offset(1 as i32 as isize) = s2 + s1;
    *y.offset(2 as i32 as isize) = s0 - s3;
    *y.offset(3 as i32 as isize) = s8 + s5;
    *y.offset(5 as i32 as isize) = s8 - s5;
    *y.offset(6 as i32 as isize) = s0 + s3;
    *y.offset(7 as i32 as isize) = s2 - s1;
    *y.offset(8 as i32 as isize) = s4 + s7;
}
unsafe fn L3_imdct36(
    mut grbuf: *mut f32,
    mut overlap: *mut f32,
    window: *const f32,
    nbands: i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
     
    j = 0 as i32;
    while j < nbands {
        let mut co: [f32; 9] = [0.; 9];
        let mut si: [f32; 9] = [0.; 9];
        co[0 as i32 as usize] = -*grbuf.offset(0 as i32 as isize);
        si[0 as i32 as usize] = *grbuf.offset(17 as i32 as isize);
        i = 0 as i32;
        while i < 4 as i32 {
            si[(8 as i32 - 2 as i32 * i)
                as usize] = *grbuf
                .offset((4 as i32 * i + 1 as i32) as isize)
                - *grbuf.offset((4 as i32 * i + 2 as i32) as isize);
            co[(1 as i32 + 2 as i32 * i)
                as usize] = *grbuf
                .offset((4 as i32 * i + 1 as i32) as isize)
                + *grbuf.offset((4 as i32 * i + 2 as i32) as isize);
            si[(7 as i32 - 2 as i32 * i)
                as usize] = *grbuf
                .offset((4 as i32 * i + 4 as i32) as isize)
                - *grbuf.offset((4 as i32 * i + 3 as i32) as isize);
            co[(2 as i32 + 2 as i32 * i)
                as usize] = -(*grbuf
                .offset((4 as i32 * i + 3 as i32) as isize)
                + *grbuf.offset((4 as i32 * i + 4 as i32) as isize));
            i += 1;
        }
        L3_dct3_9(co.as_mut_ptr());
        L3_dct3_9(si.as_mut_ptr());
        si[1 as i32 as usize] = -si[1 as i32 as usize];
        si[3 as i32 as usize] = -si[3 as i32 as usize];
        si[5 as i32 as usize] = -si[5 as i32 as usize];
        si[7 as i32 as usize] = -si[7 as i32 as usize];
        i = 0 as i32;
        while i < 9 as i32 {
            let ovl: f32 = *overlap.offset(i as isize);
            let sum: f32 = co[i as usize]
                * L3_IMDCT36_G_TWID9[(9 as i32 + i) as usize]
                + si[i as usize] * L3_IMDCT36_G_TWID9[(0 as i32 + i) as usize];
            *overlap
                .offset(
                    i as isize,
                ) = co[i as usize] * L3_IMDCT36_G_TWID9[(0 as i32 + i) as usize]
                - si[i as usize] * L3_IMDCT36_G_TWID9[(9 as i32 + i) as usize];
            *grbuf
                .offset(
                    i as isize,
                ) = ovl * *window.offset((0 as i32 + i) as isize)
                - sum * *window.offset((9 as i32 + i) as isize);
            *grbuf
                .offset(
                    (17 as i32 - i) as isize,
                ) = ovl * *window.offset((9 as i32 + i) as isize)
                + sum * *window.offset((0 as i32 + i) as isize);
            i += 1;
        }
        j += 1;
        grbuf = grbuf.offset(18 as i32 as isize);
        overlap = overlap.offset(9 as i32 as isize);
    }
}
unsafe fn L3_idct3(
    x0: f32,
    x1: f32,
    x2: f32,
    dst: *mut f32,
) {
    let m1: f32 = x1 * 0.86602540f32;
    let a1: f32 = x0 - x2 * 0.5f32;
    *dst.offset(1 as i32 as isize) = x0 + x2;
    *dst.offset(0 as i32 as isize) = a1 + m1;
    *dst.offset(2 as i32 as isize) = a1 - m1;
}
unsafe fn L3_imdct12(
    x: *mut f32,
    dst: *mut f32,
    overlap: *mut f32,
) {
    let mut co: [f32; 3] = [0.; 3];
    let mut si: [f32; 3] = [0.; 3];
    let mut i: i32 = 0;
    L3_idct3(
        -*x.offset(0 as i32 as isize),
        *x.offset(6 as i32 as isize) + *x.offset(3 as i32 as isize),
        *x.offset(12 as i32 as isize) + *x.offset(9 as i32 as isize),
        co.as_mut_ptr(),
    );
    L3_idct3(
        *x.offset(15 as i32 as isize),
        *x.offset(12 as i32 as isize) - *x.offset(9 as i32 as isize),
        *x.offset(6 as i32 as isize) - *x.offset(3 as i32 as isize),
        si.as_mut_ptr(),
    );
    si[1 as i32 as usize] = -si[1 as i32 as usize];
    i = 0 as i32;
    while i < 3 as i32 {
        let ovl: f32 = *overlap.offset(i as isize);
        let sum: f32 = co[i as usize]
            * L3_IMDCT12_G_TWID3[(3 as i32 + i) as usize]
            + si[i as usize] * L3_IMDCT12_G_TWID3[(0 as i32 + i) as usize];
        *overlap
            .offset(
                i as isize,
            ) = co[i as usize] * L3_IMDCT12_G_TWID3[(0 as i32 + i) as usize]
            - si[i as usize] * L3_IMDCT12_G_TWID3[(3 as i32 + i) as usize];
        *dst
            .offset(
                i as isize,
            ) = ovl * L3_IMDCT12_G_TWID3[(2 as i32 - i) as usize]
            - sum * L3_IMDCT12_G_TWID3[(5 as i32 - i) as usize];
        *dst
            .offset(
                (5 as i32 - i) as isize,
            ) = ovl * L3_IMDCT12_G_TWID3[(5 as i32 - i) as usize]
            + sum * L3_IMDCT12_G_TWID3[(2 as i32 - i) as usize];
        i += 1;
    }
}
unsafe fn L3_imdct_short(
    mut grbuf: *mut f32,
    mut overlap: *mut f32,
    mut nbands: i32,
) {
    while nbands > 0 as i32 {
        let mut tmp: [f32; 18] = [0.; 18];
        memcpy(
            tmp.as_mut_ptr() as *mut (),
            grbuf as *const (),
            ::core::mem::size_of::<[f32; 18]>() as usize,
        );
        memcpy(
            grbuf as *mut (),
            overlap as *const (),
            (6 as i32 as usize)
                .wrapping_mul(::core::mem::size_of::<f32>() as usize),
        );
        L3_imdct12(
            tmp.as_mut_ptr(),
            grbuf.offset(6 as i32 as isize),
            overlap.offset(6 as i32 as isize),
        );
        L3_imdct12(
            tmp.as_mut_ptr().offset(1 as i32 as isize),
            grbuf.offset(12 as i32 as isize),
            overlap.offset(6 as i32 as isize),
        );
        L3_imdct12(
            tmp.as_mut_ptr().offset(2 as i32 as isize),
            overlap,
            overlap.offset(6 as i32 as isize),
        );
        nbands -= 1;
        overlap = overlap.offset(9 as i32 as isize);
        grbuf = grbuf.offset(18 as i32 as isize);
    }
}
unsafe fn L3_change_sign(mut grbuf: *mut f32) {
    let mut b: i32 = 0;
    let mut i: i32 = 0;
    b = 0 as i32;
    grbuf = grbuf.offset(18 as i32 as isize);
    while b < 32 as i32 {
        i = 1 as i32;
        while i < 18 as i32 {
            *grbuf.offset(i as isize) = -*grbuf.offset(i as isize);
            i += 2 as i32;
        }
        b += 2 as i32;
        grbuf = grbuf.offset(36 as i32 as isize);
    }
}
unsafe fn L3_imdct_gr(
    mut grbuf: *mut f32,
    mut overlap: *mut f32,
    block_type: u32,
    n_long_bands: u32,
) {
    if n_long_bands != 0 {
        L3_imdct36(
            grbuf,
            overlap,
            (L3_IMDCT_GR_G_MDCT_WINDOW[0 as i32 as usize]).as_ptr(),
            n_long_bands as i32,
        );
        grbuf = grbuf
            .offset(
                (18 as i32 as u32).wrapping_mul(n_long_bands) as isize,
            );
        overlap = overlap
            .offset(
                (9 as i32 as u32).wrapping_mul(n_long_bands) as isize,
            );
    }
    if block_type == 2 as i32 as u32 {
        L3_imdct_short(
            grbuf,
            overlap,
            (32 as i32 as u32).wrapping_sub(n_long_bands) as i32,
        );
    } else {
        L3_imdct36(
            grbuf,
            overlap,
            (L3_IMDCT_GR_G_MDCT_WINDOW[(block_type == 3 as i32 as u32)
                as i32 as usize])
                .as_ptr(),
            (32 as i32 as u32).wrapping_sub(n_long_bands) as i32,
        );
    };
}

fn L3_save_reservoir(
    h: &mut mp3dec_t,
    s: &mut mp3dec_scratch_t,
) {
    let mut pos: i32 = (((*s).bs.pos + 7 as i32) as u32)
        .wrapping_div(8 as u32) as i32;
    let mut remains: i32 = ((*s).bs.limit as u32)
        .wrapping_div(8 as u32)
        .wrapping_sub(pos as u32) as i32;
    if remains > 511 as i32 {
        pos += remains - 511 as i32;
        remains = 511 as i32;
    }
    if remains > 0 as i32 {
        h.reserv_buf[..remains as usize].copy_from_slice(&s.maindata[pos as usize..(pos+remains) as usize]);
    }
    (*h).reserv = remains;
}

unsafe fn L3_restore_reservoir(
    h: &mut mp3dec_t,
    bs: &mut bs_t,
    s: &mut mp3dec_scratch_t,
    main_data_begin: i32,
) -> i32 {
    let frame_bytes: i32 = (bs.limit - bs.pos) / 8 as i32;
    let bytes_have: i32 = if (*h).reserv > main_data_begin {
        main_data_begin
    } else {
        (*h).reserv
    };
    memcpy(
        ((*s).maindata).as_mut_ptr() as *mut (),
        ((*h).reserv_buf)
            .as_mut_ptr()
            .offset(
                (if (0 as i32) < (*h).reserv - main_data_begin {
                    (*h).reserv - main_data_begin
                } else {
                    0 as i32
                }) as isize,
            ) as *const (),
        (if (*h).reserv > main_data_begin { main_data_begin } else { (*h).reserv })
            as usize,
    );
    memcpy(
        ((*s).maindata).as_mut_ptr().offset(bytes_have as isize) as *mut (),
        (bs.buf).offset((bs.pos / 8 as i32) as isize)
            as *const (),
        frame_bytes as usize,
    );
    s.bs = bs_init(((*s).maindata).as_mut_ptr(), bytes_have + frame_bytes);
    return ((*h).reserv >= main_data_begin) as i32;
}
unsafe fn L3_decode(
    h: &mut mp3dec_t,
    s: &mut mp3dec_scratch_t,
    mut gr_info: &mut [L3_gr_info_t],
    nch: u32,
) {
    let mut ch: u32 = 0;
    while ch < nch {
        let layer3gr_limit: i32 = (*s).bs.pos
            + gr_info[ch as usize].part_23_length as i32;
        L3_decode_scalefactors(
            ((*h).header).as_mut_ptr(),
            ((*s).ist_pos[ch as usize]).as_mut_ptr(),
            &mut (*s).bs,
            &gr_info[ch as usize],
            ((*s).scf).as_mut_ptr(),
            ch,
        );
        L3_huffman(
            ((*s).grbuf[ch as usize]).as_mut_ptr(),
            &mut (*s).bs,
            &gr_info[ch as usize],
            ((*s).scf).as_mut_ptr(),
            layer3gr_limit,
        );
        ch += 1;
    }
    if (*h).header[3 as i32 as usize] as i32 & 0x10 as i32 != 0 {
        L3_intensity_stereo(
            (*s).grbuf.as_flattened_mut().as_mut_ptr(),
            ((*s).ist_pos[1 as i32 as usize]).as_mut_ptr(),
            gr_info,
            ((*h).header).as_mut_ptr(),
        );
    } else if (*h).header[3 as i32 as usize] as i32 & 0xe0 as i32
        == 0x60 as i32
    {
        L3_midside_stereo(
            (*s).grbuf.as_flattened_mut().as_mut_ptr(),
            576 as i32,
        );
    }
    ch = 0;
    while ch < nch {
        let mut aa_bands: i32 = 31 as i32;
        let n_long_bands: i32 = (if gr_info[0].mixed_block_flag
            as i32 != 0
        {
            2 as i32
        } else {
            0 as i32
        })
            << (((*h).header[2 as i32 as usize] as i32
                >> 2 as i32 & 3 as i32)
                + (((*h).header[1 as i32 as usize] as i32
                    >> 3 as i32 & 1 as i32)
                    + ((*h).header[1 as i32 as usize] as i32
                        >> 4 as i32 & 1 as i32)) * 3 as i32
                == 2 as i32) as i32;
        if gr_info[0].n_short_sfb != 0 {
            aa_bands = n_long_bands - 1 as i32;
            L3_reorder(
                ((*s).grbuf[ch as usize])
                    .as_mut_ptr()
                    .offset((n_long_bands * 18 as i32) as isize),
                (*s).syn.as_flattened_mut().as_mut_ptr(),
                (gr_info[0].sfbtab).offset(gr_info[0].n_long_sfb as i32 as isize),
            );
        }
        L3_antialias(((*s).grbuf[ch as usize]).as_mut_ptr(), aa_bands);
        L3_imdct_gr(
            ((*s).grbuf[ch as usize]).as_mut_ptr(),
            ((*h).mdct_overlap[ch as usize]).as_mut_ptr(),
            gr_info[0].block_type as u32,
            n_long_bands as u32,
        );
        L3_change_sign(((*s).grbuf[ch as usize]).as_mut_ptr());
        ch += 1;
        gr_info = &mut gr_info[1..];
    }
}
unsafe fn mp3d_DCT_II(grbuf: *mut f32, n: u32) {
    let mut i: i32 = 0;
    let mut k = 0;
    while k < n {
        let mut t: [[f32; 8]; 4] = [[0.; 8]; 4];
        let mut x: *mut f32 = t.as_flattened_mut().as_mut_ptr();
        let mut y: *mut f32 = grbuf.offset(k as isize);
        i = 0 as i32;
        while i < 8 as i32 {
            let x0: f32 = *y.offset((i * 18 as i32) as isize);
            let x1: f32 = *y
                .offset(((15 as i32 - i) * 18 as i32) as isize);
            let x2: f32 = *y
                .offset(((16 as i32 + i) * 18 as i32) as isize);
            let x3: f32 = *y
                .offset(((31 as i32 - i) * 18 as i32) as isize);
            let t0: f32 = x0 + x3;
            let t1: f32 = x1 + x2;
            let t2: f32 = (x1 - x2)
                * MP3D_DCT_II_G_SEC[(3 as i32 * i + 0 as i32) as usize];
            let t3: f32 = (x0 - x3)
                * MP3D_DCT_II_G_SEC[(3 as i32 * i + 1 as i32) as usize];
            *x.offset(0 as i32 as isize) = t0 + t1;
            *x
                .offset(
                    8 as i32 as isize,
                ) = (t0 - t1)
                * MP3D_DCT_II_G_SEC[(3 as i32 * i + 2 as i32) as usize];
            *x.offset(16 as i32 as isize) = t3 + t2;
            *x
                .offset(
                    24 as i32 as isize,
                ) = (t3 - t2)
                * MP3D_DCT_II_G_SEC[(3 as i32 * i + 2 as i32) as usize];
            i += 1;
            x = x.offset(1);
        }
        x = t.as_flattened_mut().as_mut_ptr();
        i = 0 as i32;
        while i < 4 as i32 {
            let mut x0_0: f32 = *x.offset(0 as i32 as isize);
            let mut x1_0: f32 = *x.offset(1 as i32 as isize);
            let mut x2_0: f32 = *x.offset(2 as i32 as isize);
            let mut x3_0: f32 = *x.offset(3 as i32 as isize);
            let mut x4: f32 = *x.offset(4 as i32 as isize);
            let mut x5: f32 = *x.offset(5 as i32 as isize);
            let mut x6: f32 = *x.offset(6 as i32 as isize);
            let mut x7: f32 = *x.offset(7 as i32 as isize);
            let mut xt: f32 = 0.;
            xt = x0_0 - x7;
            x0_0 += x7;
            x7 = x1_0 - x6;
            x1_0 += x6;
            x6 = x2_0 - x5;
            x2_0 += x5;
            x5 = x3_0 - x4;
            x3_0 += x4;
            x4 = x0_0 - x3_0;
            x0_0 += x3_0;
            x3_0 = x1_0 - x2_0;
            x1_0 += x2_0;
            *x.offset(0 as i32 as isize) = x0_0 + x1_0;
            *x.offset(4 as i32 as isize) = (x0_0 - x1_0) * 0.70710677f32;
            x5 = x5 + x6;
            x6 = (x6 + x7) * 0.70710677f32;
            x7 = x7 + xt;
            x3_0 = (x3_0 + x4) * 0.70710677f32;
            x5 -= x7 * 0.198912367f32;
            x7 += x5 * 0.382683432f32;
            x5 -= x7 * 0.198912367f32;
            x0_0 = xt - x6;
            xt += x6;
            *x.offset(1 as i32 as isize) = (xt + x7) * 0.50979561f32;
            *x.offset(2 as i32 as isize) = (x4 + x3_0) * 0.54119611f32;
            *x.offset(3 as i32 as isize) = (x0_0 - x5) * 0.60134488f32;
            *x.offset(5 as i32 as isize) = (x0_0 + x5) * 0.89997619f32;
            *x.offset(6 as i32 as isize) = (x4 - x3_0) * 1.30656302f32;
            *x.offset(7 as i32 as isize) = (xt - x7) * 2.56291556f32;
            i += 1;
            x = x.offset(8 as i32 as isize);
        }
        i = 0 as i32;
        while i < 7 as i32 {
            *y
                .offset(
                    (0 as i32 * 18 as i32) as isize,
                ) = t[0 as i32 as usize][i as usize];
            *y
                .offset(
                    (1 as i32 * 18 as i32) as isize,
                ) = t[2 as i32 as usize][i as usize]
                + t[3 as i32 as usize][i as usize]
                + t[3 as i32 as usize][(i + 1 as i32) as usize];
            *y
                .offset(
                    (2 as i32 * 18 as i32) as isize,
                ) = t[1 as i32 as usize][i as usize]
                + t[1 as i32 as usize][(i + 1 as i32) as usize];
            *y
                .offset(
                    (3 as i32 * 18 as i32) as isize,
                ) = t[2 as i32 as usize][(i + 1 as i32) as usize]
                + t[3 as i32 as usize][i as usize]
                + t[3 as i32 as usize][(i + 1 as i32) as usize];
            i += 1;
            y = y.offset((4 as i32 * 18 as i32) as isize);
        }
        *y
            .offset(
                (0 as i32 * 18 as i32) as isize,
            ) = t[0 as i32 as usize][7 as i32 as usize];
        *y
            .offset(
                (1 as i32 * 18 as i32) as isize,
            ) = t[2 as i32 as usize][7 as i32 as usize]
            + t[3 as i32 as usize][7 as i32 as usize];
        *y
            .offset(
                (2 as i32 * 18 as i32) as isize,
            ) = t[1 as i32 as usize][7 as i32 as usize];
        *y
            .offset(
                (3 as i32 * 18 as i32) as isize,
            ) = t[3 as i32 as usize][7 as i32 as usize];
        k += 1;
    }
}

fn mp3d_scale_pcm(sample: f32) -> f32 {
    sample * (1f32/32768f32)
}

unsafe fn mp3d_synth_pair(
    pcm: &mut [mp3d_sample_t],
    nch: u32,
    mut z: *const f32,
) {
    let mut a: f32 = 0.;
    a = (*z.offset((14 as i32 * 64 as i32) as isize)
        - *z.offset(0 as i32 as isize)) * 29 as i32 as f32;
    a
        += (*z.offset((1 as i32 * 64 as i32) as isize)
            + *z.offset((13 as i32 * 64 as i32) as isize))
            * 213 as i32 as f32;
    a
        += (*z.offset((12 as i32 * 64 as i32) as isize)
            - *z.offset((2 as i32 * 64 as i32) as isize))
            * 459 as i32 as f32;
    a
        += (*z.offset((3 as i32 * 64 as i32) as isize)
            + *z.offset((11 as i32 * 64 as i32) as isize))
            * 2037 as i32 as f32;
    a
        += (*z.offset((10 as i32 * 64 as i32) as isize)
            - *z.offset((4 as i32 * 64 as i32) as isize))
            * 5153 as i32 as f32;
    a
        += (*z.offset((5 as i32 * 64 as i32) as isize)
            + *z.offset((9 as i32 * 64 as i32) as isize))
            * 6574 as i32 as f32;
    a
        += (*z.offset((8 as i32 * 64 as i32) as isize)
            - *z.offset((6 as i32 * 64 as i32) as isize))
            * 37489 as i32 as f32;
    a
        += *z.offset((7 as i32 * 64 as i32) as isize)
            * 75038 as i32 as f32;
    pcm[0] = mp3d_scale_pcm(a);
    z = z.offset(2 as i32 as isize);
    a = *z.offset((14 as i32 * 64 as i32) as isize)
        * 104 as i32 as f32;
    a
        += *z.offset((12 as i32 * 64 as i32) as isize)
            * 1567 as i32 as f32;
    a
        += *z.offset((10 as i32 * 64 as i32) as isize)
            * 9727 as i32 as f32;
    a
        += *z.offset((8 as i32 * 64 as i32) as isize)
            * 64019 as i32 as f32;
    a
        += *z.offset((6 as i32 * 64 as i32) as isize)
            * -(9975 as i32) as f32;
    a
        += *z.offset((4 as i32 * 64 as i32) as isize)
            * -(45 as i32) as f32;
    a
        += *z.offset((2 as i32 * 64 as i32) as isize)
            * 146 as i32 as f32;
    a
        += *z.offset((0 as i32 * 64 as i32) as isize)
            * -(5 as i32) as f32;
    pcm[(16 * nch) as usize] = mp3d_scale_pcm(a);
}
unsafe fn mp3d_synth(
    xl: *mut f32,
    dstl: &mut [mp3d_sample_t],
    nch: u32,
    lins: *mut f32,
) {
    let mut i: i32 = 0;
    let xr: *mut f32 = xl
        .offset((576 * (nch - 1)) as isize);
    let dstr_off = (nch-1) as usize;

    let zlin: *mut f32 = lins
        .offset((15 as i32 * 64 as i32) as isize);
    let mut w: *const f32 = MP3D_SYNTH_G_WIN.as_ptr();
    *zlin
        .offset(
            (4 as i32 * 15 as i32) as isize,
        ) = *xl.offset((18 as i32 * 16 as i32) as isize);
    *zlin
        .offset(
            (4 as i32 * 15 as i32 + 1 as i32) as isize,
        ) = *xr.offset((18 as i32 * 16 as i32) as isize);
    *zlin
        .offset(
            (4 as i32 * 15 as i32 + 2 as i32) as isize,
        ) = *xl.offset(0 as i32 as isize);
    *zlin
        .offset(
            (4 as i32 * 15 as i32 + 3 as i32) as isize,
        ) = *xr.offset(0 as i32 as isize);
    *zlin
        .offset(
            (4 as i32 * 31 as i32) as isize,
        ) = *xl
        .offset((1 as i32 + 18 as i32 * 16 as i32) as isize);
    *zlin
        .offset(
            (4 as i32 * 31 as i32 + 1 as i32) as isize,
        ) = *xr
        .offset((1 as i32 + 18 as i32 * 16 as i32) as isize);
    *zlin
        .offset(
            (4 as i32 * 31 as i32 + 2 as i32) as isize,
        ) = *xl.offset(1 as i32 as isize);
    *zlin
        .offset(
            (4 as i32 * 31 as i32 + 3 as i32) as isize,
        ) = *xr.offset(1 as i32 as isize);
    mp3d_synth_pair(
        &mut dstl[dstr_off..],
        nch,
        lins
            .offset((4 as i32 * 15 as i32) as isize)
            .offset(1 as i32 as isize),
    );
    mp3d_synth_pair(
        &mut dstl[dstr_off+(32 * nch) as usize..],
        nch,
        lins
            .offset((4 as i32 * 15 as i32) as isize)
            .offset(64 as i32 as isize)
            .offset(1 as i32 as isize),
    );
    mp3d_synth_pair(
        dstl,
        nch,
        lins.offset((4 as i32 * 15 as i32) as isize),
    );
    mp3d_synth_pair(
        &mut dstl[(32 * nch) as usize..],
        nch,
        lins
            .offset((4 as i32 * 15 as i32) as isize)
            .offset(64 as i32 as isize),
    );
    i = 14;
    while i >= 0 {
        let mut a: [f32; 4] = [0.; 4];
        let mut b: [f32; 4] = [0.; 4];
        *zlin
            .offset(
                (4 * i) as isize,
            ) = *xl.offset((18 * (31 - i)) as isize);
        *zlin
            .offset(
                (4 * i + 1) as isize,
            ) = *xr.offset((18 * (31 - i)) as isize);
        *zlin
            .offset(
                (4 * i + 2) as isize,
            ) = *xl
            .offset(
                (1 + 18 * (31 - i)) as isize,
            );
        *zlin
            .offset(
                (4 * i + 3) as isize,
            ) = *xr
            .offset(
                (1 + 18 * (31 - i)) as isize,
            );
        *zlin
            .offset(
                (4 * (i + 16)) as isize,
            ) = *xl
            .offset(
                (1 + 18 * (1 + i)) as isize,
            );
        *zlin
            .offset(
                (4 * (i + 16) + 1) as isize,
            ) = *xr
            .offset(
                (1 + 18 * (1 + i)) as isize,
            );
        *zlin
            .offset(
                (4 * (i - 16) + 2) as isize,
            ) = *xl.offset((18 * (1 + i)) as isize);
        *zlin
            .offset(
                (4 * (i - 16) + 3) as isize,
            ) = *xr.offset((18 * (1 + i)) as isize);
        let mut j: i32 = 0;
        let fresh22 = w;
        w = w.offset(1);
        let w0: f32 = *fresh22;
        let fresh23 = w;
        w = w.offset(1);
        let w1: f32 = *fresh23;
        let vz: *mut f32 = zlin
            .offset(
                (4 * i - 0 * 64) as isize,
            );
        let vy: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 0) * 64)
                    as isize,
            );
        j = 0;
        while j < 4 {
            b[j as usize] = *vz.offset(j as isize) * w1 + *vy.offset(j as isize) * w0;
            a[j as usize] = *vz.offset(j as isize) * w0 - *vy.offset(j as isize) * w1;
            j += 1;
        }
        let mut j_0: i32 = 0;
        let fresh24 = w;
        w = w.offset(1);
        let w0_0: f32 = *fresh24;
        let fresh25 = w;
        w = w.offset(1);
        let w1_0: f32 = *fresh25;
        let vz_0: *mut f32 = zlin
            .offset(
                (4 * i - 1 * 64) as isize,
            );
        let vy_0: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 1) * 64)
                    as isize,
            );
        j_0 = 0 as i32;
        while j_0 < 4 as i32 {
            b[j_0 as usize]
                += *vz_0.offset(j_0 as isize) * w1_0 + *vy_0.offset(j_0 as isize) * w0_0;
            a[j_0 as usize]
                += *vy_0.offset(j_0 as isize) * w1_0 - *vz_0.offset(j_0 as isize) * w0_0;
            j_0 += 1;
        }
        let mut j_1: i32 = 0;
        let fresh26 = w;
        w = w.offset(1);
        let w0_1: f32 = *fresh26;
        let fresh27 = w;
        w = w.offset(1);
        let w1_1: f32 = *fresh27;
        let vz_1: *mut f32 = zlin
            .offset(
                (4 * i - 2 * 64) as isize,
            );
        let vy_1: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 2) * 64)
                    as isize,
            );
        j_1 = 0 as i32;
        while j_1 < 4 as i32 {
            b[j_1 as usize]
                += *vz_1.offset(j_1 as isize) * w1_1 + *vy_1.offset(j_1 as isize) * w0_1;
            a[j_1 as usize]
                += *vz_1.offset(j_1 as isize) * w0_1 - *vy_1.offset(j_1 as isize) * w1_1;
            j_1 += 1;
        }
        let mut j_2: i32 = 0;
        let fresh28 = w;
        w = w.offset(1);
        let w0_2: f32 = *fresh28;
        let fresh29 = w;
        w = w.offset(1);
        let w1_2: f32 = *fresh29;
        let vz_2: *mut f32 = zlin
            .offset(
                (4 * i - 3 * 64) as isize,
            );
        let vy_2: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 3) * 64)
                    as isize,
            );
        j_2 = 0 as i32;
        while j_2 < 4 as i32 {
            b[j_2 as usize]
                += *vz_2.offset(j_2 as isize) * w1_2 + *vy_2.offset(j_2 as isize) * w0_2;
            a[j_2 as usize]
                += *vy_2.offset(j_2 as isize) * w1_2 - *vz_2.offset(j_2 as isize) * w0_2;
            j_2 += 1;
        }
        let mut j_3: i32 = 0;
        let fresh30 = w;
        w = w.offset(1);
        let w0_3: f32 = *fresh30;
        let fresh31 = w;
        w = w.offset(1);
        let w1_3: f32 = *fresh31;
        let vz_3: *mut f32 = zlin
            .offset(
                (4 * i - 4 * 64) as isize,
            );
        let vy_3: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 4) * 64)
                    as isize,
            );
        j_3 = 0 as i32;
        while j_3 < 4 as i32 {
            b[j_3 as usize]
                += *vz_3.offset(j_3 as isize) * w1_3 + *vy_3.offset(j_3 as isize) * w0_3;
            a[j_3 as usize]
                += *vz_3.offset(j_3 as isize) * w0_3 - *vy_3.offset(j_3 as isize) * w1_3;
            j_3 += 1;
        }
        let mut j_4: i32 = 0;
        let fresh32 = w;
        w = w.offset(1);
        let w0_4: f32 = *fresh32;
        let fresh33 = w;
        w = w.offset(1);
        let w1_4: f32 = *fresh33;
        let vz_4: *mut f32 = zlin
            .offset(
                (4 * i - 5 * 64) as isize,
            );
        let vy_4: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 5) * 64)
                    as isize,
            );
        j_4 = 0 as i32;
        while j_4 < 4 as i32 {
            b[j_4 as usize]
                += *vz_4.offset(j_4 as isize) * w1_4 + *vy_4.offset(j_4 as isize) * w0_4;
            a[j_4 as usize]
                += *vy_4.offset(j_4 as isize) * w1_4 - *vz_4.offset(j_4 as isize) * w0_4;
            j_4 += 1;
        }
        let mut j_5: i32 = 0;
        let fresh34 = w;
        w = w.offset(1);
        let w0_5: f32 = *fresh34;
        let fresh35 = w;
        w = w.offset(1);
        let w1_5: f32 = *fresh35;
        let vz_5: *mut f32 = zlin
            .offset(
                (4 * i - 6 * 64) as isize,
            );
        let vy_5: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 6) * 64)
                    as isize,
            );
        j_5 = 0 as i32;
        while j_5 < 4 as i32 {
            b[j_5 as usize]
                += *vz_5.offset(j_5 as isize) * w1_5 + *vy_5.offset(j_5 as isize) * w0_5;
            a[j_5 as usize]
                += *vz_5.offset(j_5 as isize) * w0_5 - *vy_5.offset(j_5 as isize) * w1_5;
            j_5 += 1;
        }
        let mut j_6: i32 = 0;
        let fresh36 = w;
        w = w.offset(1);
        let w0_6: f32 = *fresh36;
        let fresh37 = w;
        w = w.offset(1);
        let w1_6: f32 = *fresh37;
        let vz_6: *mut f32 = zlin
            .offset(
                (4 * i - 7 * 64) as isize,
            );
        let vy_6: *mut f32 = zlin
            .offset(
                (4 * i
                    - (15 - 7) * 64)
                    as isize,
            );
        j_6 = 0 as i32;
        while j_6 < 4 as i32 {
            b[j_6 as usize]
                += *vz_6.offset(j_6 as isize) * w1_6 + *vy_6.offset(j_6 as isize) * w0_6;
            a[j_6 as usize]
                += *vy_6.offset(j_6 as isize) * w1_6 - *vz_6.offset(j_6 as isize) * w0_6;
            j_6 += 1;
        }
        dstl[dstr_off+((15 - i as u32) * nch) as usize] = mp3d_scale_pcm(a[1]);
        dstl[dstr_off+((17 + i as u32) * nch) as usize] = mp3d_scale_pcm(b[1]);
        dstl[((15 - i as u32) * nch) as usize] = mp3d_scale_pcm(a[0]);
        dstl[((17 + i as u32) * nch) as usize] = mp3d_scale_pcm(b[0 as i32 as usize]);
        dstl[dstr_off+((47 - i as u32) * nch) as usize] = mp3d_scale_pcm(a[3 as i32 as usize]);
        dstl[dstr_off+((49 + i as u32) * nch) as usize] = mp3d_scale_pcm(b[3 as i32 as usize]);
        dstl[((47 - i as u32) * nch) as usize] = mp3d_scale_pcm(a[2 as i32 as usize]);
        dstl[((49 + i as u32) * nch) as usize] = mp3d_scale_pcm(b[2 as i32 as usize]);
        i -= 1;
    }
}
unsafe fn mp3d_synth_granule(
    qmf_state: *mut f32,
    grbuf: *mut f32,
    nbands: u32,
    nch: u32,
    pcm: &mut [mp3d_sample_t],
    lins: *mut f32,
) {
    let mut i: usize = 0;
    while i < nch as usize {
        mp3d_DCT_II(grbuf.offset((576 * i) as isize), nbands);
        i += 1;
    }
    memcpy(
        lins as *mut (),
        qmf_state as *const (),
        (::core::mem::size_of::<f32>() as usize)
            .wrapping_mul(15 as i32 as usize)
            .wrapping_mul(64 as i32 as usize),
    );
    i = 0;
    while i < nbands as usize {
        mp3d_synth(
            grbuf.offset(i as isize),
            &mut pcm[32 * nch as usize * i..],
            nch,
            lins.offset((i * 64) as isize),
        );
        i += 2;
    }
    memcpy(
        qmf_state as *mut (),
        lins.offset((nbands * 64) as isize) as *const (),
        (::core::mem::size_of::<f32>() as usize)
            .wrapping_mul(15 as i32 as usize)
            .wrapping_mul(64 as i32 as usize),
    );
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

pub unsafe fn mp3dec_decode_frame(
    dec: &mut mp3dec_t,
    mp3: &[u8],
    mut pcm: &mut [mp3d_sample_t],
    info: &mut mp3dec_frame_info_t,
) -> i32 {
    let mut i: usize = 0;
    let mut igr = 0u32;
    let mut frame_size: usize = 0;
    let mut success: i32 = 1 as i32;
    let mut scratch: mp3dec_scratch_t = mp3dec_scratch_t {
        bs: bs_t {
            buf: core::ptr::null(),
            pos: 0,
            limit: 0,
        },
        maindata: [0; 2815],
        grbuf: [[0.; 576]; 2],
        scf: [0.; 40],
        syn: [[0.; 64]; 33],
        ist_pos: [[0; 39]; 2],
    };
    let mut scratch_gr_info = [L3_gr_info_t {
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
    }; 4];
    if mp3.len() > 4
        && (*dec).header[0 as i32 as usize] as i32 == 0xff as i32
        && hdr_compare(&dec.header, mp3)
    {
        frame_size = hdr_frame_bytes(mp3, (*dec).free_format_bytes) + hdr_padding(mp3);
        if frame_size != mp3.len()
            && (frame_size + 4 > mp3.len()
                || !hdr_compare(mp3, &mp3[frame_size..]))
        {
            frame_size = 0;
        }
    }
    if frame_size == 0 {
        *dec = mp3dec_t::new();
        i = mp3d_find_frame(
            mp3,
            &mut (*dec).free_format_bytes,
            &mut frame_size,
        );
        if frame_size == 0 || i + frame_size > mp3.len() {
            (*info).frame_bytes = i;
            return 0 as i32;
        }
    }
    let hdr = &mp3[i..];
    memcpy(
        ((*dec).header).as_mut_ptr() as *mut (),
        hdr.as_ptr() as *const (),
        4 as i32 as usize,
    );
    (*info).frame_bytes = i + frame_size;
    (*info).frame_offset = i;
    (*info).channels = if hdr[3] & 0xc0 == 0xc0 {
        1
    } else {
        2
    };
    (*info).hz = hdr_sample_rate_hz(hdr) as i32;
    (*info)
        .layer = 4
        - (hdr[1] >> 1
            & 3);
    (*info).bitrate_kbps = hdr_bitrate_kbps(hdr) as i32;
    if pcm.is_empty() {
        return hdr_frame_samples(hdr) as i32;
    }
    let mut bs_frame = bs_init(
        hdr[4..].as_ptr(),
        (frame_size - 4) as i32,
    );
    if hdr[1] & 1 == 0 {
        get_bits(&mut bs_frame, 16);
    }
    if (*info).layer == 3 {
        let main_data_begin: i32 = L3_read_side_info(
            &mut bs_frame,
            &mut scratch_gr_info,
            hdr,
        );
        if main_data_begin < 0 as i32
            || bs_frame.pos > bs_frame.limit
        {
            mp3dec_init(dec);
            return 0 as i32;
        }
        success = L3_restore_reservoir(
            dec,
            &mut bs_frame,
            &mut scratch,
            main_data_begin,
        );
        if success != 0 {
            igr = 0;
            while igr < (if hdr[1] & 0x8 != 0 { 2 } else { 1 })
            {
                scratch.grbuf.as_flattened_mut().fill(0f32);
                L3_decode(
                    dec,
                    &mut scratch,
                    &mut scratch_gr_info[(igr * (*info).channels) as usize..],
                    (*info).channels,
                );
                mp3d_synth_granule(
                    ((*dec).qmf_state).as_mut_ptr(),
                    scratch.grbuf.as_flattened_mut().as_mut_ptr(),
                    18,
                    (*info).channels,
                    pcm,
                    scratch.syn.as_flattened_mut().as_mut_ptr(),
                );
                igr += 1;
                pcm = &mut pcm[576 * ((*info).channels as usize)..];
            }
        }
        L3_save_reservoir(dec, &mut scratch);
    } else {
        return 0 as i32
    }
    return (success as u32)
        .wrapping_mul(hdr_frame_samples(&dec.header)) as i32;
}
