//! Encoding Chunks for upload: 16 kHz mono Opus in an OGG container (ADR-0004, RFC 7845).

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

use ogg::writing::{PacketWriteEndInfo, PacketWriter};

const BITRATE: i32 = 24_000;
/// 20 ms frames.
const FRAME_MS: u32 = 20;
/// Ogg Opus granule positions always count 48 kHz samples.
const GRANULE_RATE: u64 = 48_000;
const SERIAL: u32 = 0x6d6f_6e79;

/// Writes mono s16 samples to `path` as Ogg Opus at about 24 kbps. `sample_rate` must be one Opus accepts
/// (8, 12, 16, 24 or 48 kHz).
pub fn write_ogg_opus(path: &Path, sample_rate: u32, samples: &[i16]) -> io::Result<()> {
    let err = |e: opus::Error| io::Error::other(e.to_string());
    let mut encoder = opus::Encoder::new(sample_rate, opus::Channels::Mono, opus::Application::Voip).map_err(err)?;
    encoder.set_bitrate(opus::Bitrate::Bits(BITRATE)).map_err(err)?;

    let scale = GRANULE_RATE / sample_rate as u64;
    let lookahead = encoder.get_lookahead().map_err(err)? as usize;
    let pre_skip = lookahead as u64 * scale;
    let frame = (sample_rate * FRAME_MS / 1000) as usize;
    // Encode enough frames to flush the encoder's lookahead, then trim the end with the final granule.
    let frames = (samples.len() + lookahead).div_ceil(frame).max(1);
    let final_granule = pre_skip + samples.len() as u64 * scale;

    let mut writer = PacketWriter::new(BufWriter::new(File::create(path)?));
    writer.write_packet(opus_head(sample_rate, pre_skip as u16), SERIAL, PacketWriteEndInfo::EndPage, 0)?;
    writer.write_packet(opus_tags(), SERIAL, PacketWriteEndInfo::EndPage, 0)?;

    let mut pcm = vec![0i16; frame];
    let mut packet = vec![0u8; 4000];
    for i in 0..frames {
        let from = (i * frame).min(samples.len());
        let to = ((i + 1) * frame).min(samples.len());
        pcm.fill(0);
        pcm[..to - from].copy_from_slice(&samples[from..to]);
        let len = encoder.encode(&pcm, &mut packet).map_err(err)?;

        let last = i + 1 == frames;
        // Granules count every decoded sample, pre-skip included; the last one trims the padding.
        let granule = (((i + 1) * frame) as u64 * scale).min(final_granule);
        let end = if last {
            PacketWriteEndInfo::EndStream
        } else if (i + 1) % 50 == 0 {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        writer.write_packet(packet[..len].to_vec(), SERIAL, end, granule)?;
    }
    Ok(())
}

fn opus_head(sample_rate: u32, pre_skip: u16) -> Vec<u8> {
    let mut head = b"OpusHead".to_vec();
    head.push(1); // version
    head.push(1); // channels
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&sample_rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family: mono/stereo
    head
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"tiro";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    tags
}
