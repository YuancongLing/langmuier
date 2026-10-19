use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Time to wait before marking an unobserved accepted sequence as expired.
/// This software policy does not establish that a UDP packet was lost.
pub const DEFAULT_UDP_SEQUENCE_GAP_WAIT: Duration = Duration::from_secs(2);
/// Additional time to retain expired candidates so late arrivals can resolve
/// them. The deadline is measured from first discovery, without renewal.
pub const DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY: Duration = Duration::from_secs(5);
/// Maximum combined waiting and expired sequence records in one session.
pub const DEFAULT_UDP_SEQUENCE_CAPACITY: usize = 256;
/// Largest forward or backward modular distance interpreted as local traffic.
/// Larger changes are ambiguous and re-anchor observation without guessing a
/// device reset or allocating a large range of sequence candidates.
pub const DEFAULT_UDP_SEQUENCE_MAX_ADVANCE: u32 = 256;

/// Accepted-science observations for one source endpoint and receiver session.
/// All event counters saturate. Only the two explicitly documented gauges can
/// decrease; no counter represents confirmed network loss.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UdpLvdsSequenceMetrics {
    /// Valid, dedup-accepted frames passed to this monitor by the caller.
    pub observed_frames: u64,
    /// Observations exactly one modular step beyond the current frontier.
    /// The initial anchor, late fills, and ambiguous changes are excluded.
    pub contiguous_frames: u64,
    /// Forward advances of 2..=256 whose candidate range fits the tracking
    /// bound, each counted once. Capacity re-anchors are anomalies instead.
    pub forward_jumps: u64,
    /// Newly tracked intermediate sequence candidates, not jump events.
    pub gap_candidates: u64,
    /// Gauge: tracked candidates still inside their waiting window.
    pub pending_gaps: u64,
    /// Cumulative candidates that reached the waiting deadline. Later fills do
    /// not subtract this event count, and it is not permanent missing science.
    pub expired_gaps: u64,
    /// Gauge: retained expired candidates not yet filled or discarded.
    pub unresolved_expired_gaps: u64,
    /// Observations resolving a tracked waiting or expired candidate.
    pub late_fills: u64,
    /// Subset of `late_fills` that resolved an already expired candidate.
    pub late_fills_after_expiry: u64,
    /// Local backward observations and all late fills. This overlaps
    /// `late_fills`, rather than being an additional disjoint event count.
    pub out_of_order_frames: u64,
    /// Bounded forward advances crossing from a larger `u32` value to a smaller
    /// one. A forward jump crossing zero is both a jump and a wrap event.
    pub wrap_events: u64,
    /// Ambiguous large changes, tracking-capacity exhaustion, or a repeated
    /// frontier accepted after the separate dedup history ended.
    pub sequence_anomalies: u64,
    /// Unfilled records forgotten after expired history or closed by an
    /// ambiguous re-anchor. These are observation records, not network losses.
    pub gap_records_discarded: u64,
}

#[derive(Debug)]
struct SequenceGap {
    created_at: Instant,
    expired: bool,
}

/// Observes full outer UDP `u32` sequences without sorting or withholding data.
///
/// Call `observe_at` only for frames accepted by the strict science and recent
/// duplicate gate. One instance belongs to one filtered source endpoint and
/// receiver session; construct a fresh monitor when that session is recreated.
/// It never reads the scientific model's truncated `u16` sequence.
///
/// A small modular forward advance establishes candidates for unobserved
/// intermediate sequences. Late candidates are removed without moving the
/// frontier backward. Untracked backward observations do not create gaps.
/// Changes outside the bounded local range, including exactly half the `u32`
/// range, close existing records and re-anchor at the current observation.
/// This is deliberately not a device-reset or packet-loss inference.
///
/// Waiting candidates become expired after two seconds and remain available
/// for late resolution for five more seconds. Both states share a 256-record
/// bound. A jump that would exceed it also closes records and re-anchors as an
/// anomaly; no partial range is guessed. Call `expire_at` during idle polling.
#[derive(Debug, Default)]
pub struct UdpLvdsSequenceMonitor {
    frontier: Option<u32>,
    gaps: BTreeMap<u32, SequenceGap>,
    counters: UdpLvdsSequenceMetrics,
}

impl UdpLvdsSequenceMonitor {
    /// Observes one already validated and dedup-accepted scientific frame.
    /// Earlier injected time values cannot renew or un-expire gap records.
    pub fn observe_at(&mut self, sequence: u32, now: Instant) {
        self.expire_at(now);
        self.counters.observed_frames = self.counters.observed_frames.saturating_add(1);
        let Some(frontier) = self.frontier else {
            self.frontier = Some(sequence);
            return;
        };

        if let Some(gap) = self.gaps.remove(&sequence) {
            self.counters.late_fills = self.counters.late_fills.saturating_add(1);
            self.counters.out_of_order_frames = self.counters.out_of_order_frames.saturating_add(1);
            if gap.expired {
                self.counters.late_fills_after_expiry =
                    self.counters.late_fills_after_expiry.saturating_add(1);
            }
            return;
        }

        let advance = sequence.wrapping_sub(frontier);
        if advance == 0 {
            self.counters.sequence_anomalies = self.counters.sequence_anomalies.saturating_add(1);
        } else if advance <= DEFAULT_UDP_SEQUENCE_MAX_ADVANCE {
            self.observe_forward(sequence, frontier, advance, now);
        } else if frontier.wrapping_sub(sequence) <= DEFAULT_UDP_SEQUENCE_MAX_ADVANCE {
            self.counters.out_of_order_frames = self.counters.out_of_order_frames.saturating_add(1);
        } else {
            self.reanchor(sequence);
        }
    }

    fn observe_forward(&mut self, sequence: u32, frontier: u32, advance: u32, now: Instant) {
        let new_gaps = (1..advance)
            .filter(|offset| !self.gaps.contains_key(&frontier.wrapping_add(*offset)))
            .count();
        if self.gaps.len().saturating_add(new_gaps) > DEFAULT_UDP_SEQUENCE_CAPACITY {
            self.reanchor(sequence);
            return;
        }

        if advance == 1 {
            self.counters.contiguous_frames = self.counters.contiguous_frames.saturating_add(1);
        } else {
            self.counters.forward_jumps = self.counters.forward_jumps.saturating_add(1);
            for offset in 1..advance {
                self.gaps
                    .entry(frontier.wrapping_add(offset))
                    .or_insert(SequenceGap {
                        created_at: now,
                        expired: false,
                    });
            }
            self.counters.gap_candidates = self
                .counters
                .gap_candidates
                .saturating_add(u64::try_from(new_gaps).unwrap_or(u64::MAX));
        }
        if sequence < frontier {
            self.counters.wrap_events = self.counters.wrap_events.saturating_add(1);
        }
        self.frontier = Some(sequence);
    }

    fn reanchor(&mut self, sequence: u32) {
        self.counters.sequence_anomalies = self.counters.sequence_anomalies.saturating_add(1);
        self.counters.gap_records_discarded = self
            .counters
            .gap_records_discarded
            .saturating_add(u64::try_from(self.gaps.len()).unwrap_or(u64::MAX));
        self.gaps.clear();
        self.frontier = Some(sequence);
    }

    /// Expires waiting candidates once at the exact deadline, then forgets
    /// unresolved records after the additional bounded history window.
    /// Time uses checked monotonic durations, never wall time or deadline sums.
    pub fn expire_at(&mut self, now: Instant) {
        let counters = &mut self.counters;
        self.gaps.retain(|_, gap| {
            let Some(age) = now.checked_duration_since(gap.created_at) else {
                return true;
            };
            if age < DEFAULT_UDP_SEQUENCE_GAP_WAIT {
                return true;
            }
            if !gap.expired {
                gap.expired = true;
                counters.expired_gaps = counters.expired_gaps.saturating_add(1);
            }
            if age.saturating_sub(DEFAULT_UDP_SEQUENCE_GAP_WAIT)
                >= DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY
            {
                counters.gap_records_discarded = counters.gap_records_discarded.saturating_add(1);
                return false;
            }
            true
        });
    }

    #[must_use]
    pub fn metrics(&self) -> UdpLvdsSequenceMetrics {
        let pending = self.gaps.values().filter(|gap| !gap.expired).count();
        UdpLvdsSequenceMetrics {
            pending_gaps: u64::try_from(pending).unwrap_or(u64::MAX),
            unresolved_expired_gaps: u64::try_from(self.gaps.len() - pending).unwrap_or(u64::MAX),
            ..self.counters
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_UDP_SEQUENCE_CAPACITY, DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY,
        DEFAULT_UDP_SEQUENCE_GAP_WAIT, UdpLvdsSequenceMetrics, UdpLvdsSequenceMonitor,
    };
    use std::time::{Duration, Instant};

    fn observe_all(monitor: &mut UdpLvdsSequenceMonitor, values: &[u32], now: Instant) {
        for &sequence in values {
            monitor.observe_at(sequence, now);
        }
    }

    fn assert_gap_accounting(monitor: &UdpLvdsSequenceMonitor) {
        let metrics = monitor.metrics();
        assert_eq!(
            metrics.gap_candidates,
            metrics.pending_gaps
                + metrics.unresolved_expired_gaps
                + metrics.late_fills
                + metrics.gap_records_discarded
        );
        assert!(
            metrics.pending_gaps + metrics.unresolved_expired_gaps
                <= u64::try_from(DEFAULT_UDP_SEQUENCE_CAPACITY).unwrap()
        );
    }

    #[test]
    fn sequential_frames_count_only_steps_after_the_initial_anchor() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        observe_all(&mut monitor, &[100, 101, 102, 103], Instant::now());
        assert_eq!(
            monitor.metrics(),
            UdpLvdsSequenceMetrics {
                observed_frames: 4,
                contiguous_frames: 3,
                ..UdpLvdsSequenceMetrics::default()
            }
        );
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn forward_jump_records_an_unconfirmed_candidate_then_late_fill_resolves_it() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 101, 103], now);
        assert_eq!(monitor.metrics().forward_jumps, 1);
        assert_eq!(monitor.metrics().gap_candidates, 1);
        assert_eq!(monitor.metrics().pending_gaps, 1);
        assert_eq!(monitor.metrics().expired_gaps, 0);
        assert_gap_accounting(&monitor);

        monitor.observe_at(102, now + Duration::from_millis(50));
        let metrics = monitor.metrics();
        assert_eq!(metrics.pending_gaps, 0);
        assert_eq!(metrics.expired_gaps, 0);
        assert_eq!(metrics.late_fills, 1);
        assert_eq!(metrics.out_of_order_frames, 1);
        monitor.observe_at(104, now + Duration::from_millis(100));
        assert_eq!(monitor.metrics().contiguous_frames, 2);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn reversed_late_frames_resolve_each_gap_without_moving_the_frontier_backward() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 101, 104, 103, 102], now);
        assert_eq!(monitor.metrics().gap_candidates, 2);
        assert_eq!(monitor.metrics().late_fills, 2);
        assert_eq!(monitor.metrics().out_of_order_frames, 2);
        assert_eq!(monitor.metrics().pending_gaps, 0);
        monitor.observe_at(105, now);
        assert_eq!(monitor.metrics().contiguous_frames, 2);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn local_backward_frames_are_out_of_order_without_inventing_gaps() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 99, 98, 101], now);
        assert_eq!(monitor.metrics().observed_frames, 4);
        assert_eq!(monitor.metrics().out_of_order_frames, 2);
        assert_eq!(monitor.metrics().late_fills, 0);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_eq!(monitor.metrics().gap_candidates, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn full_u32_wrap_is_contiguous_and_counts_once() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        observe_all(
            &mut monitor,
            &[u32::MAX - 1, u32::MAX, 0, 1],
            Instant::now(),
        );
        assert_eq!(monitor.metrics().contiguous_frames, 3);
        assert_eq!(monitor.metrics().wrap_events, 1);
        assert_eq!(monitor.metrics().sequence_anomalies, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn jump_across_zero_tracks_wrapped_candidates_and_late_fills_them() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[u32::MAX - 1, 1], now);
        assert_eq!(monitor.metrics().gap_candidates, 2);
        assert_eq!(monitor.metrics().wrap_events, 1);
        observe_all(&mut monitor, &[0, u32::MAX, 2], now);
        assert_eq!(monitor.metrics().late_fills, 2);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_eq!(monitor.metrics().wrap_events, 1);
        assert_eq!(monitor.metrics().sequence_anomalies, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn same_low_sixteen_bits_are_not_a_repeat_and_large_change_only_reanchors() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[1, 65_537, 65_538], now);
        assert_eq!(monitor.metrics().observed_frames, 3);
        assert_eq!(monitor.metrics().sequence_anomalies, 1);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_eq!(monitor.metrics().gap_candidates, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn ambiguous_half_range_and_large_backward_change_close_bounded_records() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 103, 0x8000_0067, 10], now);
        assert_eq!(monitor.metrics().gap_candidates, 2);
        assert_eq!(monitor.metrics().gap_records_discarded, 2);
        assert_eq!(monitor.metrics().sequence_anomalies, 2);
        assert_eq!(monitor.metrics().pending_gaps, 0);
        monitor.observe_at(11, now);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn repeated_frontier_is_anomaly_without_clearing_or_refilling_gaps() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 103, 103, 102, 102], now);
        assert_eq!(monitor.metrics().gap_candidates, 2);
        assert_eq!(monitor.metrics().pending_gaps, 1);
        assert_eq!(monitor.metrics().sequence_anomalies, 1);
        assert_eq!(monitor.metrics().late_fills, 1);
        assert_eq!(monitor.metrics().out_of_order_frames, 2);
        assert_eq!(monitor.metrics().gap_records_discarded, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn waiting_deadline_is_exact_and_expiration_events_are_not_permanent_missing() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 102], now);
        let deadline = now + DEFAULT_UDP_SEQUENCE_GAP_WAIT;
        monitor.expire_at(deadline.checked_sub(Duration::from_nanos(1)).unwrap());
        assert_eq!(monitor.metrics().pending_gaps, 1);
        assert_eq!(monitor.metrics().expired_gaps, 0);
        monitor.expire_at(deadline);
        monitor.expire_at(deadline + Duration::from_nanos(1));
        assert_eq!(monitor.metrics().pending_gaps, 0);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 1);
        assert_eq!(monitor.metrics().expired_gaps, 1);
        monitor.observe_at(101, deadline + Duration::from_millis(50));
        assert_eq!(monitor.metrics().late_fills, 1);
        assert_eq!(monitor.metrics().late_fills_after_expiry, 1);
        assert_eq!(monitor.metrics().expired_gaps, 1);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn expired_history_forgets_records_at_the_exact_limit_without_claiming_loss() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 102], now);
        let forgotten = now + DEFAULT_UDP_SEQUENCE_GAP_WAIT + DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY;
        monitor.expire_at(forgotten.checked_sub(Duration::from_nanos(1)).unwrap());
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 1);
        monitor.expire_at(forgotten);
        assert_eq!(monitor.metrics().gap_records_discarded, 1);
        assert_eq!(monitor.metrics().expired_gaps, 1);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 0);
        monitor.observe_at(101, forgotten);
        assert_eq!(monitor.metrics().out_of_order_frames, 1);
        assert_eq!(monitor.metrics().late_fills, 0);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn advancing_traffic_and_repeated_values_do_not_extend_candidate_lifetime() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 102], now);
        observe_all(
            &mut monitor,
            &[102, 103, 104],
            now + Duration::from_millis(1_900),
        );
        monitor.expire_at(now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        assert_eq!(monitor.metrics().gap_candidates, 1);
        assert_eq!(monitor.metrics().expired_gaps, 1);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 1);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn earlier_injected_time_does_not_expire_or_revert_expired_records() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 102], now);
        let earlier = now.checked_sub(Duration::from_millis(1)).unwrap();
        monitor.expire_at(earlier);
        assert_eq!(monitor.metrics().pending_gaps, 1);
        monitor.expire_at(now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        monitor.expire_at(earlier);
        assert_eq!(monitor.metrics().pending_gaps, 0);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 1);
        monitor.observe_at(101, earlier);
        assert_eq!(monitor.metrics().late_fills_after_expiry, 1);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn capacity_bound_includes_expired_history_and_reanchors_without_partial_gaps() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[0, 256], now);
        assert_eq!(monitor.metrics().pending_gaps, 255);
        monitor.observe_at(258, now);
        assert_eq!(monitor.metrics().pending_gaps, 256);
        monitor.expire_at(now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        monitor.observe_at(260, now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        assert_eq!(monitor.metrics().sequence_anomalies, 1);
        assert_eq!(monitor.metrics().gap_candidates, 256);
        assert_eq!(monitor.metrics().gap_records_discarded, 256);
        assert_eq!(monitor.metrics().pending_gaps, 0);
        assert_eq!(monitor.metrics().unresolved_expired_gaps, 0);
        monitor.observe_at(261, now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn repeated_capacity_pressure_stays_bounded_and_new_contiguous_data_recovers() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        for cycle in 0..128 {
            let anchor = cycle * 1_000;
            observe_all(
                &mut monitor,
                &[anchor, anchor + 256, anchor + 258, anchor + 260],
                now,
            );
            assert_gap_accounting(&monitor);
        }
        assert_eq!(monitor.metrics().pending_gaps, 0);
        assert_eq!(monitor.metrics().gap_candidates, 128 * 256);
        assert_eq!(monitor.metrics().gap_records_discarded, 128 * 256);
        assert_eq!(monitor.metrics().sequence_anomalies, 255);
        monitor.observe_at(127_261, now);
        assert_eq!(monitor.metrics().contiguous_frames, 1);
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn four_hundred_frames_at_twenty_hertz_have_no_false_quality_events() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        for sequence in 0..400 {
            monitor.observe_at(
                sequence,
                now + Duration::from_millis(u64::from(sequence) * 50),
            );
        }
        assert_eq!(
            monitor.metrics(),
            UdpLvdsSequenceMetrics {
                observed_frames: 400,
                contiguous_frames: 399,
                ..UdpLvdsSequenceMetrics::default()
            }
        );
        assert_gap_accounting(&monitor);
    }

    #[test]
    fn new_receiver_session_has_independent_frontier_counters_and_candidates() {
        let mut previous = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut previous, &[100, 103], now);
        let mut reconnected = UdpLvdsSequenceMonitor::default();
        observe_all(&mut reconnected, &[0, 1], now);
        assert_eq!(reconnected.metrics().observed_frames, 2);
        assert_eq!(reconnected.metrics().contiguous_frames, 1);
        assert_eq!(reconnected.metrics().gap_candidates, 0);
        assert_eq!(previous.metrics().pending_gaps, 2);
        assert_gap_accounting(&previous);
        assert_gap_accounting(&reconnected);
    }

    #[test]
    fn cumulative_counters_saturate_without_overflowing_state() {
        let mut monitor = UdpLvdsSequenceMonitor::default();
        let now = Instant::now();
        observe_all(&mut monitor, &[100, 103], now);
        monitor.counters = UdpLvdsSequenceMetrics {
            observed_frames: u64::MAX,
            contiguous_frames: u64::MAX,
            forward_jumps: u64::MAX,
            gap_candidates: u64::MAX,
            expired_gaps: u64::MAX,
            late_fills: u64::MAX,
            late_fills_after_expiry: u64::MAX,
            out_of_order_frames: u64::MAX,
            wrap_events: u64::MAX,
            sequence_anomalies: u64::MAX,
            gap_records_discarded: u64::MAX,
            ..UdpLvdsSequenceMetrics::default()
        };
        monitor.expire_at(now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        monitor.observe_at(102, now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        monitor.observe_at(104, now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        monitor.observe_at(106, now + DEFAULT_UDP_SEQUENCE_GAP_WAIT);
        observe_all(&mut monitor, &[0, u32::MAX, 1, 1], now);
        observe_all(&mut monitor, &[u32::MAX - 1_000, u32::MAX, 0], now);
        let metrics = monitor.metrics();
        assert_eq!(metrics.observed_frames, u64::MAX);
        assert_eq!(metrics.contiguous_frames, u64::MAX);
        assert_eq!(metrics.forward_jumps, u64::MAX);
        assert_eq!(metrics.gap_candidates, u64::MAX);
        assert_eq!(metrics.expired_gaps, u64::MAX);
        assert_eq!(metrics.late_fills, u64::MAX);
        assert_eq!(metrics.late_fills_after_expiry, u64::MAX);
        assert_eq!(metrics.out_of_order_frames, u64::MAX);
        assert_eq!(metrics.wrap_events, u64::MAX);
        assert_eq!(metrics.sequence_anomalies, u64::MAX);
        monitor.expire_at(now + Duration::from_secs(10));
        assert_eq!(monitor.metrics().gap_records_discarded, u64::MAX);
    }
}
