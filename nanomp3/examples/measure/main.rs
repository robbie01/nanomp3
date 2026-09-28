mod snd;

use std::{env, error::Error, fs::File, io::{BufReader, BufWriter, Write}};

/// Convert an MP3 file to a Sun Au (.snd) file and print its length.
///
/// Note: this example is *not* correct for files whose sample rate or number of
/// channels changes mid-stream.
fn main() -> Result<(), Box<dyn Error>> {
    let (Some(src), Some(dest)) = (env::args_os().nth(1), env::args_os().nth(2)) else {
        eprintln!("usage: measure <file.mp3> <output.snd>");
        return Ok(());
    };

    // The reader skips tags, trims the encoder delay/padding, and buffers input.
    let mut reader = nanomp3::Reader::<_, f32>::new(BufReader::new(File::open(src)?))?;
    let Some(channels) = reader.channels() else {
        eprintln!("no MPEG audio found");
        return Ok(());
    };

    let mut snd = snd::AuWriter::new(BufWriter::new(File::create(dest)?));
    snd.write_header(reader.sample_rate(), channels.num().into())?;

    let mut pcm = vec![0f32; 4096];
    let mut samples = 0;
    loop {
        let n = reader.read(&mut pcm)?;
        if n == 0 {
            break;
        }
        for &sample in &pcm[..n] {
            snd.write_sample(sample)?;
        }
        samples += n;
    }
    snd.into_inner().flush()?;

    let time = samples as f64 / f64::from(channels.num()) / f64::from(reader.sample_rate());
    println!("{}m{}s", (time / 60.).floor(), (time % 60.).floor());
    Ok(())
}
