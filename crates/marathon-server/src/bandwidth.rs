//! `GET /bandwidth?bytes=N` (PLAN.md M13): N bytes that cannot be
//! compressed or cached, for a client to time. The client works out its
//! throughput from how long the download took, and reports it to
//! `/clients/{id}/playback` and `/playback/profiles`.
//!
//! The bytes come from a small PRNG seeded from the clock. They only have
//! to defeat compression (by a proxy, say), not guess-proof: nothing here
//! is secret.

use axum::body::{Body, Bytes};
use futures_util::stream;

/// The most bytes one request may ask for.
pub(crate) const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Size of the chunks the body is sent in.
const CHUNK: u64 = 64 * 1024;

/// SplitMix64: a tiny generator whose output is statistically random
/// enough that gzip finds nothing in it.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

/// A body of exactly `bytes` noise bytes, produced as it is sent.
pub(crate) fn body(bytes: u64) -> Body {
    // Seeded from the clock; the nanoseconds are all the variety needed.
    // Truncating the 128-bit nanosecond count to 64 bits keeps the part
    // that changes.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let body = stream::unfold((bytes, Noise(seed)), |(remaining, mut noise)| async move {
        if remaining == 0 {
            return None;
        }
        // At most CHUNK, so it fits in usize everywhere.
        let len = remaining.min(CHUNK) as usize;
        let mut buf = vec![0; len];
        noise.fill(&mut buf);
        Some((
            Ok::<_, std::convert::Infallible>(Bytes::from(buf)),
            (remaining - len as u64, noise),
        ))
    });
    Body::from_stream(body)
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    async fn collect(bytes: u64) -> Vec<u8> {
        body(bytes).collect().await.unwrap().to_bytes().to_vec()
    }

    #[tokio::test]
    async fn sends_exactly_the_bytes_asked_for() {
        for n in [1, 7, 8, 9, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 5] {
            assert_eq!(collect(n).await.len() as u64, n, "{n}");
        }
        assert!(collect(0).await.is_empty());
    }

    #[tokio::test]
    async fn the_bytes_do_not_repeat_or_compress_into_nothing() {
        let data = collect(256 * 1024).await;
        // Every byte value shows up, and the stream is not a repeated block.
        let mut seen = [false; 256];
        for b in &data {
            seen[usize::from(*b)] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_ne!(data[..4096], data[4096..8192]);
        let chunk = CHUNK as usize;
        assert_ne!(data[..chunk], data[chunk..2 * chunk]);
    }
}
