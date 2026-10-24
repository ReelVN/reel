use super::*;
use std::io::Cursor;
fn header_input() -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], bytes: &[u8]) {
        out.extend(tag);
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend(bytes);
    }
    let block_bytes = 96 * 4u16;
    let config = 0xfe00_0000u32 | (7 << 20) | (1 << 17) | (95 << 5) | (2 << 3);
    let mut fmt = [0u8; 52];
    fmt[0..2].copy_from_slice(&0xfffeu16.to_le_bytes());
    fmt[2..4].copy_from_slice(&2u16.to_le_bytes());
    fmt[4..8].copy_from_slice(&48000u32.to_le_bytes());
    fmt[12..14].copy_from_slice(&block_bytes.to_le_bytes());
    fmt[16..18].copy_from_slice(&34u16.to_le_bytes());
    fmt[18..20].copy_from_slice(&1024u16.to_le_bytes());
    fmt[20..24].copy_from_slice(&3u32.to_le_bytes());
    fmt[24..40].copy_from_slice(&[
        0xd2, 0x42, 0xe1, 0x47, 0xba, 0x36, 0x8d, 0x4d, 0x88, 0xfc, 0x61, 0x65, 0x4f, 0x8c, 0x83,
        0x6c,
    ]);
    fmt[40..44].copy_from_slice(&1u32.to_le_bytes());
    fmt[44..48].copy_from_slice(&config.to_be_bytes());
    let mut fact = [0u8; 12];
    fact[..4].copy_from_slice(&4003u32.to_le_bytes());
    fact[8..].copy_from_slice(&256u32.to_le_bytes());
    let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
    chunk(&mut out, b"fmt ", &fmt);
    chunk(&mut out, b"fact", &fact);
    // Geometry-only bytes, never passed to a codec or treated as encoded audio.
    chunk(&mut out, b"data", &vec![0; usize::from(block_bytes) * 5]);
    let size = out.len() as u32 - 8;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}

#[test]
fn audible_seek_ranges_keep_priming_overlap_and_final_padding_out_of_pcm() {
    let encoded = header_input();
    let at9 = encoded.as_slice();
    let header = inspect(&mut Cursor::new(at9), at9.len() as u64)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            header.format.rate,
            header.format.channels,
            header.format.frames
        ),
        (48000, 2, 4003)
    );
    assert_eq!(
        (header.delay, header.frame_samples, header.block_frames),
        (256, 256, 4)
    );
    for (sample, first_block, first_range) in [
        (0, 0, 256..1024),
        (767, 0, 1023..1024),
        (768, 0, 1024..1024),
        (1024, 1, 256..1024),
        (2049, 2, 257..1024),
        (3999, 3, 1024..1024),
    ] {
        let (offset, _) = header.seek(sample).unwrap();
        let block = (offset - header.data_offset) / u64::from(header.block_bytes);
        assert_eq!(block, first_block);
        assert_eq!(header.pcm_range(sample, block).unwrap(), first_range);
        let mut position = sample;
        let mut block = block;
        while position < header.format.frames {
            position += header.pcm_range(position, block).unwrap().len() as u64;
            block += 1;
        }
        assert_eq!(position, 4003);
    }
    assert_eq!(header.pcm_range(3999, 4).unwrap(), 159..163);
    assert!(header.seek(4004).is_err());
    assert!(header.pcm_range(0, u64::MAX).is_err());
    let mut source_rate = at9.to_vec();
    source_rate.extend(b"krSR");
    source_rate.extend(8u32.to_le_bytes());
    source_rate.extend(1u32.to_le_bytes());
    source_rate.extend(44100u32.to_le_bytes());
    let end = source_rate.len() as u32 - 8;
    source_rate[4..8].copy_from_slice(&end.to_le_bytes());
    let source_rate = inspect(&mut Cursor::new(&source_rate), source_rate.len() as u64)
        .unwrap()
        .unwrap();
    assert_eq!(
        (source_rate.format.rate, source_rate.codec_rate),
        (44100, 48000)
    );
    assert_eq!(source_rate.seek(1024).unwrap(), header.seek(1024).unwrap());
    for length in [12, 19] {
        assert!(
            inspect(&mut Cursor::new(&at9[..length]), length as u64)
                .unwrap()
                .is_none()
        );
    }
    for length in [71, 99, at9.len() - 1] {
        assert!(inspect(&mut Cursor::new(&at9[..length]), length as u64).is_err());
    }
}
