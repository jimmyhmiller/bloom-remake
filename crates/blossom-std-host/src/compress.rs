//! Host functions of `blossom_std::compress` (FOREIGN-PROTOCOLS §4): gzip, snappy (the raw block format), LZ4 (the
//! frame format) and Zstandard, each a compressor and a bounded decompressor.
//!
//! A decompressor takes the largest output it may produce and returns `None` past it, or on any malformed input; it
//! never allocates beyond that bound (plus the codec's own working state) and never panics. Compressors are
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

fn lz4_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    bounded(lz4_flex::frame::FrameDecoder::new(input), max)
}

fn zstd_compress(input: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(input, ruzstd::encoding::CompressionLevel::Fastest)
}

/// Every Zstandard frame of `input`, concatenated.
fn zstd_decompress(input: &[u8], max: u64) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    let mut src = input;
    let mut out = Vec::new();
    while !src.is_empty() {
        let before = src.len();
        let dec = ruzstd::decoding::StreamingDecoder::new(&mut src).ok()?;
        let room = max.checked_sub(out.len() as u64)?;
        dec.take(room.saturating_add(1)).read_to_end(&mut out).ok()?;
        if out.len() as u64 > max || src.len() >= before {
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
}
