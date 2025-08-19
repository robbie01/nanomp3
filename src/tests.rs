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