use super::*;

// Available in the public domain as a work of the United States government.
// https://www.marineband.marines.mil/Audio-Resources/The-Complete-Marches-of-John-Philip-Sousa/
const THE_WASHINGTON_POST_MARCH: &[u8] = include_bytes!("tests/The Washington Post.mp3");

#[test]
fn measure_length_of_march() {
    let mut march = THE_WASHINGTON_POST_MARCH;
    let mut decoder = Decoder::new();
    let mut pcm_buffer = [0f32; MAX_SAMPLES_PER_FRAME];
    let mut n = 0;
    while !march.is_empty() {
        let (mp3_consumed, frame_info) = decoder.decode(march, &mut pcm_buffer);
        march = &march[mp3_consumed..];
        if let Ok(frame_info) = frame_info {
            assert_eq!(frame_info.layer, 3);
            assert_eq!(frame_info.bitrate, 320);
            assert_eq!(frame_info.sample_rate, 48000);
            assert_eq!(frame_info.channels, Channels::Stereo);
            n += frame_info.samples_produced;
        }
    }
    
    assert_eq!(n, 243072);
}
#[test]
fn i16_rounds_half_away_from_zero() {
    let conv = |x: f32| i16::from_synth(x, false);
    assert_eq!(conv(0.0), 0);
    assert_eq!(conv(0.49999997), 0); // (x + 0.5) would round up to 1.0
    assert_eq!(conv(0.5), 1);
    assert_eq!(conv(-0.49999997), 0);
    assert_eq!(conv(-0.5), -1);
    assert_eq!(conv(-1.2), -1);
    assert_eq!(conv(-1.5), -2);
    assert_eq!(conv(2.5), 3);
    assert_eq!(conv(-2.5), -3);
    assert_eq!(conv(32766.4), 32766);
    assert_eq!(conv(32766.5), 32767);
    assert_eq!(conv(1e9), i16::MAX);
    assert_eq!(conv(-32767.5), i16::MIN);
    assert_eq!(conv(-1e9), i16::MIN);
}

#[test]
fn i16_minimp3_compat_keeps_the_quirk() {
    let conv = |x: f32| i16::from_synth(x, true);
    // minimp3 rounds (-1.5, -0.5] to 0; the default conversion gives -1.
    assert_eq!(conv(-0.5), 0);
    assert_eq!(conv(-1.2), 0);
    assert_eq!(conv(-1.5), -2);
    assert_eq!(conv(0.49999997), 1); // float rounding in (x + 0.5)
    assert_eq!(conv(2.5), 3);
    assert_eq!(conv(1e9), i16::MAX);
    assert_eq!(conv(-1e9), i16::MIN);
}

#[test]
fn other_integer_formats() {
    assert_eq!(i8::from_synth(128.0, false), 1); // 0.5 in 8-bit units
    assert_eq!(i8::from_synth(-128.0, false), -1);
    assert_eq!(i8::from_synth(32767.0, false), i8::MAX);
    assert_eq!(i8::from_synth(-32768.0, false), i8::MIN);
    assert_eq!(u8::from_synth(0.0, false), 128);
    assert_eq!(u8::from_synth(-32768.0, false), 0);
    assert_eq!(u8::from_synth(1e9, false), 255);
    assert_eq!(u16::from_synth(0.0, false), 32768);
    assert_eq!(u16::from_synth(-1.2, true), 32767); // compat doesn't apply to u16
    assert_eq!(i32::from_synth(1.0, false), 65536);
    assert_eq!(i32::from_synth(-32768.0, false), i32::MIN);
    assert_eq!(i32::from_synth(1e9, false), i32::MAX);
    assert_eq!(i32::from_synth(f32::NAN, false), 0);
    assert_eq!(u32::from_synth(0.0, false), 1 << 31);
    assert_eq!(u32::from_synth(-1e9, false), 0);
}

#[test]
fn decode_errors() {
    let mut decoder = Decoder::new();
    let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];

    // Nothing, and junk, are consumed whole.
    assert_eq!(decoder.decode(&[], &mut pcm), (0, Err(DecodeError::NoFrame)));
    assert_eq!(decoder.decode(&[0x55; 1000], &mut pcm), (1000, Err(DecodeError::NoFrame)));

    // Starting mid-stream, the first frames can't be decoded until the bit
    // reservoir has been filled; they're still identified.
    let mut march = &THE_WASHINGTON_POST_MARCH[THE_WASHINGTON_POST_MARCH.len() / 2..];
    let mut reservoir_misses = 0;
    loop {
        let (n, result) = decoder.decode(march, &mut pcm);
        march = &march[n..];
        match result {
            Ok(info) => {
                assert_eq!((info.layer, info.sample_rate), (3, 48000));
                break;
            }
            Err(DecodeError::ReservoirUnavailable(info)) => {
                assert_eq!((info.layer, info.sample_rate, info.samples_produced), (3, 48000, 0));
                reservoir_misses += 1;
            }
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    assert!(reservoir_misses > 0);

    // A frame with an impossible side info (big_values > 288) is corrupt.
    let frame_at = march.iter().position(|&b| b == 0xff).unwrap();
    let mut corrupt = march.to_vec();
    // Stereo MPEG-1 side info: main_data_begin(9) private(3) scfsi(8), then
    // part2_3_length(12) and big_values(9) of granule 0, channel 0.
    let side = frame_at + 4;
    corrupt[side + 4] = 0xff; // big_values bits
    corrupt[side + 5] = 0xff;
    let mut decoder = Decoder::new();
    let mut saw_corrupt = false;
    let mut input = &corrupt[frame_at..];
    for _ in 0..4 {
        let (n, result) = decoder.decode(input, &mut pcm);
        input = &input[n..];
        if let Err(DecodeError::Corrupt(info)) = result {
            assert_eq!(info.layer, 3);
            saw_corrupt = true;
        }
    }
    assert!(saw_corrupt);
}

#[test]
fn layer_2_frames() {
    // MPEG-1 Layer II, 128 kbps, 44.1 kHz, mono, no CRC: 417-byte frames of
    // silence (all bit allocations zero).
    let mut stream = [0u8; 4 * 417];
    for frame in stream.chunks_exact_mut(417) {
        frame[..4].copy_from_slice(&[0xff, 0xfd, 0x80, 0xc0]);
    }
    let mut decoder = Decoder::new();
    let mut pcm = [0i16; MAX_SAMPLES_PER_FRAME];
    let (n, result) = decoder.decode(&stream, &mut pcm);
    assert_eq!(n, 417);
    let info = match result {
        Ok(info) if cfg!(feature = "layer12") => info,
        Err(DecodeError::UnsupportedLayer(info)) if !cfg!(feature = "layer12") => info,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!((info.layer, info.sample_rate, info.bitrate, info.channels), (2, 44100, 128, Channels::Mono));
}
