# LVDS P1-3 UDP 完整序号连续性与科学数据质量报告

日期：2026-10-19。HEAD：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。

本阶段软件验收 **PASS**：仅在科学验证及近期去重门禁返回 Accepted 后，用完整外层 `u32` 观察连续、跳号、缺口、迟到、乱序、回绕及无法判定的变化。含 Mock workspace 最终 **170 passed**，较原 149 项新增 21 项。原到达/重组完成交付顺序、存储格式、CRC16、残帧 TTL 和去重策略保持。严格 workspace Clippy 仍为 **FAIL**，新增代码诊断结果单列于第 8 节。

状态：`PASS` 为实际执行通过，`FAIL` 为实际检查失败，`BLOCKED` 为环境阻断，`NOT_RUN` 为未执行，`SPEC_ASSUMPTION` 为尚未获厂家确认的解释或软件策略。本轮没有连接硬件、读取历史异常 PCAP、发送设备控制指令或执行 Git 提交。

## 1. 修改前审计与限制

首先执行 `git status --short` 和 `git diff --stat`。开始时已有 CRC16、TTL 和去重三个阶段的 18 个已跟踪修改文件，累计 2175 insertions、268 deletions；另有未跟踪的去重模块与三个历史报告。本轮增量实施，历史报告保留原样。

现有链路为：来源过滤 → 24B UDP 封装 → 按 offset 重组及 2 秒残帧 TTL → 269B 科学结构、CRC16、时间和 ADC 验证 → 5 秒 / 256 帧去重 → Controller → BatchWriter 与 GUI 科学事件。

审计确认：`ReassembledLvdsFrame.frame_sequence` 保留完整 `u32`，在 `UdpLvdsScienceGate` 返回 Accepted 后仍可访问。科学模型中的 `ScientificFrame.sequence` 已按原兼容规则截为 `u16`，不能据此分析完整连续性。此前只有分片、科学验证、重复和冲突指标，没有跨完整科学帧的观察参考或缺口状态。

厂家资料只说明外层大端 `u32`，未明确序号复位、回绕、设备会话及重传语义。因此不能从序号倒退直接推断设备重启，也不能把有效科学帧未被观察直接宣称为 UDP 网络丢包。

## 2. 新监测器与接入位置

新增纯逻辑模块 `crates/langmuir-core/src/udp_lvds_sequence.rs`，导出 `UdpLvdsSequenceMonitor`、`UdpLvdsSequenceMetrics` 与四个集中常量。API 为 `observe_at(sequence, Instant)`、`expire_at(Instant)` 和 `metrics()`。不读文件、socket 或科学模型，不排序、不拒收、不生成样本，也没有后台线程。

```text
过滤来源 → 原 24B 封装 → 原重组 / 2s TTL
  → 原科学验证：结构 / CRC16 / 时间 / 两路 ADC
  → 原 5s / 256 FIFO 去重门禁
  → Accepted：sequence_monitor.observe_at(原完整 u32, now)
  → 原正常回调 → Controller → 原存储 / GUI
```

桌面 `UdpLvdsIngress::push_at()` 仅在 `outcome.into_frame()` 为 `Some` 时观察，随后原样调用 `on_frame(scientific)`。重复、合法内容冲突、科学错误及错误来源不推进观察。原 `expire_at()` 同时维护序号记录，并保持返回“被清理的残帧数量”的既有语义；空闲 socket 由原 Controller tick 清理。

CLI `receive-udp-lvds` 共用同一监测器，只在 Accepted 后、原 raw/index 写入前观察完整序号，100ms 读超时及退出时维护到期记录，结束/超时汇总全部序号指标。raw/index 写入顺序、列定义和目标帧数量规则未改。监测发生在下游回调/写入前，不是存储落盘回执；后续 I/O 失败不会回滚观察计数。

每个实例属于一个已过滤源端点和一个接收会话。重新监听会销毁旧 receiver/ingress，并创建新监测器；每次 CLI 接收调用也独立创建。采集启停不等于接收会话重建，继续保留该会话状态。旧字节流 reader 缺少可靠外层标签，新增序号指标保持零；直接 GUI Mock 不伪造 UDP 序号观察。

## 3. 状态模型与有界策略

监测器保存一个可选参考序号 `frontier`、以完整 `u32` 为键的 `BTreeMap` 缺口记录和饱和累计计数。记录只含首次发现 `Instant` 和是否已等待到期。初始参考为空，首个 Accepted 帧只建立参考。

| 常量 | 当前值与含义 |
| --- | --- |
| `DEFAULT_UDP_SEQUENCE_GAP_WAIT` | 2 秒候选等待期，与残帧 TTL 独立 |
| `DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY` | 等待期结束后额外保留 5 秒未解决历史 |
| `DEFAULT_UDP_SEQUENCE_CAPACITY` | 等待与到期历史合计最多 256 条 |
| `DEFAULT_UDP_SEQUENCE_MAX_ADVANCE` | 前进/后退局部解释距离上限 256 |

以上均为集中定义的软件策略，属于 `SPEC_ASSUMPTION`，不是厂家确认的网络延迟或复位规则；本轮没有增加用户配置界面。

每次观察先清理到期状态，再优先查找已有缺口。若没有命中，使用 `sequence.wrapping_sub(frontier)` 和相反方向的无符号距离判断，不进行负数或有符号转换：

| 观察条件 | 状态及计数 |
| --- | --- |
| 首次观察 | 建立参考，不计连续或缺口 |
| 命中等待/保留的到期缺口 | 移除一次，计迟到补齐和乱序；到期记录另计到期后补齐；参考不后退 |
| 向前距离为 1 | 连续加一，更新参考 |
| 向前距离 2–256，容量足够 | 跳号事件加一，登记中间未观察序号，再更新参考 |
| 未命中缺口、向后距离 1–256 | 乱序加一，不建立缺口、不后退参考 |
| 与当前参考相同且被去重门禁再次接受 | 异常加一，不推进参考，不清除已有缺口 |
| 巨大/半圈变化，或新增缺口将超过容量 | 异常加一，遗忘旧缺口并单独记账，以当前帧重锚观察参考 |

只对被解释为局部前进且更新参考的跨零行为增加回绕计数。`FFFFFFFE,FFFFFFFF,0,1` 是三次连续、一次回绕；跨零跳号可同时计一次跳号和一次回绕。迟到帧跨零填补不再次计回绕。

`100,101,103` 只建立 102 候选，不立即计永久缺失。记录年龄 `<2s` 保持等待，`>=2s` 只累计一次等待到期，并进入未解决到期状态。之后仍可填补，降低当前未解决量。首次发现起 `>=7s` 仍未补齐则遗忘；延迟清理一次跨过两个期限时，分别记录一次到期及一次遗忘。后续数据或重复观察均不续期。

时间使用 `checked_duration_since()` 和 `Duration::saturating_sub()`，不使用墙上时间、未检查减法或截止时间加法。更早的注入时间不使记录错误到期，也不将已到期状态回退。

每次最多检查/建立 255 个中间候选；缺口状态始终最多 256 条，清理及快照扫描也受此限制，无与输入序号差值成比例的巨大循环。容量不足不登记部分区间，而是显式异常重锚。监测没有锁、sleep、网络或磁盘等待；本轮未执行实时延迟或进程堆基准，不能据此宣称硬实时保证。

重锚只改变观察参考，不重置去重、残帧表或接收会话，不推断设备重启，不阻止当前合法科学帧交付。被遗忘的记录无法在窗口外继续关联；以后极迟到的帧按当时局部距离或异常规则观察。

## 4. 指标精确定义

`ReaderMetricsSnapshot.udp_sequence` 与 `LinkMetricsSnapshot.udp_sequence` 复用同一个 `Copy/Default/Eq` 核心指标结构。除明确标记 gauge 的两项，以下均为饱和累计值：

| 字段 | 定义 |
| --- | --- |
| `observed_frames` | 门禁 Accepted 后传给监测器的帧数，包括初始锚点 |
| `contiguous_frames` | 比当前参考模前进恰好一步的次数，不含初始帧或迟到补齐 |
| `forward_jumps` | 成功建立完整候选范围的局部前进 2–256 事件数；容量不足归异常 |
| `gap_candidates` | 累计登记的中间候选序号数量，不是跳号事件数 |
| `pending_gaps` | gauge：当前仍在 2 秒等待期的候选数 |
| `expired_gaps` | 累计等待期满事件；补齐后不撤销，不代表永久缺失或网络丢包 |
| `unresolved_expired_gaps` | gauge：仍被保留、尚未补齐的到期记录数 |
| `late_fills` | 移除已跟踪等待/到期候选的补齐次数 |
| `late_fills_after_expiry` | `late_fills` 的子集：候选已到期后才补齐 |
| `out_of_order_frames` | 全部迟到补齐，加未命中候选的局部向后观察；包含 `late_fills` |
| `wrap_events` | 局部前进并更新参考时跨越 `u32` 零点的次数 |
| `sequence_anomalies` | 巨大/半圈变化、容量不足或再次接受当前参考的次数 |
| `gap_records_discarded` | 因额外历史到期或异常重锚而遗忘的未补齐记录数 |

在计数尚未饱和时，测试验证以下账目恒等式：

```text
gap_candidates = pending_gaps + unresolved_expired_gaps
               + late_fills + gap_records_discarded
```

迟到是乱序子集，到期后迟到是迟到子集；这些计数不能相加当作互斥帧数。累计等待到期不会因后续补齐降低，但当前未解决到期 gauge 会降低，因此不会把已经补齐的序号继续列为当前缺口。

原 `reassembled_frames` 仍含科学错误、重复及冲突；`frames_decoded` 仍含全部科学验证成功候选；`scientific_frames_delivered` 表示 Accepted 后正常回调次数。当前桌面路径中 `observed_frames` 与最后一项同步，均不等价于实际存储/显示成功数。序号观察不增加拒收或 CRC 计数。

日志沿用 500ms 快照与非阻塞事件通道，将跳号、到期、迟到、乱序、回绕、异常和遗忘的增长合并为现有质量日志。正常观察/连续以及单独的 gauge 更新保持静默，不逐帧输出；日志用“待观察有效序号”“缺口等待到期”，不命名为网络丢包。

## 5. 核心新增 18 项测试

全部通过正式 Cargo 执行；`Instant` 由测试注入，没有用长时间真实 sleep 推进等待窗口。

| 测试函数 | 状态与证据 |
| --- | --- |
| `sequential_frames_count_only_steps_after_the_initial_anchor` | PASS：100,101,102,103，观察 4、连续 3、无缺口 |
| `forward_jump_records_an_unconfirmed_candidate_then_late_fill_resolves_it` | PASS：100,101,103 建候选，102 补齐后等待为 0，104 继续连续 |
| `reversed_late_frames_resolve_each_gap_without_moving_the_frontier_backward` | PASS：100,101,104,103,102，补齐 2、乱序 2，105 仍连续 |
| `local_backward_frames_are_out_of_order_without_inventing_gaps` | PASS：局部旧帧只计乱序，不凭空登记缺口 |
| `full_u32_wrap_is_contiguous_and_counts_once` | PASS：FFFFFFFE,FFFFFFFF,0,1，连续 3、回绕 1 |
| `jump_across_zero_tracks_wrapped_candidates_and_late_fills_them` | PASS：跨零跳号登记 MAX/0，反序补齐，回绕不重复计数 |
| `same_low_sixteen_bits_are_not_a_repeat_and_large_change_only_reanchors` | PASS：1,65537 是完整序号巨大变化，计异常；65538 随后连续 |
| `ambiguous_half_range_and_large_backward_change_close_bounded_records` | PASS：半圈及巨大后退不推测复位，单独记旧缺口遗忘 |
| `repeated_frontier_is_anomaly_without_clearing_or_refilling_gaps` | PASS：重复参考不推进，已移除缺口不再次补齐 |
| `waiting_deadline_is_exact_and_expiration_events_are_not_permanent_missing` | PASS：2s−1ns 等待，恰 2s 只到期一次，之后补齐解除未解决量 |
| `expired_history_forgets_records_at_the_exact_limit_without_claiming_loss` | PASS：7s 精确遗忘，之后不再关联为原候选补齐 |
| `advancing_traffic_and_repeated_values_do_not_extend_candidate_lifetime` | PASS：后续序号和重复值均不续期 |
| `earlier_injected_time_does_not_expire_or_revert_expired_records` | PASS：逆时间安全，已到期状态不回退 |
| `capacity_bound_includes_expired_history_and_reanchors_without_partial_gaps` | PASS：等待及到期共用 256 上限，超限异常，无部分登记 |
| `repeated_capacity_pressure_stays_bounded_and_new_contiguous_data_recovers` | PASS：128 轮压力，32768 候选均正确记遗忘，状态始终有界，随后连续恢复 |
| `four_hundred_frames_at_twenty_hertz_have_no_false_quality_events` | PASS：400 观察、399 连续，其余序号事件及 gauge 为 0 |
| `new_receiver_session_has_independent_frontier_counters_and_candidates` | PASS：新对象无旧参考、缺口或计数继承 |
| `cumulative_counters_saturate_without_overflowing_state` | PASS：全部累计值在 `u64::MAX` 饱和，当前状态仍有界 |

## 6. 桌面生产链与强制场景

新增 3 项桌面测试均为 **PASS**：

| 测试 | 实际证据 |
| --- | --- |
| `udp_sequence_observes_only_accepted_science_and_retains_late_expired_fills` | 100 合法、101 坏 CRC、102 合法、102 重复、102 合法冲突及错误来源；只观察 100/102。恰 2s 空闲维护后 101 合法重传补齐，未解决到期量归零，历史到期仍为 1 |
| `udp_sequence_monitor_preserves_controller_storage_and_display_arrival_order` | 输入 100,100,101,104,103,102，经真实 Controller、BatchWriter 内存 sink 与 GUI 队列；三处均只有 100,101,104,103,102 五次且同序 |
| `udp_sequence_quality_logs_aggregate_changes_and_leave_normal_progress_silent` | 同一异常快照只一条聚合日志；相同快照及正常连续/gauge-only 更新无新增日志；完整指标桥接一致 |

上述存储/GUI 测试用独立测试 CRC 参考函数重算变化后的合法帧，每帧首 ADC 携带序号以核对显示顺序。实际计数为：观察/回调/存储/GUI 各 5，重复抑制 1，连续 1，跳号 1，候选 2，迟到补齐 2，乱序 2，最终等待 0、到期 0、异常 0。监测没有把 103/102 排回前面，也没有补出其他样本。

原测试全部保留，并扩展断言：

- 完整 `u32` 的 7 与 65543 各自 Accepted，不能按低 16 位合并；CRC、冲突、重复不增加观察数。
- 同一 ingress 的 MAX→0 正常回绕，新 ingress 独立；去重到期后再次接受当前参考记异常，不阻止合法交付。
- 原 400 帧 / 50ms ingress 测试为观察 400、连续 399，全部其他序号字段为零。
- 原 64 槽 TTL 恢复集成仍为超时 64、容量拒绝 1、重组 5、科学验证成功 4、重复抑制 2、坏 CRC 1、正式回调/存储/GUI 各 2；序号观察 2、连续 1。

坏 CRC 场景只说明 101 的有效科学帧尚未被观察，不产生“已确认网络丢包”指标。随后同序号合法重传仍可填补，说明 CRC 错误没有污染去重或连续性状态。

400 帧时间模拟从 0 到 19.95 秒，不是设备实收 20 秒。完整生产存储/显示证据为内存数据报注入同一 `UdpLvdsIngress`；本轮未做桌面真实 socket 故障注入、CLI socket→文件专项、GUI 截图或硬件验收，均为 **NOT_RUN**。workspace 中既有 localhost UDP 回环测试实际通过，不等同于真实设备测试。

## 7. Cargo 与此前阶段回归

工具链：`rustc 1.99.0 (b940084d7 2026-09-28)`。依赖版本、Cargo.toml 和 Cargo.lock 未改。

| 命令 / 验证 | 实际结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test -p langmuir-core --locked` | PASS：83 tests |
| `cargo test -p langmuir-desktop --features mock --locked --offline` | PASS：56 tests |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS：170，core 83 / desktop 56 / CLI 5 / storage 9 / transport 17 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | FAIL：退出 101，既有 lint，详见第 8 节 |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| 原 CRC16 黄金帧、错误范围/字节序及坏 CRC 拒收 | PASS：269B、`[7..267]`、`44C8/5EF9` 既有测试继续通过 |
| 原 TTL T1–T12 与 128 轮 64 槽容量恢复 | PASS：原实现及测试源码未改，全部继续通过 |
| 原 P1-2 12 项核心去重测试及生产链去重测试 | PASS：5 秒 / 256 FIFO、同序号冲突、错误帧不缓存语义保持 |
| 六条 RS422 黄金帧、SUM、遥测及流式恢复 | PASS：既有测试继续通过 |

总数为 `149 + 核心新增18 + 桌面新增3 = 170`，没有删除测试。含 localhost 绑定的 workspace 测试使用沙箱外本机回环权限执行，没有连接真实设备。早期编辑中格式检查曾提示新代码排版差异，已修正，表中为最终结果；无遗留环境阻断。

## 8. 严格 Clippy 与新增代码质量

实施前和收尾均实际执行用户要求的严格 workspace Clippy，均 **FAIL**。首批诊断一致：

| 文件 | 既有问题与证据 |
| --- | --- |
| `tools/protocol-cli/src/main.rs:325` | `parse_hex()` 的 `chunks_exact_to_as_chunks`；本轮前相同代码在 311 行并实际报错 |
| `crates/langmuir-storage/src/binary.rs:143,146` | `explicit_iter_loop`；文件无差异，本轮前同诊断 |
| `crates/langmuir-core/src/framing.rs:289,291` | 原 RS422 测试的 `assert_is_empty`；本轮前后 SHA-256 一致 |

另外仍有既有 `app.rs` 的五个过长函数，以及缺分号、`match_result_ok`、格式参数、`if_not_else`、`manual_clamp`、`items_after_statements`、常量断言等诊断。保留 `too_many_lines` 的桌面诊断只报告这五个既有函数，新增 backend 函数未出现该诊断。

本轮指标增大曾实际触发新的 `large_enum_variant`。已将 `BackendEvent::LinkMetrics` 中快照改为 `Box<LinkMetricsSnapshot>`，两处发送点装箱，`app.rs` 两处指标赋值解包；没有禁用该 lint，没有修改科学事件或布局。装箱发生在既有指标快照发布处，不是每帧科学事件处。

新增核心库严格 Clippy **PASS**；核心 all-target 严格检查仅因上述既有 `assert_is_empty` **FAIL**，命令行只放宽该既有类别后 **PASS**。最终含 Mock 的下列诊断实际 **PASS**，未发现其他新增诊断；它不替代严格 workspace 的 **FAIL**：

```text
cargo clippy --workspace --all-targets --features langmuir-desktop/mock --locked -- -D warnings
  -A clippy::chunks_exact_to_as_chunks -A clippy::explicit_iter_loop
  -A clippy::assert_is_empty -A clippy::too_many_lines
  -A clippy::semicolon_if_nothing_returned -A clippy::match_result_ok
  -A clippy::uninlined_format_args -A clippy::if_not_else
  -A clippy::manual_clamp -A clippy::items_after_statements
  -A clippy::assertions_on_constants
```

上面换行仅为阅读，复现时合并为同一命令。未改工程 lint 配置，也未用放宽诊断冒充严格通过。

## 9. Git 变更与保护核对

本轮直接变更七个 Rust 文件、三份当前说明与本报告：

| 文件 | 本轮职责 |
| --- | --- |
| `crates/langmuir-core/src/udp_lvds_sequence.rs`（新增） | 纯序号观察、指标及 18 项测试 |
| `crates/langmuir-core/src/lib.rs` | 导出新模块 API |
| `apps/desktop/src/backend.rs` | Accepted 后观察、空闲维护、指标、聚合日志、3 项新增及既有测试扩展 |
| `apps/desktop/src/bridge.rs` | 嵌套序号快照及指标事件装箱 |
| `apps/desktop/src/app.rs` | 仅两处 `Some(metrics)` 改为 `Some(*metrics)` 接收装箱指标；原 UI 布局未变 |
| `crates/langmuir-transport/src/metrics.rs` | 嵌套指标字段，旧字节流为默认零 |
| `tools/protocol-cli/src/main.rs` | 共用监测器及结束/超时汇总，原 capture 格式和顺序未变 |
| `docs/UDP_LVDS_REASSEMBLY_PROTOCOL.md`、`ARCHITECTURE.md`、`MIGRATION.md` | 同步当前观察行为和边界 |
| `docs/UDP_LVDS_SEQUENCE_QUALITY_REPORT.md`（新增） | 本报告 |

本轮前后 SHA-256 实际比较一致的 14 个保护文件：核心 `checksum.rs/framing.rs/lvds.rs/udp_lvds.rs/udp_lvds_dedup.rs/command.rs/rs422.rs`，transport `core_adapter.rs/mock.rs/pipeline_test.rs`，三个历史 CRC16/TTL/去重报告及 `Cargo.lock`。这包括原残帧重组及去重文件的全部实现与测试。storage、GUI Mock、RS422 指令/解析和依赖文件的 `git diff --name-only` 检查无输出。

最终 `git status --short`：

```text
 M apps/desktop/src/app.rs
 M apps/desktop/src/backend.rs
 M apps/desktop/src/bridge.rs
 M crates/langmuir-core/src/checksum.rs
 M crates/langmuir-core/src/framing.rs
 M crates/langmuir-core/src/lib.rs
 M crates/langmuir-core/src/lvds.rs
 M crates/langmuir-core/src/udp_lvds.rs
 M crates/langmuir-transport/src/core_adapter.rs
 M crates/langmuir-transport/src/metrics.rs
 M crates/langmuir-transport/src/mock.rs
 M crates/langmuir-transport/src/pipeline_test.rs
 M docs/ARCHITECTURE.md
 M docs/LVDS_DATA_FORMAT_20260724.md
 M docs/MIGRATION.md
 M docs/PROTOCOL.md
 M docs/UDP_LVDS_REASSEMBLY_PROTOCOL.md
 M tools/protocol-cli/src/main.rs
?? crates/langmuir-core/src/udp_lvds_dedup.rs
?? crates/langmuir-core/src/udp_lvds_sequence.rs
?? docs/LVDS_CRC16_IMPLEMENTATION_REPORT.md
?? docs/UDP_LVDS_DEDUP_REPORT.md
?? docs/UDP_LVDS_REASSEMBLY_TTL_REPORT.md
?? docs/UDP_LVDS_SEQUENCE_QUALITY_REPORT.md
```

`git diff --stat` 为四个阶段累计的 **18 个已跟踪文件，2522 insertions、270 deletions**，不是本轮独立差异。默认统计不含未跟踪的 553 行序号模块、546 行历史去重模块和四个报告。此前未提交工作全部保留，没有提交、重置、清理或删除用户文件。

## 10. 未确认语义与停止点

| 项目 | 状态与限制 |
| --- | --- |
| 厂家序号递增、复位、重传、回绕及会话标记 | SPEC_ASSUMPTION：仅按局部模距离观察，不推断设备重启 |
| 2 秒等待、额外 5 秒历史、256 记录/步范围 | SPEC_ASSUMPTION：软件初值，尚未实网调优 |
| 等待到期原因 | 无法区分 CRC 拒收、设备行为、网络丢失等，不宣称已确认网络丢包 |
| 窗口外极迟到帧、重锚后的旧候选 | 有界历史已遗忘，无法永久关联或恢复缺口账目 |
| 重新监听后迟到旧会话包 | 外层没有会话标记，仍可能作为新会话观察；不承诺跨会话识别 |
| 全 `u32` 存储模型迁移、重排序、自动补样本 | NOT_RUN：未实施，低 `u16` 文件兼容规则保持 |
| 真机、真实网络故障注入、GUI 视觉验收 | NOT_RUN |
| 全 workspace 既有严格 Clippy 清理 | FAIL：保留为独立任务 |

本阶段止于完整科学帧序号的观察与质量统计。269B、55→AA、CRC16 算法与覆盖、两路各 125 点、24B UDP、残帧 2 秒 TTL、去重 5 秒 / 256 FIFO、RS422 和科学文件格式均保持；没有进入其他可靠性或存储迁移任务。
