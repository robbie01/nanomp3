mod buffer;
mod snd;

use std::{env, fs::File, io::{BufWriter, Read as _, Write}};

use buffer::Buffer;

const MIN_BUFFER_SIZE: usize = 16384;

// Convert an MP3 file to a Sun Au (.snd) file
fn main() {
    let (mut file, dest) = match (env::args_os().nth(1), env::args_os().nth(2)) {
        (Some(arg1), Some(arg2)) => (
            File::open(arg1).unwrap(),
            BufWriter::new(File::create(arg2).unwrap())
        ),
        _ => {
            eprintln!("usage: measure <file.mp3> <output.snd>");
            return;
        }
    };

    let mut snd = snd::AuWriter::new(dest);
    let mut written_header = false;
    
    let mut decoder = nanomp3::Decoder::new();
    let mut mp3_buffer = Buffer::<{128*MIN_BUFFER_SIZE}>::new();
    let mut eos = false;
    let mut pcm_buffer = [0f32; nanomp3::MAX_SAMPLES_PER_FRAME];

    let mut time = 0.;
    
    loop {
        if mp3_buffer.is_empty() {
            if eos {
                // End of stream reached
                break;
            }
        } else {
            let (consumed, info) = decoder.decode(mp3_buffer.data(), &mut pcm_buffer);
            mp3_buffer.consume(consumed);
            if let Some(info) = info {
                if !written_header {
                    snd.write_header(info.sample_rate, info.channels.num().into()).unwrap();
                    written_header = true;
                }

                let n = info.samples_produced * usize::from(info.channels.num());

                for &sample in &pcm_buffer[..n] {
                    snd.write_sample(sample).unwrap();
                }
                // println!("{info:?}");
                time += (info.samples_produced as f64) / (info.sample_rate as f64);
            }
        }

        if !eos && mp3_buffer.len() < MIN_BUFFER_SIZE {
            mp3_buffer.reclaim();

            // Read until buffer is sufficiently full or EOS
            loop {
                let n = file.read(mp3_buffer.remaining_capacity()).unwrap();
                if n == 0 {
                    eos = true;
                }
                mp3_buffer.expand(n);
                println!("{}", mp3_buffer.len());

                if eos || mp3_buffer.len() >= MIN_BUFFER_SIZE {
                    break;
                }
            }
        }
    }

    snd.into_inner().flush().unwrap();

    let m = (time / 60.).floor();
    let s = (time % 60.).floor();
    println!("{m}m{s}s");
}
