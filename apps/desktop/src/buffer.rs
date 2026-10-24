use std::collections::VecDeque;

use egui_plot::PlotPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplePoint {
    pub time_seconds: f64,
    pub value: f64,
}

/// UI-owned plot history. It is bounded by both age and point count, so a
/// stalled renderer cannot grow memory without limit.
#[derive(Debug, Clone)]
pub struct TimeSeriesBuffer {
    points: VecDeque<SamplePoint>,
    sample_offset: u64,
    capacity: usize,
    window_seconds: f64,
    display_cache: Option<DisplayEnvelopeCache>,
    viewport: ViewportCache,
}

#[derive(Debug, Clone)]
struct EnvelopeBucket {
    start_seconds: f64,
    min: SamplePoint,
    max: SamplePoint,
}

#[derive(Debug, Clone)]
struct DisplayEnvelopeCache {
    #[cfg(test)]
    max_points: usize,
    bucket_width_seconds: f64,
    buckets: VecDeque<EnvelopeBucket>,
    #[cfg(test)]
    rendered: Vec<[f64; 2]>,
    dirty: bool,
    #[cfg(test)]
    rebuilds: usize,
}

#[derive(Debug, Clone, Default)]
struct ViewportCache {
    key: Option<ViewportKey>,
    candidates: Vec<[f64; 2]>,
    points: Vec<PlotPoint>,
    value_range: Option<(f64, f64)>,
    #[cfg(test)]
    rebuilds: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewportKey {
    range: [u64; 2],
    first: Option<u64>,
    last: Option<u64>,
    sample_ids: Option<[u64; 2]>,
    count: usize,
    budget: usize,
    normalized: bool,
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
            // Reserve the explicit history bound. Grow-before-trim would otherwise
            // round the 120100 sample bound up to 131072 samples per channel.
            points: VecDeque::with_capacity(capacity),
            sample_offset: 0,
            capacity,
            window_seconds,
            display_cache,
            viewport: ViewportCache::default(),
        }
    }

    #[must_use]
    pub fn with_history(
        capacity: usize,
        history_seconds: f64,
        display_seconds: f64,
        display_points: usize,
    ) -> Self {
        let mut buffer = Self::with_display_cache(capacity, history_seconds, display_points);
        buffer.display_cache = Some(DisplayEnvelopeCache::new(display_points, display_seconds));
        buffer
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
                    self.sample_offset += self.points.len() as u64;
                    self.points.clear();
                    self.viewport.key = None;
                    if let Some(cache) = &mut self.display_cache {
                        cache.clear();
                    }
                }
                if self.points.len() == self.capacity {
                    self.pop_front();
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
            self.pop_front();
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
            self.pop_front();
        }
        if let Some(cache) = &mut self.display_cache {
            cache.trim_to_raw_front(&self.points);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    fn pop_front(&mut self) {
        if self.points.pop_front().is_some() {
            self.sample_offset += 1;
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn clear(&mut self) {
        self.sample_offset += self.points.len() as u64;
        self.points.clear();
        self.viewport.key = None;
        self.viewport.points.clear();
        if let Some(cache) = &mut self.display_cache {
            cache.clear();
        }
    }

    #[must_use]
    pub fn latest_time(&self) -> Option<f64> {
        self.points.back().map(|point| point.time_seconds)
    }

    fn lower_bound(&self, time: f64, inclusive: bool) -> usize {
        self.points.partition_point(|point| {
            point.time_seconds < time || (inclusive && point.time_seconds <= time)
        })
    }

    /// Reuses the viewport on unchanged repaints and when incoming data is
    /// outside a manual view. Only visible raw samples or the incremental
    /// extrema summaries are inspected; the full 60 s history is not copied.
    pub fn prepare_visible(&mut self, range: [f64; 2], budget: usize, normalized: bool) {
        let start = self.lower_bound(range[0], false);
        let end = self.lower_bound(range[1], true);
        let key = ViewportKey {
            range: range.map(f64::to_bits),
            first: self
                .points
                .get(start)
                .filter(|_| start < end)
                .map(|p| p.time_seconds.to_bits()),
            last: end
                .checked_sub(1)
                .filter(|_| start < end)
                .and_then(|i| self.points.get(i))
                .map(|p| p.time_seconds.to_bits()),
            count: end - start,
            sample_ids: (start < end).then_some([
                self.sample_offset + start as u64,
                self.sample_offset + end as u64,
            ]),
            budget,
            normalized,
        };
        if self.viewport.key == Some(key) {
            return;
        }
        let cache = &mut self.viewport;
        cache.key = Some(key);
        cache.candidates.clear();
        cache.points.clear();
        cache.value_range = None;
        #[cfg(test)]
        {
            cache.rebuilds += 1;
        }
        if start == end || budget < 4 {
            return;
        }
        let mut add = |point: SamplePoint| {
            if cache
                .candidates
                .last()
                .is_none_or(|last| last[0].to_bits() != point.time_seconds.to_bits())
            {
                cache.candidates.push([point.time_seconds, point.value]);
            }
        };
        // At high zoom use every visible sample. At broad zoom the existing
        // append-only summaries retain extrema without revisiting old samples.
        if end - start <= budget * 2 || self.display_cache.is_none() {
            for point in self.points.range(start..end) {
                add(*point);
            }
        } else if let Some(envelope) = &self.display_cache {
            add(self.points[start]);
            let bucket_start = envelope.buckets.partition_point(|bucket| {
                bucket.start_seconds + envelope.bucket_width_seconds < range[0]
            });
            let bucket_end = envelope
                .buckets
                .partition_point(|bucket| bucket.start_seconds <= range[1]);
            for bucket in envelope.buckets.range(bucket_start..bucket_end) {
                let bucket_end = bucket.start_seconds + envelope.bucket_width_seconds;
                if bucket_end < range[0] || bucket.start_seconds > range[1] {
                    continue;
                }
                if bucket.start_seconds >= range[0] && bucket_end <= range[1] {
                    let ordered = if bucket.min.time_seconds <= bucket.max.time_seconds {
                        [bucket.min, bucket.max]
                    } else {
                        [bucket.max, bucket.min]
                    };
                    for point in ordered {
                        add(point);
                    }
                } else {
                    // Only the two clipped boundary buckets need raw samples.
                    let raw_start = self
                        .points
                        .partition_point(|p| p.time_seconds < bucket.start_seconds)
                        .max(start);
                    let raw_end = self
                        .points
                        .partition_point(|p| p.time_seconds < bucket_end)
                        .min(end);
                    for point in self.points.range(raw_start..raw_end) {
                        add(*point);
                    }
                }
            }
            add(self.points[end - 1]);
        }
        cache.render_candidates(budget, normalized);
    }

    #[must_use]
    pub fn visible_points(&self) -> &[PlotPoint] {
        &self.viewport.points
    }

    #[must_use]
    pub fn visible_value_range(&self) -> Option<(f64, f64)> {
        self.viewport.value_range
    }

    #[must_use]
    pub fn nearest_value(&self, time: f64) -> Option<f64> {
        let index = self.lower_bound(time, false);
        match (
            index.checked_sub(1).and_then(|i| self.points.get(i)),
            self.points.get(index),
        ) {
            (Some(before), Some(after)) => {
                Some(if time - before.time_seconds <= after.time_seconds - time {
                    before.value
                } else {
                    after.value
                })
            }
            (Some(point), None) | (None, Some(point)) => Some(point.value),
            (None, None) => None,
        }
    }

    /// Returns a bounded, ordered min/max envelope. First and last points are
    /// retained and spikes survive better than with simple stride sampling.
    #[must_use]
    #[cfg(test)]
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

impl ViewportCache {
    fn render_candidates(&mut self, budget: usize, normalized: bool) {
        let candidates = &self.candidates;
        let output = &mut self.points;
        if candidates.len() <= budget {
            output.extend(candidates.iter().copied().map(PlotPoint::from));
        } else {
            output.push(candidates[0].into());
            let interior = candidates.len() - 2;
            let bins = (budget - 2) / 2;
            for bin in 0..bins {
                let begin = 1 + bin * interior / bins;
                let end = 1 + (bin + 1) * interior / bins;
                let mut min = begin;
                let mut max = begin;
                for index in begin + 1..end {
                    if candidates[index][1] < candidates[min][1] {
                        min = index;
                    }
                    if candidates[index][1] > candidates[max][1] {
                        max = index;
                    }
                }
                for index in [min.min(max), min.max(max)] {
                    if output
                        .last()
                        .is_none_or(|last| last.x.to_bits() != candidates[index][0].to_bits())
                    {
                        output.push(candidates[index].into());
                    }
                }
            }
            output.push(
                candidates
                    .last()
                    .copied()
                    .expect("visible samples exist")
                    .into(),
            );
        }
        if let Some(first) = output.first() {
            let (min, max) = output.iter().fold((first.y, first.y), |(min, max), p| {
                (min.min(p.y), max.max(p.y))
            });
            self.value_range = Some((min, max));
            if normalized {
                let span = max - min;
                for point in output {
                    point.y = if span > f64::EPSILON {
                        (point.y - min) / span
                    } else {
                        0.5
                    };
                }
            }
        }
    }
}

impl DisplayEnvelopeCache {
    fn new(max_points: usize, window_seconds: f64) -> Self {
        let bucket_count = (max_points / 2).saturating_sub(2).max(1);
        let bucket_count = u32::try_from(bucket_count).expect("display bucket count fits in u32");
        let bucket_capacity = usize::try_from(bucket_count).expect("u32 fits in usize");
        Self {
            #[cfg(test)]
            max_points,
            bucket_width_seconds: window_seconds / f64::from(bucket_count),
            buckets: VecDeque::with_capacity(bucket_capacity + 2),
            #[cfg(test)]
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

    #[cfg(test)]
    fn render<'a>(&'a mut self, raw: &VecDeque<SamplePoint>) -> &'a [[f64; 2]] {
        if !self.dirty {
            return &self.rendered;
        }
        #[cfg(test)]
        {
            self.rebuilds += 1;
        }
        #[cfg(test)]
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
        #[cfg(test)]
        self.rendered.clear();
        self.dirty = true;
    }
}

#[cfg(test)]
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
    fn history_follows_capacity_age_clear_and_clock_restart() {
        let mut buffer = TimeSeriesBuffer::new(3, 2.0);
        buffer.extend([point(1.0, 1.0), point(1.1, 2.0), point(1.2, 3.0)]);
        assert_eq!(buffer.len(), 3);
        buffer.extend([point(1.3, 4.0)]);
        assert_eq!(buffer.decimated(10)[0], [1.1, 2.0]);
        buffer.extend([point(1.4, 1.0), point(3.5, 5.0)]);
        assert_eq!(buffer.len(), 1);
        buffer.extend([point(3.6, 1.0)]);
        assert_eq!(buffer.len(), 2);
        buffer.extend([point(0.0, 0.0)]);
        assert_eq!(buffer.len(), 1);
        buffer.extend([point(0.1, 1.0)]);
        buffer.clear();
        assert!(buffer.is_empty());
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
        assert_eq!(buffer.decimated(0), [] as [[f64; 2]; 0]);
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

    #[test]
    fn viewport_reuses_borrowed_storage_and_ignores_append_outside_manual_range() {
        let mut buffer = TimeSeriesBuffer::with_history(10_000, 60.0, 10.0, 2_000);
        buffer.extend((0..5_000).map(|i| point(f64::from(i) / 1_000.0, f64::from(i))));
        buffer.prepare_visible([1.0, 2.0], 400, false);
        let pointer = buffer.visible_points().as_ptr();
        let rebuilds = buffer.viewport.rebuilds;
        buffer.prepare_visible([1.0, 2.0], 400, false);
        assert_eq!(buffer.visible_points().as_ptr(), pointer);
        assert_eq!(buffer.viewport.rebuilds, rebuilds);
        buffer.extend((5_000..6_000).map(|i| point(f64::from(i) / 1_000.0, f64::from(i))));
        buffer.prepare_visible([1.0, 2.0], 400, false);
        assert_eq!(
            buffer.viewport.rebuilds, rebuilds,
            "unseen new history must not rebuild a manual viewport"
        );
        buffer.prepare_visible([1.0, 2.0], 200, false);
        assert!(buffer.visible_points().len() <= 200);
        assert_eq!(buffer.viewport.rebuilds, rebuilds + 1);
    }

    #[test]
    fn viewport_keeps_extrema_raw_hover_values_and_visible_window() {
        let mut buffer = TimeSeriesBuffer::with_history(200_000, 60.0, 10.0, 2_000);
        buffer.extend((0..100_000).map(|i| SamplePoint {
            time_seconds: f64::from(i) * f64::from(langmuir_core::SAMPLE_INTERVAL_MICROS)
                / 1_000_000.0,
            value: if i == 77_777 {
                90_000.0
            } else if i == 77_778 {
                -80_000.0
            } else {
                1.0
            },
        }));
        buffer.prepare_visible([30.0, 40.0], 600, false);
        assert!(buffer.visible_points().len() <= 600);
        assert!(
            buffer
                .visible_points()
                .iter()
                .any(|p| p.y.to_bits() == 90_000.0_f64.to_bits())
        );
        assert!(
            buffer
                .visible_points()
                .iter()
                .any(|p| p.y.to_bits() == (-80_000.0_f64).to_bits())
        );
        assert_eq!(
            buffer.nearest_value(
                77_777.0 * f64::from(langmuir_core::SAMPLE_INTERVAL_MICROS) / 1_000_000.0
            ),
            Some(90_000.0)
        );
        buffer.prepare_visible([0.0, 2.0], 128, false);
        buffer.prepare_visible([38.88, 38.90], 600, true);
        assert!(
            buffer.visible_points().len() < 100,
            "zooming in recovers the original raw samples"
        );
        assert_eq!(buffer.visible_value_range(), Some((-80_000.0, 90_000.0)));
        assert!(
            buffer
                .visible_points()
                .iter()
                .all(|p| (0.0..=1.0).contains(&p.y))
        );
    }

    #[test]
    fn sixty_second_history_and_frozen_copy_have_explicit_memory_bounds() {
        let capacity = 60 * 1_000_000 / langmuir_core::SAMPLE_INTERVAL_MICROS as usize
            + langmuir_core::SAMPLE_COUNT;
        let mut buffer = TimeSeriesBuffer::with_history(capacity, 60.0, 10.0, 2_000);
        buffer.extend((0..240_000).map(|i| {
            point(
                f64::from(i) * f64::from(langmuir_core::SAMPLE_INTERVAL_MICROS) / 1_000_000.0,
                f64::from(i),
            )
        }));
        assert!((119_999..=120_001).contains(&buffer.len()));
        assert_eq!(buffer.points.capacity(), 120_100);
        assert_eq!(std::mem::size_of::<SamplePoint>(), 16);
        let snapshot = buffer.clone();
        let last = snapshot.latest_time();
        buffer.extend([point(200.0, 10.0)]);
        assert_eq!(snapshot.latest_time(), last);
        assert!(snapshot.len() > buffer.len());
        buffer.prepare_visible([0.0, 10.0], 128, false);
        assert!(
            buffer.visible_points().is_empty(),
            "expired history is not fabricated"
        );
    }

    #[test]
    fn viewport_invalidates_same_timestamp_replacements_and_clock_epoch_reset() {
        let mut buffer = TimeSeriesBuffer::with_history(3, 60.0, 10.0, 2_000);
        buffer.extend([point(1.0, 10.0), point(1.0, 20.0), point(1.0, 30.0)]);
        buffer.prepare_visible([0.0, 2.0], 128, false);
        assert_eq!(buffer.visible_points()[0].y.to_bits(), 10.0_f64.to_bits());
        let rebuilds = buffer.viewport.rebuilds;
        buffer.extend([point(1.0, 40.0)]);
        buffer.prepare_visible([0.0, 2.0], 128, false);
        assert_eq!(
            buffer.viewport.rebuilds,
            rebuilds + 1,
            "first/last timestamps and count alone are insufficient after capacity eviction"
        );
        assert_eq!(buffer.visible_points()[0].y.to_bits(), 20.0_f64.to_bits());
        buffer.clear();
        buffer.extend([point(0.0, 1.0), point(1.0, 2.0)]);
        buffer.prepare_visible([0.0, 2.0], 128, false);
        let rebuilds = buffer.viewport.rebuilds;
        buffer.extend([point(0.0, 10.0), point(1.0, 20.0)]);
        buffer.prepare_visible([0.0, 2.0], 128, false);
        assert_eq!(buffer.viewport.rebuilds, rebuilds + 1);
        assert_eq!(buffer.visible_points()[0].y.to_bits(), 10.0_f64.to_bits());
    }
}
