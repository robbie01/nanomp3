use std::{io::{self, Write}, marker::PhantomData};

use byteorder::{BigEndian, WriteBytesExt as _};

pub trait SampleFormat {
    const CODE: u32;

    fn write_sample<W: Write>(self, writer: &mut W) -> io::Result<()>;
}

impl SampleFormat for i16 {
    const CODE: u32 = 3;

    fn write_sample<W: Write>(self, writer: &mut W) -> io::Result<()> {
        writer.write_i16::<BigEndian>(self)
    }
}

impl SampleFormat for f32 {
    const CODE: u32 = 6;

    fn write_sample<W: Write>(self, writer: &mut W) -> io::Result<()> {
        writer.write_f32::<BigEndian>(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initial,
    WritingHeader,
    WrittenHeader
}

#[derive(Debug)]
pub struct AuWriter<W, F> {
    phase: Phase,
    writer: W,
    _phantom: PhantomData<fn(F)>
}

impl<W, F> AuWriter<W, F> {
    pub const fn new(writer: W) -> Self {
        Self { 
            phase: Phase::Initial,
            writer,
            _phantom: PhantomData
        }
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

const MAGIC: u32 = 0x2e736e64;
const HEADER_SIZE: u32 = 28;
const UNKNOWN_DATA_SIZE: u32 = 0xFFFFFFFF;

impl<W: Write, F: SampleFormat> AuWriter<W, F> {
    pub fn write_header(&mut self, sample_rate: u32, channels: u32) -> io::Result<()> {
        assert_eq!(Phase::Initial, self.phase);
        self.phase = Phase::WritingHeader;

        self.writer.write_u32::<BigEndian>(MAGIC)?;
        self.writer.write_u32::<BigEndian>(HEADER_SIZE)?;
        self.writer.write_u32::<BigEndian>(UNKNOWN_DATA_SIZE)?;
        self.writer.write_u32::<BigEndian>(F::CODE)?;
        self.writer.write_u32::<BigEndian>(sample_rate)?;
        self.writer.write_u32::<BigEndian>(channels)?;
        self.writer.write_u32::<BigEndian>(0)?;

        self.phase = Phase::WrittenHeader;
        
        Ok(())
    }

    pub fn write_sample(&mut self, sample: F) -> io::Result<()> {
        assert_eq!(Phase::WrittenHeader, self.phase);

        sample.write_sample(&mut self.writer)
    }
}