use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplePoint {
    pub time_seconds: f64,
    pub value: f64,
}

/// UI-owned plot history. It is bounded by both age and point count, so a
/// stalled renderer cannot grow memory without limit.
#[derive(Debug)]
pub struct TimeSeriesBuffer {
    points: VecDeque<SamplePoint>,
    capacity: usize,
    window_seconds: f64,
    display_cache: Option<DisplayEnvelopeCache>,
}

#[derive(Debug)]
struct EnvelopeBucket {
    start_seconds: f64,
    min: SamplePoint,
    max: SamplePoint,
}

#[derive(Debug)]
struct DisplayEnvelopeCache {
    max_points: usize,
    bucket_width_seconds: f64,
    buckets: VecDeque<EnvelopeBucket>,
    rendered: Vec<[f64; 2]>,
    dirty: bool,
    #[cfg(test)]
    rebuilds: usize,
}

impl TimeSeriesBuffer {
    #[cfg(test)]
    #[must_use]
    pub fn new(capacity: usize, window_seconds: f64) -> Self {
        Self::with_display_cache(capacity, window_seconds, 0)
    }

    /// Creates a raw bounded history plus an incremental min/max display
    /// envelope. The envelope contains only `O(display_points)` state, so a UI
    /// repaint never scans the full raw acquisition history.
    #[must_use]
    pub fn with_display_cache(capacity: usize, window_seconds: f64, display_points: usize) -> Self {
        assert!(capacity > 0, "plot capacity must be non-zero");
        assert!(window_seconds.is_finite() && window_seconds > 0.0);
        let display_cache = (display_points >= 4)
            .then(|| DisplayEnvelopeCache::new(display_points.min(capacity), window_seconds));
        Self {
            points: VecDeque::with_capacity(capacity.min(16_384)),
            capacity,
            window_seconds,
            display_cache,
        }
    }

    pub fn extend<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = SamplePoint>,
    {
        for point in points {
            if point.time_seconds.is_finite() && point.value.is_finite() {
                // Device clocks may restart after a reconnect. Mixing the old
                // and new epochs would make the plot non-monotonic and would
                // also defeat age-based trimming, so begin a fresh window.
                if self
                    .points
                    .back()
                    .is_some_and(|last| point.time_seconds < last.time_seconds)
                {
                    self.points.clear();
                    if let Some(cache) = &mut self.display_cache {
                        cache.clear();
                    }
                }
                self.points.push_back(point);
                if let Some(cache) = &mut self.display_cache {
                    cache.push(point);
                }
            }
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.points.len() > self.capacity {
            self.points.pop_front();
        }

        let Some(latest) = self.points.back().map(|point| point.time_seconds) else {
            return;
        };
        let cutoff = latest - self.window_seconds;
        while self
            .points
            .front()
            .is_some_and(|point| point.time_seconds < cutoff)
        {
            self.points.pop_front();
        }
        if let Some(cache) = &mut self.display_cache {
            cache.trim_to_raw_front(&self.points);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn clear(&mut self) {
        self.points.clear();
        if let Some(cache) = &mut self.display_cache {
            cache.clear();
        }
    }

    /// Returns a bounded, ordered min/max envelope. First and last points are
    /// retained and spikes survive better than with simple stride sampling.
    #[must_use]
    pub fn decimated(&mut self, max_points: usize) -> Vec<[f64; 2]> {
        if self
            .display_cache
            .as_ref()
            .is_some_and(|cache| cache.max_points == max_points)
        {
            let cache = self.display_cache.as_mut().expect("cache was checked");
            return cache.render(&self.points).to_vec();
        }
        decimate_points(&self.points, max_points)
    }
}

impl DisplayEnvelopeCache {
    fn new(max_points: usize, window_seconds: f64) -> Self {
        let bucket_count = (max_points / 2).saturating_sub(2).max(1);
        let bucket_count = u32::try_from(bucket_count).expect("display bucket count fits in u32");
        let bucket_capacity = usize::try_from(bucket_count).expect("u32 fits in usize");
        Self {
            max_points,
            bucket_width_seconds: window_seconds / f64::from(bucket_count),
            buckets: VecDeque::with_capacity(bucket_capacity + 2),
            rendered: Vec::with_capacity(max_points),
            dirty: true,
            #[cfg(test)]
            rebuilds: 0,
        }
    }

    fn bucket_start(&self, time_seconds: f64) -> f64 {
        (time_seconds / self.bucket_width_seconds).floor() * self.bucket_width_seconds
    }

    fn push(&mut self, point: SamplePoint) {
        let start_seconds = self.bucket_start(point.time_seconds);
        if let Some(bucket) = self.buckets.back_mut()
            && bucket.start_seconds.to_bits() == start_seconds.to_bits()
        {
            if point.value < bucket.min.value {
                bucket.min = point;
            }
            if point.value > bucket.max.value {
                bucket.max = point;
            }
        } else {
            self.buckets.push_back(EnvelopeBucket {
                start_seconds,
                min: point,
                max: point,
            });
        }
        self.dirty = true;
    }

    fn trim_to_raw_front(&mut self, raw: &VecDeque<SamplePoint>) {
        let Some(front) = raw.front().copied() else {
            self.clear();
            return;
        };
        let front_start = self.bucket_start(front.time_seconds);
        while self
            .buckets
            .front()
            .is_some_and(|bucket| bucket.start_seconds < front_start)
        {
            self.buckets.pop_front();
            self.dirty = true;
        }

        // Window trimming can cut through the oldest bucket. Rebuild only that
        // small bucket so an expired spike cannot remain on screen.
        let mut points = raw.iter().copied().take_while(|point| {
            self.bucket_start(point.time_seconds).to_bits() == front_start.to_bits()
        });
        if let Some(first) = points.next() {
            let mut min = first;
            let mut max = first;
            for point in points {
                if point.value < min.value {
                    min = point;
                }
                if point.value > max.value {
                    max = point;
                }
            }
            if let Some(bucket) = self.buckets.front_mut()
                && bucket.start_seconds.to_bits() == front_start.to_bits()
                && (bucket.min != min || bucket.max != max)
            {
                bucket.min = min;
                bucket.max = max;
                self.dirty = true;
            }
        }
    }

    fn render<'a>(&'a mut self, raw: &VecDeque<SamplePoint>) -> &'a [[f64; 2]] {
        if !self.dirty {
            return &self.rendered;
        }
        #[cfg(test)]
        {
            self.rebuilds += 1;
        }
        self.rendered.clear();
        let Some(first) = raw.front() else {
            self.dirty = false;
            return &self.rendered;
        };
        let last = raw.back().expect("a non-empty deque has a last point");
        let mut candidates = VecDeque::with_capacity(self.buckets.len() * 2 + 2);
        candidates.push_back(*first);
        for bucket in &self.buckets {
            let ordered = if bucket.min.time_seconds <= bucket.max.time_seconds {
                [bucket.min, bucket.max]
            } else {
                [bucket.max, bucket.min]
            };
            for point in ordered {
                if candidates.back().is_none_or(|previous| {
                    previous.time_seconds.to_bits() != point.time_seconds.to_bits()
                }) {
                    candidates.push_back(point);
                }
            }
        }
        if candidates
            .back()
            .is_none_or(|previous| previous.time_seconds.to_bits() != last.time_seconds.to_bits())
        {
            candidates.push_back(*last);
        }
        self.rendered = decimate_points(&candidates, self.max_points);
        self.dirty = false;
        &self.rendered
    }

    fn clear(&mut self) {
        self.buckets.clear();
        self.rendered.clear();
        self.dirty = true;
    }
}

fn decimate_points(points: &VecDeque<SamplePoint>, max_points: usize) -> Vec<[f64; 2]> {
    if max_points == 0 || points.is_empty() {
        return Vec::new();
    }
    if points.len() <= max_points {
        return points
            .iter()
            .map(|point| [point.time_seconds, point.value])
            .collect();
    }
    if max_points == 1 {
        let point = points.back().expect("buffer is not empty");
        return vec![[point.time_seconds, point.value]];
    }
    if max_points == 2 {
        let first = points.front().expect("buffer is not empty");
        let last = points.back().expect("buffer is not empty");
        return vec![
            [first.time_seconds, first.value],
            [last.time_seconds, last.value],
        ];
    }

    let interior = points.len() - 2;
    let bucket_count = ((max_points - 2) / 2).max(1);
    let bucket_width = interior.div_ceil(bucket_count);
    let mut result = Vec::with_capacity(max_points);
    let first = points.front().expect("buffer is not empty");
    result.push([first.time_seconds, first.value]);

    for bucket_start in (1..points.len() - 1).step_by(bucket_width) {
        if result.len() >= max_points - 1 {
            break;
        }
        let bucket_end = (bucket_start + bucket_width).min(points.len() - 1);
        let first_in_bucket = *points
            .get(bucket_start)
            .expect("bucket starts inside the buffer");
        let mut min = first_in_bucket;
        let mut max = first_in_bucket;
        for index in bucket_start + 1..bucket_end {
            let point = *points.get(index).expect("bucket is inside the buffer");
            if point.value < min.value {
                min = point;
            }
            if point.value > max.value {
                max = point;
            }
        }
        let ordered = if min.time_seconds <= max.time_seconds {
            [min, max]
        } else {
            [max, min]
        };
        for point in ordered {
            if result.len() < max_points - 1
                && result
                    .last()
                    .is_none_or(|last| last[0].to_bits() != point.time_seconds.to_bits())
            {
                result.push([point.time_seconds, point.value]);
            }
        }
    }

    let last = points.back().expect("buffer is not empty");
    result.push([last.time_seconds, last.value]);
    result
}

#[cfg(test)]
mod tests {
    use super::{SamplePoint, TimeSeriesBuffer};

    fn point(time_seconds: f64, value: f64) -> SamplePoint {
        SamplePoint {
            time_seconds,
            value,
        }
    }

    #[test]
    fn ring_buffer_is_bounded_by_capacity() {
        let mut buffer = TimeSeriesBuffer::new(3, 100.0);
        buffer.extend((0..5).map(|value| point(f64::from(value), f64::from(value))));
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.decimated(10)[0][0].to_bits(), 2.0_f64.to_bits());
        assert_eq!(buffer.decimated(10)[0][1].to_bits(), 2.0_f64.to_bits());
    }

    #[test]
    fn ring_buffer_discards_points_outside_time_window() {
        let mut buffer = TimeSeriesBuffer::new(100, 2.0);
        buffer.extend([point(1.0, 1.0), point(2.9, 2.0), point(3.0, 3.0)]);
        assert_eq!(
            buffer.decimated(10),
            vec![[1.0, 1.0], [2.9, 2.0], [3.0, 3.0]]
        );
        buffer.extend([point(3.1, 4.0)]);
        assert_eq!(
            buffer.decimated(10),
            vec![[2.9, 2.0], [3.0, 3.0], [3.1, 4.0]]
        );
    }

    #[test]
    fn decimation_is_bounded_ordered_and_retains_spike() {
        let mut buffer = TimeSeriesBuffer::new(1_000, 1_000.0);
        buffer.extend((0..100).map(|index| {
            let value = if index == 50 {
                10_000.0
            } else {
                f64::from(index)
            };
            point(f64::from(index), value)
        }));
        let output = buffer.decimated(12);
        assert!(output.len() <= 12);
        assert_eq!(output.first(), Some(&[0.0, 0.0]));
        assert_eq!(output.last(), Some(&[99.0, 99.0]));
        assert!(output.windows(2).all(|pair| pair[0][0] <= pair[1][0]));
        assert!(
            output
                .iter()
                .any(|point| point[1].to_bits() == 10_000.0_f64.to_bits())
        );
    }

    #[test]
    fn zero_budget_returns_no_points() {
        let mut buffer = TimeSeriesBuffer::new(10, 10.0);
        buffer.extend([point(1.0, 2.0)]);
        assert!(buffer.decimated(0).is_empty());
    }

    #[test]
    fn backwards_device_clock_starts_a_fresh_window() {
        let mut buffer = TimeSeriesBuffer::new(10, 10.0);
        buffer.extend([point(100.0, 1.0), point(101.0, 2.0)]);
        buffer.extend([point(1.0, 3.0), point(2.0, 4.0)]);
        assert_eq!(buffer.decimated(10), vec![[1.0, 3.0], [2.0, 4.0]]);
    }

    #[test]
    fn clear_removes_all_points() {
        let mut buffer = TimeSeriesBuffer::new(10, 10.0);
        buffer.extend([point(1.0, 2.0)]);
        buffer.clear();
        assert!(buffer.is_empty());
    }

    #[test]
    fn incremental_display_cache_is_bounded_retains_spikes_and_reuses_render() {
        let mut buffer = TimeSeriesBuffer::with_display_cache(20_000, 10.0, 200);
        buffer.extend((0..10_000).map(|index| {
            point(
                f64::from(index) / 1_000.0,
                if index == 5_555 { 1_000_000.0 } else { 1.0 },
            )
        }));

        let first = buffer.decimated(200);
        assert!(first.len() <= 200);
        assert!(
            first
                .iter()
                .any(|point| point[1].to_bits() == 1_000_000.0_f64.to_bits())
        );
        let rebuilds = buffer
            .display_cache
            .as_ref()
            .expect("display cache is configured")
            .rebuilds;

        let second = buffer.decimated(200);
        assert_eq!(second, first);
        assert_eq!(
            buffer
                .display_cache
                .as_ref()
                .expect("display cache is configured")
                .rebuilds,
            rebuilds,
            "a repaint without new data must reuse the envelope",
        );
    }
}
