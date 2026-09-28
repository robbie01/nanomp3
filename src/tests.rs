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
        if let Some(frame_info) = frame_info {
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
