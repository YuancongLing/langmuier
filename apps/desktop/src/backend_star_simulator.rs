use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use langmuir_core::{BroadcastKind, GnssPoint, StarTime, encode_broadcast};
use langmuir_transport::{BroadcastCancellation, WriteAckError, WriteTicket};

use super::Controller;
use crate::bridge::{BackendEvent, LogLevel};
use crate::star_simulator::{StarSimulatorConfig, StarSimulatorStatus};

struct PendingBroadcast {
    ticket: WriteTicket,
    kind: BroadcastKind,
    due: Instant,
}

#[derive(Default)]
pub(super) struct StarComputerSimulator {
    pub(super) status: StarSimulatorStatus,
    next_due: [Option<Instant>; 3],
    last_started: [Option<Instant>; 3],
    cancellation: BroadcastCancellation,
    pending: BTreeMap<u64, PendingBroadcast>,
    sequence: u16,
}

impl StarComputerSimulator {
    pub(super) fn start(
        &mut self,
        config: StarSimulatorConfig,
        now: Instant,
    ) -> Result<(), String> {
        config.validate()?;
        self.stop();
        self.status.config = config;
        self.status.active = true;
        self.status.last_error = None;
        self.last_started = [None; 3];
        self.status.timing = Default::default();
        self.next_due = std::array::from_fn(|i| self.status.config.enabled[i].then_some(now));
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        self.cancellation.cancel();
        self.cancellation = BroadcastCancellation::default();
        self.status.active = false;
        self.next_due = [None; 3];
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Vec<(BroadcastKind, Instant)> {
        let mut due = Vec::with_capacity(3);
        if !self.status.active {
            return due;
        }
        for kind in BroadcastKind::ALL {
            let index = kind.index();
            let Some(deadline) = self.next_due[index].filter(|deadline| now >= *deadline) else {
                continue;
            };
            let interval =
                Duration::from_millis(u64::from(self.status.config.interval_millis[index]));
            let missed = now.saturating_duration_since(deadline).as_millis() / interval.as_millis();
            self.status.skipped[index] = self.status.skipped[index]
                .saturating_add(u64::try_from(missed).unwrap_or(u64::MAX));
            self.next_due[index] = Some(now + interval);
            if self.pending.values().any(|item| item.kind == kind) {
                self.status.skipped[index] = self.status.skipped[index].saturating_add(1);
            } else {
                due.push((kind, deadline));
            }
        }
        due
    }
}

impl Controller {
    pub(super) fn configure_star_simulator(&mut self, config: StarSimulatorConfig) {
        if !config.enabled.iter().any(|enabled| *enabled) {
            self.star_simulator.status.config = config;
            self.stop_star_simulator();
            return;
        }
        if let Err(error) = config.validate() {
            self.star_simulator_error(error);
            return;
        }
        // Already queued frames own their original point snapshot.
        if self.star_simulator.status.active {
            let now = Instant::now();
            self.star_simulator.next_due = std::array::from_fn(|index| {
                config.enabled[index]
                    .then(|| now + Duration::from_millis(u64::from(config.interval_millis[index])))
            });
        }
        self.star_simulator.status.config = config;
        self.publish_star_simulator();
    }

    pub(super) fn start_star_simulator(&mut self, config: StarSimulatorConfig) {
        if !self.rs422_connected {
            self.star_simulator_error("RS422 未连接，不能启动星务模拟".to_owned());
            return;
        }
        if let Err(error) = config.validate() {
            self.star_simulator_error(error);
            return;
        }
        if self
            .telemetry_polling
            .is_none_or(|polling| polling.interval_millis() >= 20_000)
        {
            self.set_telemetry_polling(true, 5_000);
        }
        if self.telemetry_polling.is_none() {
            self.star_simulator_error("遥测轮询未能启动，星务模拟未启动".to_owned());
            return;
        }
        let now = Instant::now();
        if let Err(error) = self.star_simulator.start(config, now) {
            self.star_simulator_error(error);
            return;
        }
        self.log(
            LogLevel::Info,
            format!(
                "用户主动开启星务模拟 RS422 发送 · SYNTHETIC · {} · {}",
                self.rs422_endpoint, self.star_simulator.status.config.point.label,
            ),
        );
        self.poll_star_simulator(now);
    }

    pub(super) fn stop_star_simulator(&mut self) {
        let was_active = self.star_simulator.status.active;
        self.star_simulator.stop();
        self.drain_star_broadcast_acks();
        self.publish_star_simulator();
        if was_active {
            self.log(
                LogLevel::Info,
                "星务模拟周期广播已停止；未开始写入的广播已取消",
            );
        }
    }

    pub(super) fn send_star_broadcast(
        &mut self,
        kind: BroadcastKind,
        point: GnssPoint,
        due: Instant,
    ) {
        let Some(worker) = self.rs422.as_ref().filter(|_| self.rs422_connected) else {
            self.star_simulator_error("RS422 未连接，广播未发送".to_owned());
            return;
        };
        let index = kind.index();
        if self
            .star_simulator
            .pending
            .values()
            .any(|pending| pending.kind == kind)
        {
            self.star_simulator.status.skipped[index] =
                self.star_simulator.status.skipped[index].saturating_add(1);
            self.publish_star_simulator();
            return;
        }
        let sequence = self.star_simulator.sequence;
        // Reject invalid point/clock input before reserving bounded writer space.
        let validate = StarTime::from_system_time(SystemTime::now())
            .and_then(|time| encode_broadcast(kind, time, point, sequence));
        if let Err(error) = validate {
            self.star_simulator.status.failed[index] =
                self.star_simulator.status.failed[index].saturating_add(1);
            self.star_simulator_error(error.to_string());
            return;
        }
        match worker.try_send_timed_broadcast(
            self.star_simulator.cancellation.clone(),
            move |started_at| {
                let time =
                    StarTime::from_system_time(started_at).map_err(|error| error.to_string())?;
                encode_broadcast(kind, time, point, sequence).map_err(|error| error.to_string())
            },
        ) {
            Ok(ticket) => {
                self.star_simulator
                    .pending
                    .insert(ticket.id(), PendingBroadcast { ticket, kind, due });
                if kind == BroadcastKind::Gnss {
                    self.star_simulator.sequence = (sequence + 1) & 0x3FFF;
                }
            }
            Err(error) => {
                self.star_simulator.status.skipped[index] =
                    self.star_simulator.status.skipped[index].saturating_add(1);
                self.star_simulator.status.last_error = Some(error.to_string());
            }
        }
        self.publish_star_simulator();
    }

    pub(super) fn poll_star_simulator(&mut self, now: Instant) {
        self.drain_star_broadcast_acks();
        if !self.rs422_connected {
            if self.star_simulator.status.active {
                self.stop_star_simulator();
            }
            return;
        }
        if self.star_simulator.status.active && self.telemetry_polling.is_none() {
            self.star_simulator.status.last_error =
                Some("遥测轮询停止，已停止周期星务模拟".to_owned());
            self.stop_star_simulator();
            return;
        }
        let point = self.star_simulator.status.config.point;
        for (kind, due) in self.star_simulator.take_due(now) {
            self.send_star_broadcast(kind, point, due);
        }
    }

    fn drain_star_broadcast_acks(&mut self) {
        let completed: Vec<_> = self
            .star_simulator
            .pending
            .iter()
            .filter_map(|(&id, pending)| pending.ticket.try_ack().map(|ack| (id, ack)))
            .collect();
        if completed.is_empty() {
            return;
        }
        for (id, result) in completed {
            let pending = self
                .star_simulator
                .pending
                .remove(&id)
                .expect("completed pending broadcast");
            let index = pending.kind.index();
            match result {
                Ok(receipt) => {
                    let state = &mut self.star_simulator;
                    state.status.sent[index] = state.status.sent[index].saturating_add(1);
                    state.status.last_sent_utc = Some(receipt.started_at);
                    state.status.last_frames[index] = receipt.frame;
                    let lateness = u64::try_from(
                        receipt
                            .started
                            .saturating_duration_since(pending.due)
                            .as_micros(),
                    )
                    .unwrap_or(u64::MAX);
                    let timing = &mut state.status.timing[index];
                    if let Some(last) = state.last_started[index] {
                        timing.samples = timing.samples.saturating_add(1);
                        timing.last_interval_micros = u64::try_from(
                            receipt.started.saturating_duration_since(last).as_micros(),
                        )
                        .unwrap_or(u64::MAX);
                    }
                    state.last_started[index] = Some(receipt.started);
                    timing.max_lateness_micros = timing.max_lateness_micros.max(lateness);
                    timing.total_lateness_micros =
                        timing.total_lateness_micros.saturating_add(lateness);
                    self.log(
                        LogLevel::Info,
                        format!(
                            "星务模拟来源 · SYNTHETIC · {} 整帧已写入本机 RS422（无设备应答）",
                            pending.kind.label()
                        ),
                    );
                }
                Err(WriteAckError::Cancelled { .. }) => {
                    self.star_simulator.status.skipped[index] =
                        self.star_simulator.status.skipped[index].saturating_add(1);
                }
                Err(error) => {
                    self.star_simulator.status.failed[index] =
                        self.star_simulator.status.failed[index].saturating_add(1);
                    self.star_simulator.status.last_error = Some(error.to_string());
                    self.star_simulator.stop();
                    self.fault("星务模拟写入", error.to_string());
                }
            }
        }
        self.publish_star_simulator();
    }

    fn star_simulator_error(&mut self, message: String) {
        self.star_simulator.status.last_error = Some(message.clone());
        self.fault("星务模拟", message);
        self.publish_star_simulator();
    }

    fn publish_star_simulator(&mut self) {
        self.star_simulator.status.queue_depth = self
            .rs422
            .as_ref()
            .map_or(0, langmuir_transport::ReaderWorker::broadcast_queue_depth);
        self.emit_critical(BackendEvent::StarSimulatorStatus(Box::new(
            self.star_simulator.status.clone(),
        )));
    }
}
