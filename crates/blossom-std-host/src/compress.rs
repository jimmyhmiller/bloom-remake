//! Host functions of `blossom_std::compress` (FOREIGN-PROTOCOLS §4): gzip, snappy (the raw block format), LZ4 (the
//! frame format) and Zstandard, each a compressor and a bounded decompressor.
//!
//! A decompressor takes the largest output it may produce and returns `None` past it, or on any malformed input; it
//! never allocates beyond that bound plus the codec's own working state, and never panics. The working state is
//! bounded whatever the input claims: LZ4's is its largest block size (4 MB) twice plus its 64 KB window; a Zstandard
//! frame whose window is larger than both the bound and 8 MB is refused. Compressors are
//! deterministic: the same input always gives the same bytes (gzip writes no timestamp).

use std::io::{Read, Write};
use std::sync::Arc;

use blossom_value::Value;
use blossom_value::externs::ExternError;

use crate::host::{bytes_arg, register_std, u8_arg, u64_arg};

/// All of `r`, if it yields at most `max` bytes; `None` past `max` or on a read error.
fn bounded(r: impl Read, max: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    r.take(max.saturating_add(1)).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= max).then_some(out)
}

fn gzip_compress(input: &[u8], level: u8) -> Result<Vec<u8>, ExternError> {
    if level > 9 {
        return Err(ExternError::Failed(format!("gzip level {level} is not 0 to 9").into()));
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(u32::from(level)));
    enc.write_all(input)
        .and_then(|()| enc.finish())
        .map_err(|e| ExternError::Failed(format!("gzip: {e}").into()))
}

/// Every gzip member of `input`, concatenated (RFC 1952 allows several).
fn gzip_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    bounded(flate2::read::MultiGzDecoder::new(input), max)
}

fn snappy_compress(input: &[u8]) -> Result<Vec<u8>, ExternError> {
    snap::raw::Encoder::new()
        .compress_vec(input)
        .map_err(|e| ExternError::Failed(format!("snappy: {e}").into()))
}

/// A raw snappy block: its header states the output length, checked against `max` before decoding.
fn snappy_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    let len = snap::raw::decompress_len(input).ok()?;
    if len as u64 > max {
        return None;
    }
    snap::raw::Decoder::new().decompress_vec(input).ok()
}

fn lz4_compress(input: &[u8]) -> Result<Vec<u8>, ExternError> {
    let mut enc = lz4_flex::frame::FrameEncoder::new(Vec::new());
    enc.write_all(input)
        .map_err(|e| ExternError::Failed(format!("lz4: {e}").into()))?;
    enc.finish()
        .map_err(|e| ExternError::Failed(format!("lz4: {e}").into()))
}

/// Every LZ4 frame of `input`, concatenated, skipping skippable frames, as the frame format specifies. Each frame's
/// extent is found from its block sizes first, so a frame is decoded from exactly its own bytes and anything after the
/// last frame that is not a whole frame is malformed. The legacy format (a different format) is refused.
fn lz4_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    let mut src = input;
    let mut out = Vec::new();
    while !src.is_empty() {
        let len = match lz4_frame(src)? {
            Frame::Skippable(len) => len,
            Frame::Data(len) => {
                let room = max.checked_sub(out.len() as u64)?;
                out.extend(bounded(lz4_flex::frame::FrameDecoder::new(src.get(..len)?), room)?);
                len
            }
        };
        src = src.get(len..)?;
    }
    Some(out)
}

/// A frame at the front of a buffer, and the bytes it takes.
enum Frame {
    Skippable(usize),
    Data(usize),
}

/// A little-endian `u32` at `at`.
fn le32(b: &[u8], at: usize) -> Option<u32> {
    let x: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(x))
}

/// Magic numbers of skippable frames, shared by LZ4 and Zstandard.
const SKIPPABLE: std::ops::RangeInclusive<u32> = 0x184D_2A50..=0x184D_2A5F;

/// A skippable frame's extent: the magic, a 4-byte length, and that many bytes.
fn skippable(b: &[u8]) -> Option<Frame> {
    let n = usize::try_from(le32(b, 4)?).ok()?;
    let len = n.checked_add(8)?;
    (len <= b.len()).then_some(Frame::Skippable(len))
}

/// The LZ4 frame at the front of `b` (the LZ4 frame format, v1.6.4): its header, then blocks each prefixed by a
/// 4-byte size whose high bit marks a stored block, each followed by a checksum when the header says so, a zero
/// end mark, and a content checksum when the header says so. Only the extent is found here; the decoder checks the
/// rest.
fn lz4_frame(b: &[u8]) -> Option<Frame> {
    let magic = le32(b, 0)?;
    if SKIPPABLE.contains(&magic) {
        return skippable(b);
    }
    if magic != 0x184D_2204 {
        return None;
    }
    let flg = *b.get(4)?;
    let block_checksum = flg & 0x10 != 0;
    let content_checksum = flg & 0x04 != 0;
    // Magic, FLG, BD, the content size and dictionary id if present, and the header checksum.
    let mut at = 7 + if flg & 0x08 != 0 { 8 } else { 0 } + if flg & 0x01 != 0 { 4 } else { 0 };
    loop {
        let size = le32(b, at)?;
        at = at.checked_add(4)?;
        if size == 0 {
            break;
        }
        let data = usize::try_from(size & 0x7FFF_FFFF).ok()?;
        at = at.checked_add(data)?.checked_add(if block_checksum { 4 } else { 0 })?;
    }
    let end = at.checked_add(if content_checksum { 4 } else { 0 })?;
    (end <= b.len()).then_some(Frame::Data(end))
}

fn zstd_compress(input: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(input, ruzstd::encoding::CompressionLevel::Fastest)
}

/// The largest window a Zstandard frame may ask for when it is larger than the output bound: the 8 MB RFC 8878
/// recommends every decoder support. The decoder allocates the whole window up front, so without this a 20-byte
/// frame could make it allocate 100 MB (the most `ruzstd` allows) whatever the bound.
const ZSTD_WINDOW_FLOOR: u64 = 8 << 20;

/// What a Zstandard frame header declares (RFC 8878 §3.1.1.1).
struct ZstdHeader {
    window: u64,
    content: Option<u64>,
    checksum: bool,
}

/// The Zstandard frame header at the front of `b`, or `None` if it is not a frame (the magic number is checked by
/// the caller). The decoder parses the header again; this reads only what the bounds need.
fn zstd_header(b: &[u8]) -> Option<ZstdHeader> {
    let d = *b.get(4)?;
    let (single, checksum) = (d & 0x20 != 0, d & 0x04 != 0);
    let mut at = 5;
    let window_descriptor = if single {
        None
    } else {
        at += 1;
        Some(*b.get(5)?)
    };
    at += match d & 0x03 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let fcs_len = match (d >> 6, single) {
        (0, false) => 0,
        (0, true) => 1,
        (1, _) => 2,
        (2, _) => 4,
        _ => 8,
    };
    let content = if fcs_len == 0 {
        None
    } else {
        let v = b
            .get(at..at + fcs_len)?
            .iter()
            .rev()
            .fold(0u64, |v, x| v << 8 | u64::from(*x));
        Some(if fcs_len == 2 { v + 256 } else { v })
    };
    let window = match window_descriptor {
        Some(w) => {
            let base = 1u64 << (10 + u32::from(w >> 3));
            base + base / 8 * u64::from(w & 7)
        }
        // A single-segment frame's window is its content size, which it then always declares.
        None => content?,
    };
    Some(ZstdHeader {
        window,
        content,
        checksum,
    })
}

/// Every Zstandard frame of `input`, concatenated, skipping skippable frames. A frame is refused when its window is
/// larger than both `max` and 8 MB, when it declares a content size other than what it decodes to, and when its
/// content checksum does not match.
fn zstd_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    let mut src = input;
    let mut out = Vec::new();
    while !src.is_empty() {
        let magic = le32(src, 0)?;
        if SKIPPABLE.contains(&magic) {
            let Frame::Skippable(len) = skippable(src)? else {
                return None;
            };
            src = src.get(len..)?;
            continue;
        }
        if magic != 0xFD2F_B528 {
            return None;
        }
        let header = zstd_header(src)?;
        let room = max.checked_sub(out.len() as u64)?;
        if header.window > max.max(ZSTD_WINDOW_FLOOR) || header.content.is_some_and(|n| n > room) {
            return None;
        }
        let (before, start) = (src.len(), out.len());
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut src).ok()?;
        (&mut dec).take(room.saturating_add(1)).read_to_end(&mut out).ok()?;
        let frame = dec.into_frame_decoder();
        let produced = (out.len() - start) as u64;
        let whole = frame.is_finished()
            && header.content.is_none_or(|n| n == produced)
            && (!header.checksum || frame.get_checksum_from_data() == frame.get_calculated_checksum());
        if !whole || out.len() as u64 > max || src.len() >= before {
            return None;
        }
    }
    Some(out)
}

fn two<'a>(name: &str, args: &'a [Value]) -> Result<(&'a Value, &'a Value), ExternError> {
    match args {
        [a, b] => Ok((a, b)),
        _ => Err(ExternError::InvalidArguments(
            format!("{name} takes two arguments").into(),
        )),
    }
}

fn one<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value, ExternError> {
    match args {
        [a] => Ok(a),
        _ => Err(ExternError::InvalidArguments(
            format!("{name} takes one argument").into(),
        )),
    }
}

fn bytes(v: Vec<u8>) -> Value {
    Value::Bytes(Arc::from(v))
}

fn optional(v: Option<Vec<u8>>) -> Value {
    Value::Option(v.map(|b| Arc::new(bytes(b))))
}

pub fn register(reg: &mut blossom_value::ExternRegistry) -> Result<(), blossom_value::error::ValueError> {
    register_std(reg, "blossom_std::compress::gzip_compress", |args: &[Value]| {
        let (b, level) = two("gzip_compress", args)?;
        gzip_compress(bytes_arg(b)?, u8_arg(level)?).map(bytes)
    })?;
    register_std(reg, "blossom_std::compress::gzip_decompress", |args: &[Value]| {
        let (b, max) = two("gzip_decompress", args)?;
        Ok(optional(gzip_decompress(bytes_arg(b)?, u64_arg(max)?)))
    })?;
    register_std(reg, "blossom_std::compress::snappy_compress", |args: &[Value]| {
        snappy_compress(bytes_arg(one("snappy_compress", args)?)?).map(bytes)
    })?;
    register_std(reg, "blossom_std::compress::snappy_decompress", |args: &[Value]| {
        let (b, max) = two("snappy_decompress", args)?;
        Ok(optional(snappy_decompress(bytes_arg(b)?, u64_arg(max)?)))
    })?;
    register_std(reg, "blossom_std::compress::lz4_compress", |args: &[Value]| {
        lz4_compress(bytes_arg(one("lz4_compress", args)?)?).map(bytes)
    })?;
    register_std(reg, "blossom_std::compress::lz4_decompress", |args: &[Value]| {
        let (b, max) = two("lz4_decompress", args)?;
        Ok(optional(lz4_decompress(bytes_arg(b)?, u64_arg(max)?)))
    })?;
    register_std(reg, "blossom_std::compress::zstd_compress", |args: &[Value]| {
        Ok(bytes(zstd_compress(bytes_arg(one("zstd_compress", args)?)?)))
    })?;
    register_std(reg, "blossom_std::compress::zstd_decompress", |args: &[Value]| {
        let (b, max) = two("zstd_decompress", args)?;
        Ok(optional(zstd_decompress(bytes_arg(b)?, u64_arg(max)?)))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        /// Compressible bytes: runs drawn from a small alphabet, with some noise.
        fn data(&mut self) -> Vec<u8> {
            let n = self.below(5000) as usize;
            let mut v = Vec::with_capacity(n);
            while v.len() < n {
                let byte = if self.below(4) == 0 {
                    self.next() as u8
                } else {
                    b'a' + self.below(4) as u8
                };
                let run = 1 + self.below(20) as usize;
                v.extend(std::iter::repeat_n(byte, run));
            }
            v.truncate(n);
            v
        }
    }

    type Codec = (&'static str, fn(&[u8]) -> Vec<u8>, fn(&[u8], u64) -> Option<Vec<u8>>);

    fn codecs() -> Vec<Codec> {
        vec![
            ("gzip", |b| gzip_compress(b, 6).unwrap(), gzip_decompress),
            ("snappy", |b| snappy_compress(b).unwrap(), snappy_decompress),
            ("lz4", |b| lz4_compress(b).unwrap(), lz4_decompress),
            ("zstd", zstd_compress, zstd_decompress),
        ]
    }

    #[test]
    fn every_codec_round_trips_and_is_deterministic() {
        let mut rng = Rng(7);
        for _ in 0..60 {
            let data = rng.data();
            for (name, compress, decompress) in codecs() {
                let c = compress(&data);
                assert_eq!(c, compress(&data), "{name}: compression is deterministic");
                assert_eq!(
                    decompress(&c, data.len() as u64).as_deref(),
                    Some(data.as_slice()),
                    "{name}"
                );
                assert_eq!(decompress(&c, u64::MAX).as_deref(), Some(data.as_slice()), "{name}");
                if !data.is_empty() {
                    assert_eq!(
                        decompress(&c, data.len() as u64 - 1),
                        None,
                        "{name}: the bound is exact"
                    );
                }
            }
        }
    }

    #[test]
    fn gzip_levels_all_decode_and_an_out_of_range_level_fails() {
        let data = b"the quick brown fox jumps over the lazy dog, again and again and again".repeat(20);
        for level in 0..=9 {
            let c = gzip_compress(&data, level).unwrap();
            assert_eq!(gzip_decompress(&c, 1 << 20).as_deref(), Some(data.as_slice()));
        }
        assert!(matches!(gzip_compress(&data, 10), Err(ExternError::Failed(_))));
    }

    #[test]
    fn concatenated_gzip_members_and_zstd_frames_decode_whole() {
        let (a, b) = (b"first part ".repeat(30), b"second part".repeat(40));
        let whole = [a.clone(), b.clone()].concat();
        let gz = [gzip_compress(&a, 6).unwrap(), gzip_compress(&b, 1).unwrap()].concat();
        assert_eq!(gzip_decompress(&gz, 1 << 20), Some(whole.clone()));
        let zs = [zstd_compress(&a), zstd_compress(&b)].concat();
        assert_eq!(zstd_decompress(&zs, 1 << 20), Some(whole.clone()));
        assert_eq!(zstd_decompress(&zs, whole.len() as u64 - 1), None);
    }

    #[test]
    fn malformed_input_is_none_never_a_panic() {
        let mut rng = Rng(11);
        for (name, compress, decompress) in codecs() {
            assert_eq!(decompress(&[], 1 << 20), None, "{name}: empty input");
            for _ in 0..400 {
                // Random bytes, and valid streams with bytes flipped, truncated or extended.
                let noise: Vec<u8> = (0..rng.below(200)).map(|_| rng.next() as u8).collect();
                let _ = decompress(&noise, 1 << 16);
                let mut c = compress(&rng.data());
                match rng.below(3) {
                    0 if !c.is_empty() => {
                        let i = rng.below(c.len() as u64) as usize;
                        c[i] ^= 1 << rng.below(8);
                    }
                    1 => c.truncate(rng.below(c.len() as u64 + 1) as usize),
                    _ => c.extend((0..rng.below(8)).map(|_| rng.next() as u8)),
                }
                let _ = decompress(&c, 1 << 16);
            }
        }
    }

    #[test]
    fn a_small_bound_stops_a_large_output() {
        // 10 MB of zeros compresses to little; a 1 KB bound refuses it.
        let zeros = vec![0u8; 10 << 20];
        for (name, compress, decompress) in codecs() {
            let c = compress(&zeros);
            assert!(c.len() < zeros.len() / 10, "{name}");
            assert_eq!(decompress(&c, 1024), None, "{name}");
        }
    }

    /// A Zstandard frame by hand (RFC 8878): the magic, a frame header descriptor `fhd`, the header's other fields
    /// `rest`, one last raw block holding `data`, and `checksum` if given.
    fn zstd_raw(fhd: u8, rest: &[u8], data: &[u8], checksum: Option<u32>) -> Vec<u8> {
        let mut f = 0xFD2F_B528u32.to_le_bytes().to_vec();
        f.push(fhd);
        f.extend(rest);
        let block = 1u32 | (data.len() as u32) << 3;
        f.extend(&block.to_le_bytes()[..3]);
        f.extend(data);
        if let Some(c) = checksum {
            f.extend(c.to_le_bytes());
        }
        f
    }

    #[test]
    fn a_zstd_window_larger_than_the_bound_and_8_mb_is_refused() {
        let data = b"hello, window";
        // Window descriptors: exponent 13 is 8 MB, exponent 16 is 64 MB.
        let small = zstd_raw(0x00, &[13 << 3], data, None);
        let large = zstd_raw(0x00, &[16 << 3], data, None);
        assert_eq!(zstd_decompress(&small, 1024).as_deref(), Some(&data[..]));
        assert_eq!(zstd_decompress(&large, 1024), None, "a 64 MB window for a 1 KB bound");
        assert_eq!(zstd_decompress(&large, 64 << 20).as_deref(), Some(&data[..]));
    }

    #[test]
    fn a_zstd_frame_must_decode_to_its_declared_size_and_checksum() {
        let data = b"declared";
        // Single segment with a 1-byte content size.
        let right = zstd_raw(0x20, &[data.len() as u8], data, None);
        let wrong = zstd_raw(0x20, &[data.len() as u8 + 2], data, None);
        assert_eq!(zstd_decompress(&right, 1024).as_deref(), Some(&data[..]));
        assert_eq!(zstd_decompress(&wrong, 1024), None, "a content size other than the content");
        // A declared size over the bound is refused before decoding.
        assert_eq!(zstd_decompress(&right, data.len() as u64 - 1), None);
        // The checksum ruzstd computes for `data`, read back from a decode with a placeholder checksum.
        let probe = zstd_raw(0x04, &[13 << 3], data, Some(0));
        let mut src = &probe[..];
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut src).unwrap();
        let mut sink = Vec::new();
        dec.read_to_end(&mut sink).unwrap();
        let sum = dec.into_frame_decoder().get_calculated_checksum().unwrap();
        let good = zstd_raw(0x04, &[13 << 3], data, Some(sum));
        let bad = zstd_raw(0x04, &[13 << 3], data, Some(sum ^ 1));
        assert_eq!(zstd_decompress(&good, 1024).as_deref(), Some(&data[..]));
        assert_eq!(zstd_decompress(&bad, 1024), None, "a checksum that does not match");
    }

    /// A skippable frame (LZ4 and Zstandard share the format) with `n` bytes of user data.
    fn skip_frame(n: usize) -> Vec<u8> {
        let mut f = 0x184D_2A53u32.to_le_bytes().to_vec();
        f.extend((n as u32).to_le_bytes());
        f.extend(std::iter::repeat_n(0xAB, n));
        f
    }

    #[test]
    fn frames_decode_whole_skippable_frames_are_skipped_and_trailing_bytes_refused() {
        let (a, b) = (b"first frame ".repeat(20), b"second frame".repeat(30));
        let whole = [a.clone(), b.clone()].concat();
        for (name, compress, decompress) in codecs().into_iter().filter(|c| c.0 == "lz4" || c.0 == "zstd") {
            let (ca, cb) = (compress(&a), compress(&b));
            let two = [ca.clone(), cb.clone()].concat();
            assert_eq!(decompress(&two, 1 << 20).as_ref(), Some(&whole), "{name}: two frames");
            let skipping = [skip_frame(5), ca.clone(), skip_frame(0), cb.clone()].concat();
            assert_eq!(decompress(&skipping, 1 << 20).as_ref(), Some(&whole), "{name}: skippable frames");
            for tail in [&[0u8, 0, 0, 0][..], &[0x04, 0x22, 0x4D, 0x18][..], &[1, 2, 3, 4, 5, 6, 7][..], &cb[..cb.len() - 1]] {
                let trailing = [ca.clone(), tail.to_vec()].concat();
                assert_eq!(decompress(&trailing, 1 << 20), None, "{name}: trailing {tail:02x?}");
            }
            assert_eq!(decompress(&skip_frame(3)[..10], 1 << 20), None, "{name}: a cut skippable frame");
        }
        // The LZ4 legacy format is a different format.
        let legacy = [0x184C_2102u32.to_le_bytes().to_vec(), vec![0; 8]].concat();
        assert_eq!(lz4_decompress(&legacy, 1 << 20), None);
    }
}
