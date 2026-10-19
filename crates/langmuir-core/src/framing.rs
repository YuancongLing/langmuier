use crate::{
    FRAME_HEADER, FrameError, LVDS_FRAME_LEN, RS422_FRAME_LEN, Rs422Telemetry, ScientificFrame,
    decode_rs422_telemetry, decode_scientific_frame,
};

/// Counters maintained by each stream decoder.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamMetrics {
    pub bytes_received: u64,
    pub bytes_discarded: u64,
    pub candidate_frames: u64,
    pub frames_decoded: u64,
    pub frames_rejected: u64,
}

/// Owning convenience result from [`Rs422StreamDecoder::feed`] or
/// [`LvdsStreamDecoder::feed`]. For allocation-sensitive paths, use
/// `push_with` and process each item through callbacks.
#[derive(Debug)]
pub struct DecodeBatch<T> {
    pub frames: Vec<T>,
    pub errors: Vec<FrameError>,
}

impl<T> Default for DecodeBatch<T> {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            errors: Vec::new(),
        }
    }
}

struct FixedFrameDecoder<T> {
    frame_len: usize,
    decode: fn(&[u8]) -> Result<T, FrameError>,
    buffer: Vec<u8>,
    start: usize,
    metrics: StreamMetrics,
}

impl<T> FixedFrameDecoder<T> {
    fn new(frame_len: usize, decode: fn(&[u8]) -> Result<T, FrameError>) -> Self {
        Self {
            frame_len,
            decode,
            buffer: Vec::with_capacity(frame_len * 2),
            start: 0,
            metrics: StreamMetrics::default(),
        }
    }

    fn push_with(
        &mut self,
        input: &[u8],
        mut on_frame: impl FnMut(T),
        mut on_error: impl FnMut(FrameError),
    ) {
        self.metrics.bytes_received = self
            .metrics
            .bytes_received
            .saturating_add(input.len() as u64);
        self.buffer.extend_from_slice(input);

        loop {
            let Some(header) = self.find_header() else {
                self.retain_possible_header_prefix();
                break;
            };

            self.discard_through(header);
            if self.buffer.len() - self.start < self.frame_len {
                break;
            }

            self.metrics.candidate_frames = self.metrics.candidate_frames.saturating_add(1);
            let candidate = &self.buffer[self.start..self.start + self.frame_len];
            match (self.decode)(candidate) {
                Ok(frame) => {
                    self.metrics.frames_decoded = self.metrics.frames_decoded.saturating_add(1);
                    self.start += self.frame_len;
                    on_frame(frame);
                }
                Err(error) => {
                    self.metrics.frames_rejected = self.metrics.frames_rejected.saturating_add(1);
                    // Only discard the first marker byte. A valid frame may begin
                    // anywhere inside this rejected fixed-length candidate.
                    self.discard_through(self.start + 1);
                    on_error(error);
                }
            }
        }

        self.compact_if_useful();
    }

    fn find_header(&self) -> Option<usize> {
        let data = &self.buffer[self.start..];
        data.windows(2)
            .position(|window| window == FRAME_HEADER)
            .map(|offset| self.start + offset)
    }

    fn retain_possible_header_prefix(&mut self) {
        let retain = usize::from(self.buffer.last() == Some(&FRAME_HEADER[0]));
        let new_start = self.buffer.len().saturating_sub(retain);
        self.discard_through(new_start);
    }

    fn discard_through(&mut self, new_start: usize) {
        debug_assert!(new_start >= self.start);
        let discarded = new_start - self.start;
        self.metrics.bytes_discarded = self
            .metrics
            .bytes_discarded
            .saturating_add(discarded as u64);
        self.start = new_start;
    }

    fn compact_if_useful(&mut self) {
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start > self.frame_len || self.start * 2 >= self.buffer.len() {
            self.buffer.copy_within(self.start.., 0);
            self.buffer.truncate(self.buffer.len() - self.start);
            self.start = 0;
        }
    }

    const fn metrics(&self) -> StreamMetrics {
        self.metrics
    }

    fn buffered_len(&self) -> usize {
        self.buffer.len() - self.start
    }

    fn clear(&mut self) {
        self.metrics.bytes_discarded = self
            .metrics
            .bytes_discarded
            .saturating_add(self.buffered_len() as u64);
        self.buffer.clear();
        self.start = 0;
    }
}

macro_rules! stream_decoder {
    ($name:ident, $output:ty, $frame_len:expr, $decoder:path) => {
        pub struct $name {
            inner: FixedFrameDecoder<$output>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self {
                    inner: FixedFrameDecoder::new($frame_len, $decoder),
                }
            }

            pub fn feed(&mut self, input: &[u8]) -> DecodeBatch<$output> {
                let mut batch = DecodeBatch::default();
                self.push_with(
                    input,
                    |frame| batch.frames.push(frame),
                    |error| batch.errors.push(error),
                );
                batch
            }

            pub fn push_with(
                &mut self,
                input: &[u8],
                on_frame: impl FnMut($output),
                on_error: impl FnMut(FrameError),
            ) {
                self.inner.push_with(input, on_frame, on_error);
            }

            #[must_use]
            pub const fn metrics(&self) -> StreamMetrics {
                self.inner.metrics()
            }

            #[must_use]
            pub fn buffered_len(&self) -> usize {
                self.inner.buffered_len()
            }

            /// Discards an incomplete frame without resetting cumulative metrics.
            pub fn clear_buffer(&mut self) {
                self.inner.clear();
            }
        }
    };
}

stream_decoder!(
    Rs422StreamDecoder,
    Rs422Telemetry,
    RS422_FRAME_LEN,
    decode_rs422_telemetry
);
stream_decoder!(
    LvdsStreamDecoder,
    ScientificFrame,
    LVDS_FRAME_LEN,
    decode_scientific_frame
);

#[cfg(test)]
mod tests {
    use super::{LvdsStreamDecoder, Rs422StreamDecoder};
    use crate::{
        FrameError, LVDS_FRAME_LEN, Protocol, RS422_FRAME_LEN,
        lvds::test_support::valid_frame as valid_lvds,
        rs422::test_support::valid_frame as valid_rs422,
    };

    #[test]
    fn rs422_accepts_every_chunk_size_and_multiple_frames() {
        let frame = valid_rs422();
        for chunk_size in 1..=RS422_FRAME_LEN * 2 {
            let mut input = vec![0x00, 0xEB, 0x01];
            input.extend_from_slice(&frame);
            input.extend_from_slice(&frame);
            input.extend_from_slice(&[0x55, 0xEB]);
            let mut decoder = Rs422StreamDecoder::new();
            let mut results = Vec::new();
            let mut errors = Vec::new();
            for chunk in input.chunks(chunk_size) {
                decoder.push_with(
                    chunk,
                    |value| results.push(value),
                    |error| errors.push(error),
                );
            }
            assert_eq!(results.len(), 2, "chunk size {chunk_size}");
            assert!(errors.is_empty(), "chunk size {chunk_size}");
            assert_eq!(decoder.buffered_len(), 1);
            assert_eq!(decoder.metrics().frames_decoded, 2);
        }
    }

    #[test]
    fn rs422_resynchronizes_after_corrupt_frame_and_noise() {
        let valid = valid_rs422();
        let mut corrupt = valid;
        corrupt[20] ^= 1;
        let mut input = vec![0x11, 0x22, 0xEB, 0x01];
        input.extend_from_slice(&corrupt);
        input.extend_from_slice(&[0x33, 0x44]);
        input.extend_from_slice(&valid);

        let mut decoder = Rs422StreamDecoder::new();
        let batch = decoder.feed(&input);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(decoder.metrics().candidate_frames, 2);
        assert_eq!(decoder.metrics().frames_rejected, 1);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn rs422_recovers_valid_header_embedded_in_bad_candidate() {
        let valid = valid_rs422();
        let mut input = vec![0xEB, 0x90, 0x00, 0x55, 0x66];
        input.extend_from_slice(&valid);

        let mut decoder = Rs422StreamDecoder::new();
        let batch = decoder.feed(&input);
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.frames.len(), 1);
    }

    #[test]
    fn split_header_is_retained_and_noise_does_not_grow_buffer() {
        let valid = valid_rs422();
        let mut decoder = Rs422StreamDecoder::new();
        let noise = vec![0x55; RS422_FRAME_LEN * 4];
        assert!(decoder.feed(&noise).frames.is_empty());
        assert_eq!(decoder.buffered_len(), 0);
        assert!(decoder.feed(&[0x01, 0xEB]).frames.is_empty());
        assert_eq!(decoder.buffered_len(), 1);
        let batch = decoder.feed(&valid[1..]);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn lvds_accepts_single_byte_chunks() {
        let frame = valid_lvds();
        let mut decoder = LvdsStreamDecoder::new();
        let mut count = 0;
        let mut error_count = 0;
        for byte in frame.as_slice() {
            decoder.push_with(
                std::slice::from_ref(byte),
                |_| count += 1,
                |_| error_count += 1,
            );
        }
        assert_eq!(count, 1);
        assert_eq!(error_count, 0);
        assert_eq!(decoder.metrics().bytes_received, LVDS_FRAME_LEN as u64);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn lvds_recovers_after_bad_candidate_containing_valid_frame() {
        let valid = valid_lvds();
        let mut input = vec![0xEB, 0x90, 0x00];
        input.extend_from_slice(valid.as_slice());
        let mut decoder = LvdsStreamDecoder::new();
        let batch = decoder.feed(&input);
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(decoder.metrics().candidate_frames, 2);
    }

    #[test]
    fn lvds_rejects_crc_failure_and_resynchronizes_to_next_valid_frame() {
        let valid = valid_lvds();
        let mut corrupt = valid.clone();
        corrupt[100] ^= 1;
        let mut input = corrupt.to_vec();
        input.extend_from_slice(&[0x11, 0x22, 0xEB]);
        input.extend_from_slice(valid.as_slice());
        for chunk_size in [1, 7, LVDS_FRAME_LEN, LVDS_FRAME_LEN * 2] {
            let mut decoder = LvdsStreamDecoder::new();
            let mut frames = Vec::new();
            let mut errors = Vec::new();
            for chunk in input.chunks(chunk_size) {
                decoder.push_with(
                    chunk,
                    |frame| frames.push(frame),
                    |error| errors.push(error),
                );
            }
            assert_eq!(frames.len(), 1, "chunk size {chunk_size}");
            assert_eq!(frames[0].channel_1[0], 2048);
            assert!(matches!(
                errors.as_slice(),
                [FrameError::ChecksumMismatch {
                    protocol: Protocol::Lvds,
                    ..
                }]
            ));
            assert_eq!(decoder.metrics().frames_rejected, 1);
            assert_eq!(decoder.metrics().frames_decoded, 1);
            assert_eq!(decoder.buffered_len(), 0);
        }
    }

    #[test]
    fn clear_buffer_accounts_for_discarded_partial_frame() {
        let frame = valid_rs422();
        let mut decoder = Rs422StreamDecoder::new();
        decoder.feed(&frame[..10]);
        decoder.clear_buffer();
        assert_eq!(decoder.buffered_len(), 0);
        assert_eq!(decoder.metrics().bytes_discarded, 10);
    }

    #[test]
    fn deterministic_noise_and_fragmentation_never_lose_embedded_frames() {
        let valid = valid_rs422();
        let mut seed = 0xA5A5_1234_u32;
        for round in 0..128 {
            let mut input = Vec::new();
            for _ in 0..round {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let byte = seed.to_be_bytes()[0];
                // Avoid accidentally manufacturing an additional frame marker.
                input.push(if byte == 0xEB { 0xEA } else { byte });
            }
            input.extend_from_slice(&valid);

            let mut decoder = Rs422StreamDecoder::new();
            let mut frame_count = 0;
            for chunk in input.chunks(round % 17 + 1) {
                decoder.push_with(chunk, |_| frame_count += 1, |_| {});
            }
            assert_eq!(frame_count, 1, "round {round}");
            assert!(decoder.buffered_len() <= 1);
        }
    }
}
