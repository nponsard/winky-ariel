#![no_std]

// New linky separators

use defmt::{Debug2Format, debug, info};
use serde::{Deserialize, Serialize};

const FRAME_START: u8 = 0x02;
const FRAME_END: u8 = 0x03;
// LF
const DATA_START: u8 = 0x0A;
// CR
const DATA_END: u8 = 0x0D;
// HT, this changes between historic and standard modes.
const SEPARATOR_STANDARD: u8 = 0x09;
const SEPARATOR_HISTORIC: u8 = 0x20;

pub const BAUDRATE_STANDARD: u32 = 9600;
pub const BAUDRATE_HISTORIC: u32 = 1200;

const LABEL_MAX_SIZE: usize = 16;
const DATA_MAX_SIZE: usize = 100;

const PROCESS_BUFFER_SIZE: usize = 4096;

// // Wh
// const ACTIVE_ENERGY_TOTAL_LABEL: &str = "EAST";
// // VA
// const APPARENT_POWER_LABEL: &str = "SINSTS";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Report {
    /// Label: EAST, unit: Wh
    pub active_energy_total: Option<u32>,
    /// Label: SINSTS, unit: VA
    pub apparent_power: Option<u32>,
}

pub struct Field {
    pub label: [u8; LABEL_MAX_SIZE],
    pub data: [u8; DATA_MAX_SIZE],
}
impl Field {
    pub fn new(ref_label: &[u8], ref_data: &[u8]) -> Self {
        let mut label = [0u8; LABEL_MAX_SIZE];
        let mut data = [0u8; DATA_MAX_SIZE];

        let len = ref_label.len().min(LABEL_MAX_SIZE);
        label[..len].copy_from_slice(&ref_label[..len]);

        let len = ref_data.len().min(DATA_MAX_SIZE);
        data[..len].copy_from_slice(&ref_data[..len]);

        Self { label, data }
    }
}

#[derive(Debug)]
pub enum ParserError {
    /// No frame was detected before the buffer filled up.
    BufferFull,
}

pub enum ParserStatus {
    /// No frame start symbol found yet, send more data.
    NoFrame,
    /// Found a frame start symbol, send more data.
    FrameNotFinished,
    /// Successfully parsed a frame.
    ParsedFrame(Report),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicMode {
    Historic,
    Standard,
}

impl TicMode {
    fn separator(&self) -> u8 {
        match self {
            Self::Historic => SEPARATOR_HISTORIC,
            Self::Standard => SEPARATOR_STANDARD,
        }
    }
    pub fn baudrate(&self) -> u32 {
        match self {
            Self::Historic => BAUDRATE_HISTORIC,
            Self::Standard => BAUDRATE_STANDARD,
        }
    }
}

pub struct TicParser {
    buffer: [u8; PROCESS_BUFFER_SIZE],
    current_index: usize,
    reading_a_frame: bool,
    mode: TicMode,
}
impl TicParser {
    pub fn new(mode: TicMode) -> Self {
        Self {
            buffer: [0; PROCESS_BUFFER_SIZE],
            current_index: 0,
            mode,
            reading_a_frame: false,
        }
    }

    pub fn add_data(&mut self, input: &[u8]) -> Result<ParserStatus, ParserError> {
        for char in input.iter() {
            if *char == FRAME_START {
                self.reading_a_frame = true;
                self.current_index = 0;
                self.buffer = [0; PROCESS_BUFFER_SIZE];
            } else if *char == FRAME_END {
                self.reading_a_frame = false;
                return self.process_frame(&self.buffer[..self.current_index]);
            } else if self.reading_a_frame {
                if self.current_index >= PROCESS_BUFFER_SIZE - 1 {
                    return Err(ParserError::BufferFull);
                }
                self.buffer[self.current_index] = *char;
                self.current_index += 1;
            }
        }
        if self.reading_a_frame {
            Ok(ParserStatus::FrameNotFinished)
        } else {
            Ok(ParserStatus::NoFrame)
        }
    }

    fn parse_buffer(&self, buffer: &[u8]) -> impl Iterator<Item = Field> {
        buffer.split(|c| *c == DATA_START).filter_map(|d| {
            let mut split = d.split(|c| *c == self.mode.separator());

            let ref_label = split.next()?;

            let ref_time_or_data = split.next()?;
            let ref_data_or_checksum = split.next()?;

            // If we have one more field it means whe had a timestamp in there.
            let ref_data = if split.next().is_some() {
                ref_data_or_checksum
            } else {
                ref_time_or_data
            };

            Some(Field::new(ref_label, ref_data))
        })
    }

    fn process_frame(&self, buffer: &[u8]) -> Result<ParserStatus, ParserError> {
        let data = unsafe { str::from_utf8_unchecked(buffer) };

        debug!("Frame: {}", data);

        let fields = self.parse_buffer(buffer);

        let mut report = Report::default();
        for field in fields {
            let Ok(label) = str::from_utf8(&field.label) else {
                continue;
            };
            let label = label.trim_matches(|c| c == char::from(0));
            match label {
                // PAPP is for historic mode.
                "SINSTS" | "PAPP" => {
                    let d = parse_unsigned_integer(&field.data);

                    report.apparent_power = d;
                }

                // BASE is for historic mode and depends on the contract.
                "EAST" | "BASE" => {
                    report.active_energy_total = parse_unsigned_integer(&field.data);
                }
                _ => {
                    debug!("Ignoring {}, {:?}", label, label.as_bytes());
                }
            }
        }

        info!("report : {:?}", Debug2Format(&report));
        Ok(ParserStatus::ParsedFrame(report))
    }
}
fn parse_unsigned_integer(bytes: &[u8]) -> Option<u32> {
    let s = str::from_utf8(bytes)
        .ok()?
        // Remove trailling null bytes.
        .trim_matches(|c| c == char::from(0));

    str::parse(s).ok()
}
