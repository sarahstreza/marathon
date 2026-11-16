//! The HLS playlist for a transcoded item (PLAN.md M13).
//!
//! A transcode is served as an HLS VOD playlist that lists *every* segment of
//! the item, from the first to the last, before any of them exists. The
//! player therefore sees the item's real timeline and duration, and can
//! start or seek anywhere: a segment that has not been encoded yet is
//! encoded when it is asked for (see [`crate::transcode`]). That works
//! because segments are a fixed length, so which segment covers a given
//! moment never depends on what has been encoded.
//!
//! Only pure arithmetic and text live here.

use std::fmt::Write;

/// How many segments of `segment_seconds` cover `duration_ms`: always at
/// least one, and the last may be shorter than the rest.
pub(crate) fn segment_count(duration_ms: u64, segment_seconds: u32) -> u32 {
    let segment_ms = u64::from(segment_seconds) * 1000;
    let count = duration_ms.div_ceil(segment_ms).max(1);
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The VOD playlist for an item `duration_ms` long, whose segments are
/// requested as `{n}.ts?s={session}` relative to the playlist's own URL.
pub(crate) fn playlist(duration_ms: u64, segment_seconds: u32, session: &str) -> String {
    let segment_ms = u64::from(segment_seconds) * 1000;
    let count = segment_count(duration_ms, segment_seconds);
    let mut out = String::new();
    // Writing to a String cannot fail.
    let _ = write!(
        out,
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-TARGETDURATION:{segment_seconds}\n\
         #EXT-X-MEDIA-SEQUENCE:0\n"
    );
    for n in 0..count {
        let start = u64::from(n) * segment_ms;
        let length_ms = duration_ms.saturating_sub(start).min(segment_ms);
        let _ = write!(
            out,
            "#EXTINF:{}.{:03},\n{n}.ts?s={session}\n",
            length_ms / 1000,
            length_ms % 1000
        );
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_segments_rounding_up_and_never_zero() {
        assert_eq!(segment_count(7_000, 2), 4);
        assert_eq!(segment_count(6_000, 2), 3);
        assert_eq!(segment_count(6_001, 2), 4);
        assert_eq!(segment_count(100, 6), 1);
        assert_eq!(segment_count(0, 6), 1);
        assert_eq!(segment_count(1_380_000, 6), 230);
    }

    #[test]
    fn lists_every_segment_with_the_last_one_short() {
        let text = playlist(7_000, 2, "abc");
        assert_eq!(
            text,
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n\
             #EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-TARGETDURATION:2\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:2.000,\n0.ts?s=abc\n\
             #EXTINF:2.000,\n1.ts?s=abc\n\
             #EXTINF:2.000,\n2.ts?s=abc\n\
             #EXTINF:1.000,\n3.ts?s=abc\n\
             #EXT-X-ENDLIST\n"
        );
    }

    #[test]
    fn the_durations_add_up_to_the_item() {
        let (duration_ms, seconds) = (1_234_567_u64, 6);
        let text = playlist(duration_ms, seconds, "s");
        let total: u64 = text
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .map(|l| {
                let secs: f64 = l.trim_end_matches(',').parse().unwrap();
                (secs * 1000.0).round() as u64
            })
            .sum();
        assert_eq!(total, duration_ms);
        assert_eq!(
            text.matches(".ts?s=s").count() as u32,
            segment_count(duration_ms, seconds)
        );
    }

    #[test]
    fn an_exact_multiple_has_no_empty_tail() {
        let text = playlist(12_000, 6, "x");
        assert_eq!(text.matches("#EXTINF:6.000,").count(), 2);
        assert!(!text.contains("#EXTINF:0.000"));
    }

    #[test]
    fn a_very_short_item_is_one_short_segment() {
        let text = playlist(250, 6, "x");
        assert!(text.contains("#EXTINF:0.250,\n0.ts?s=x\n"));
        assert!(text.ends_with("#EXT-X-ENDLIST\n"));
    }
}
