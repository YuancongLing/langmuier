# UDP LVDS 残帧 TTL 与容量恢复实施报告

日期：2026-10-19。HEAD：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。

P1-1 软件验收 **PASS**：同一重组器的 64 槽残帧在 TTL 到达后自动释放，新合法帧可以继续接收，无需重启、重连或人工清空。默认 TTL 为首次分片起 2 秒。最终含 Mock 的 workspace 回归 **134 passed**，上一阶段为 118 passed。整个 workspace 严格 Clippy 仍为 **FAIL**，原因及新增代码检查见第 10 节。

本轮开始时已执行 `git status --short` 和 `git diff --stat`：存在上一阶段 CRC16 的 17 个已跟踪修改文件（1169 insertions、243 deletions）及未跟踪的 CRC16 报告。本轮在这些修改之上实施，历史 [CRC16 实施报告](LVDS_CRC16_IMPLEMENTATION_REPORT.md) 保留原样。本报告中的 Git 统计包含两阶段累计变更，不代表本轮独立差异。

状态含义：`PASS` 为实际执行通过，`FAIL` 为实际检查失败，`BLOCKED` 为环境阻断，`NOT_RUN` 为未执行。本轮没有使用旧异常 PCAP、连接真实设备、发送控制指令或执行 Git 提交。真实硬件及 GUI 验收为 `NOT_RUN`。

## 1. 原始 64 槽耗尽根因

旧 `UdpLvdsReassembler` 使用 `BTreeMap<u32, PendingFrame>`，以外层 `frame_sequence` 归属分片。每个接收器实例先过滤源端点，因此不同设备不共享此表。默认最多 64 个未完成帧；新序号在容量检查通过后分配整个声明长度的 payload、覆盖表及分片元数据。

原代码在完整覆盖且收到全部分片索引后立即移除并返回完整帧；元数据或分片冲突也会移除对应候选。另有显式 `discard()`，但生产调用方没有使用它。未完成帧没有首次接收时间、TTL 或空闲清理，分片永久丢失后会一直占槽。

当 64 个不同序号均成为残帧，所有新序号返回 `TooManyInFlightFrames`，连合法单片完整帧也不能进入。该错误不会释放旧状态，所以接收能力不能自行恢复。根因已在修改前确认并说明。

## 2. TTL 设计与选择依据

集中定义 `DEFAULT_UDP_LVDS_REASSEMBLY_TTL = Duration::from_secs(2)`；原 `new()` 和 `default()` 使用它，新增 `new_with_ttl()` 可配置非零 TTL，零值返回配置错误。该值是软件资源回收策略，不是厂家规定的超时。

预计 50ms 一帧，即 20Hz。2 秒相当于 40 个帧周期，在每周期产生一个残帧的模型下，稳态约占 40 槽，低于 64 槽并留有突发余量；也为短时网络抖动保留超过单帧周期的容忍时间。这不是对任意分片延迟的保证，超过 TTL 的未完成帧仍会被回收。

采用首次创建时间，后续有效片和相同重复片均不续期，避免重复输入无限延长残帧寿命。时间使用单调时钟 `Instant` 和 `checked_duration_since()`；不使用墙上时间，也不计算 `created_at + ttl`。注入时间早于创建时间时保留状态，不发生减法或截止时间加法溢出。

## 3. 残帧生命周期与边界

| 条件 | 行为 | 验证 |
| --- | --- | --- |
| 首个合法片、新序号且有槽 | 建立一个 `PendingFrame`，记录 `created_at`、长度、分片数、覆盖表 | PASS |
| 同序号后续片或内容相同重复片 | 按 offset 更新原状态；不更新创建时间 | PASS |
| 年龄小于 TTL | 保留状态 | PASS |
| 年龄恰好等于或大于 TTL | 完整移除状态、释放 payload/覆盖表/分片元数据，超时计数加一 | PASS |
| 分片完整且覆盖所有字节 | 立即移除并返回完整字节；不计超时 | PASS |
| 元数据、范围复用或内容冲突 | 延续原有报错和删除候选行为；计分片错误 | PASS |
| 显式 `discard()` | 移除但不计 TTL 超时 | 保留原 API |
| 过期后迟到片 | 可按原 sequence 新建状态，不继承过期字节，不污染其他 sequence | PASS |

不保留“完成待交付”槽，完整帧直接返回调用者，因此清理只涉及未完成对象。没有增加 tombstone、完整帧去重、序号复用代际判断或跨帧排序。

## 4. 自动清理、容量策略与生产链路

`push()` 使用 `Instant::now()` 调用 `push_at()`；后者先执行 `expire_at()`，然后解析和检查容量，即使新输入无效也先释放过期状态。公共 `expire()` / `expire_at()` 支持空闲清理，不增加后台线程。

清理后如果仍有 64 个未过期对象，新序号被明确拒绝并累计容量拒绝；不淘汰仍有效的帧。同一已存在序号仍可继续重组、完成并释放槽。任何过期状态都不会造成永久容量锁死。

生产路径为：

```text
桌面 Controller 现有 5ms tick
  → UdpLvdsReceiver::drain() 先清理，即使 socket 无数据
  → recv_from（保留数据报边界）
  → UdpLvdsIngress：源端点过滤、24B 头与 269B 外层长度限制
  → 核心 push_at：清理 → 容量检查 → 按 offset 重组
  → 269B 结构、CRC16、时间和双通道 ADC 严格解码
  → Controller::handle_scientific_frame
  → 原有 BatchWriter 与 ScientificSamples 显示事件
```

CLI `receive-udp-lvds` 在每次读取前以及退出汇总前调用 `expire()`；现有 100ms socket 读超时允许无新数据时回到清理，不需新线程。核心对象本身不会自动唤醒，独立调用方空闲时应调用 `expire()`。全仓调用方已检查：生产实例为桌面和 CLI，其他为核心及 transport Mock 测试；旧 `push()` API 保持兼容。

沿用 `ReaderMetricsSnapshot`、`LinkMetricsSnapshot` 和 500ms 聚合非阻塞日志，增加如下诊断：

| 快照字段 | 含义及计数规则 |
| --- | --- |
| `udp_fragment_errors` | 外层解析/长度错误和分片冲突；同时总拒收加一 |
| `reassembly_timeouts` | 被 TTL 清理的残帧；不增加科学拒收或 CRC 失败 |
| `reassembly_capacity_rejections` | 新序号因容量拒绝；同时总拒收加一，不记分片/CRC 错误 |
| `reassembled_frames` | 交付完整字节次数，包含随后被科学 CRC 拒绝的帧 |
| `reassembly_pending_frames` | 当前残帧数量，非累计值 |
| `reassembly_buffered_bytes` | 当前为残帧预留的 payload 字节，不包含元数据 |
| 原 `header_failures` / `checksum_failures` / `frames_decoded` | 继续分别表示内层帧头失败、CRC 失败、完整科学解码成功 |

核心累计计数使用饱和加法。桌面不叠加核心与 ingress 的同一分片错误；外层非法长度归 UDP 分类，不冒充内层科学长度错误。原字节流 reader 的 UDP 字段为零。只有异常增长触发聚合日志，成功数及当前缓存变化不逐帧写日志。

容量仍有明确边界：通用默认每帧至多 1MiB、1024 片、64 槽，预留 payload 至多 64MiB，覆盖表和分片元数据另受同一配置限制。桌面和 CLI 进入重组前严格限制为 269B，故残帧 payload 至多 `64 × 269 = 17216B`。该值不等于整个堆占用；测试验证的是状态数量与预留 payload，并未做进程堆分析。

## 5. 本轮修改的 Rust 文件与函数

| 文件 | 本轮直接改动 |
| --- | --- |
| `crates/langmuir-core/src/udp_lvds.rs` | TTL 常量、`PendingFrame::created_at`、`new_with_ttl()`、`push_at()`、`push_inner()`、`expire_at()` / `expire()`、`metrics()`、独立重组指标、13 项确定性测试 |
| `crates/langmuir-core/src/lib.rs` | 导出 TTL 常量与 `UdpLvdsReassemblyMetrics` |
| `apps/desktop/src/backend.rs` | `drain()` 空闲清理；ingress 的 `push_at()` / `expire_at()` / 指标同步和错误分类；`report_metric_growth()`、`ui_metrics()`；生产链恢复和诊断测试 |
| `apps/desktop/src/bridge.rs` | 现有快照增加六个 UDP 重组指标及缓存字段说明 |
| `crates/langmuir-transport/src/metrics.rs` | `ReaderMetricsSnapshot` 六字段；字节流 `snapshot()` 初始化为零，直接复用既有指标系统 |
| `tools/protocol-cli/src/main.rs` | `receive_udp_lvds()` 清理及结束/超时分类汇总；保留原严格 CRC 后写文件流程 |

同步 `UDP_LVDS_REASSEMBLY_PROTOCOL.md`、`ARCHITECTURE.md` 和 `MIGRATION.md` 的 TTL 状态与使用说明。没有改 UI 布局；桥接与 transport 的扩展仅用于将现有重组状态传递到诊断快照。

## 6. T1–T12 自动化结果

以下测试位于 `crates/langmuir-core/src/udp_lvds.rs`。均实际通过 Cargo 执行，使用注入的 `Instant` 推进时间，没有以真实 sleep 制造 TTL。

| 编号 | 测试函数 | 实际结果 | 关键证据 |
| --- | --- | --- | --- |
| T1 | `ttl_t1_single_fragment_preserves_bytes_without_pending_state` | PASS | 单片 269B 逐字节一致；完成 1；等待、缓存、超时为 0 |
| T2 | `ttl_t2_multiple_fragments_reassemble_in_order` | PASS | 三片顺序重组，完整字节一致，缓存归零 |
| T3 | `ttl_t3_interleaved_sequences_reassemble_reversed_fragments` | PASS | 两个序号交织、片内反序，正确输出两帧 |
| T4 | `ttl_t4_incomplete_frame_expires_once_and_releases_its_buffers` | PASS | 单片残帧超时移除；再次清理为 0；只计一次 |
| T5 | `ttl_t5_boundary_uses_first_fragment_and_handles_earlier_times` | PASS | TTL−1ns 保留、恰好 TTL / +1ns 清理；新片和重复片不续期；更早时间安全，零 TTL 拒绝 |
| T6 | `ttl_t6_full_capacity_rejects_new_sequence_without_evicting_live_frames` | PASS | 64 槽、17216B；第 65 序号明确拒绝；已有帧仍能完成 |
| T7 | `ttl_t7_full_capacity_automatically_recovers_on_next_datagram` | PASS | 同实例下一次 push 自动清 64 槽，再交付新合法单片；无显式清理或重启 |
| T8 | `ttl_t8_late_fragment_cannot_reuse_expired_bytes_or_pollute_other_sequences` | PASS | 过期后旧尾片不产生旧完整帧；另一序号完整帧逐字节正确 |
| T9 | `ttl_t9_twenty_hertz_reception_is_lossless_and_has_no_residual_growth` | PASS | 400 帧、50ms 注入间隔，全部严格科学解码，无残帧增长 |
| T10 | `ttl_t10_duplicates_are_idempotent_and_conflicts_release_the_frame` | PASS | 相同重复不增槽；内容冲突删除候选，仅加分片错误 |
| T11 | `ttl_t11_bad_science_crc_is_separate_from_successful_reassembly` | PASS | 重组交付坏 CRC 完整字节；科学解码拒绝，超时和分片错误均为 0 |
| T12 | `ttl_t12_repeated_capacity_pressure_keeps_resources_bounded_and_recovers` | PASS | 128 轮容量压力，8192 残帧过期、128 完成、128 容量拒绝，缓存每轮归零 |

额外 `ttl_cleanup_precedes_bad_datagram_parsing_and_counters_saturate` 为 **PASS**：坏数据报到来仍先清理；超时、完成、容量拒绝和分片错误在 `u64::MAX` 饱和，不溢出。桌面新增 3 项独立测试覆盖空闲回收、UDP 错误分类、日志聚合及成功状态静默，并扩展既有完整处理链测试。

## 7. 正常 20Hz 模拟接收

T9 向同一重组器输入 400 个合法单片黄金帧，时间为 `start + sequence × 50ms`，覆盖从 0 到 19.95 秒的模拟到达时间。每帧字节核对后均调用真实 `decode_scientific_frame()`，CRC 门禁通过，两路各 125 点。

| 指标 | 结果 |
| --- | --- |
| 完成/科学解码通过 | PASS：400 / 400 |
| 超时、容量拒绝 | PASS：均为 0 |
| 每次完成后等待帧、预留 payload | PASS：始终为 0 |
| 真实设备连续接收 20 秒 | NOT_RUN：测试为确定性时间模拟 |

## 8. 64 槽容量自动恢复证据

核心 T7 在时间 t0 创建 64 个不同序号残帧，直接在 `t0 + 2s` 调用同一对象的 `push_at(new_complete_frame)`，没有预先调用 `expire()`。实际断言：超时 64、完成 1、等待 0、缓存 0，输出为原始合法黄金帧。T6 则验证到期前的新序号拒绝以及已有序号继续完成。

桌面 `protocol_mock_recovers_full_capacity_then_delivers_only_valid_frames` 将内存数据报注入生产 `UdpLvdsIngress`：先填满 64 槽，确认第 65 个新序号被拒绝；推进到 TTL 后，输入倒序的两片合法帧、另一合法帧和一帧坏 CRC。随后使用真实 `Controller::handle_scientific_frame()`、`BatchWriter` 内存 sink 和显示事件队列核对。

| 生产 ingress 集成断言 | 实际值 | 状态 |
| --- | ---: | --- |
| 超时回收 | 64 | PASS |
| 容量拒绝 | 1 | PASS |
| 完整重组 | 3 | PASS |
| 科学解码/正常 handler/存储帧/显示事件 | 各 2 | PASS |
| CRC 失败 | 1，计算 `44C8` / 接收 `44C9` | PASS |
| 总拒收 | 2：容量 1 + CRC 1 | PASS |
| UDP 分片错误、最终等待、最终缓存 | 均为 0 | PASS |

T12 重复 128 轮上述容量压力，始终不超过 64 槽及 17216B payload，完成帧持续可接收。它验证有界状态及计数，不替代长期真实网络或内存分析。

生产 socket 接入已通过代码核查，workspace 内既有本机 UDP 回环测试实际通过；本轮完整 TTL→存储/显示证据来自生产 ingress 的内存数据报注入。真实桌面 socket 端到端 TTL 故障注入、真实硬件和 GUI 观察均为 `NOT_RUN`。

## 9. CRC16、RS422 与 Cargo 回归

工具链为 `rustc 1.99.0 (b940084d7 2026-09-28)`。没有修改 Cargo.toml、Cargo.lock、依赖版本或删除测试。

| 命令/验证 | 实际结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test -p langmuir-core --locked` | PASS：53 tests |
| `cargo test -p langmuir-protocol-cli --locked` | PASS：5 tests |
| `cargo test -p langmuir-desktop --features mock --locked` | PASS：50 tests |
| `cargo test --workspace --locked` | PASS：131 tests，core 53 / desktop 47 / CLI 5 / storage 9 / transport 17 |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS：134 tests，desktop 50，其他同上；收尾复核再次通过 |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| 269B 黄金帧、`44C8/5EF9`、变异 CRC 拒绝与原范围保护 | PASS：原 core / CLI / transport 回归继续通过 |
| 六条 RS422 黄金指令、SUM、遥测与流式恢复 | PASS：原 core / CLI 回归继续通过 |
| 真实设备或旧 PCAP 验收 | NOT_RUN |

含 Mock 总数由 118 增为 134：核心新增 13 项，桌面新增 3 项，并扩展原有处理链测试。全部既有测试保留。workspace 测试含既有 localhost UDP 绑定，使用沙箱外本机回环权限执行；没有连接真实设备，也没有以独立 harness 代替正式 Cargo 测试。

269B 布局、`55 → AA`、`0107`、TYPE=`AA`、CRC16 `1021/FFFF`、覆盖 `frame[7..267]` 和尾部大端值均保持。厂家 CRC 实际调用范围等原待确认项继续见 CRC16 报告，不因 TTL 测试而变成硬件确认。

## 10. Clippy 实际状态与新增代码

`cargo clippy --workspace --all-targets --locked -- -D warnings` 实际退出 101，**FAIL**。严格运行直接报告下列既有问题；本轮实施前的同命令也报告相同代码和 lint：

| 文件 | 当前位置 | 既有问题与证据 |
| --- | --- | --- |
| `tools/protocol-cli/src/main.rs` | 294 | `parse_hex()` 的 `.chunks_exact(2)`，`chunks_exact_to_as_chunks`；本轮前为 277 行，同一代码 |
| `crates/langmuir-storage/src/binary.rs` | 143、146 | 两路 `.iter()`，`explicit_iter_loop`；本文件无差异 |
| `crates/langmuir-core/src/framing.rs` | 289、291 | 原 RS422 空集合断言，`assert_is_empty`；本轮未修改 |

实施前另用命令行放宽上述三类检查，已确认既有 `app.rs` 还存在 `too_many_lines`、`semicolon_if_nothing_returned`、`match_result_ok`、`uninlined_format_args`、`if_not_else`、`manual_clamp`、`items_after_statements`、`assertions_on_constants`。`app.rs` 本轮未修改。实施前诊断记录为临时文件 `/private/tmp/langmuir-ttl-clippy-baseline.md`，关键证据在本节保留，不依赖其长期存在。

本轮新增桌面边界测试曾报 `unchecked_time_subtraction`，已改用 `Instant::checked_sub()`，没有为此禁用 lint。最终以下含 Mock 诊断实际 **PASS**，未发现其他新增诊断：

```text
cargo clippy --workspace --all-targets --features langmuir-desktop/mock --locked -- -D warnings
  -A clippy::chunks_exact_to_as_chunks -A clippy::explicit_iter_loop
  -A clippy::assert_is_empty -A clippy::too_many_lines
  -A clippy::semicolon_if_nothing_returned -A clippy::match_result_ok
  -A clippy::uninlined_format_args -A clippy::if_not_else
  -A clippy::manual_clamp -A clippy::items_after_statements
  -A clippy::assertions_on_constants
```

上面换行只为阅读，复现时应合并为同一命令。它放宽的是已知既有类别，并非所有 lint 的严格证明，不能替代 workspace 严格 **FAIL**。工程 lint 配置没有修改，也没有因既有 lint 扩大到 UI、RS422 或存储清理。

## 11. Git 变更清单与范围

本轮直接变更为第 5 节六个 Rust 文件、三份同步文档和本报告。`app.rs`、`checksum.rs`、`framing.rs`、`lvds.rs`、`core_adapter.rs`、transport `mock.rs` / `pipeline_test.rs`、`LVDS_DATA_FORMAT_20260724.md`、`PROTOCOL.md` 的已有差异来自上一阶段 CRC16，本轮予以保留；历史 CRC16 报告未重写。

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
?? docs/LVDS_CRC16_IMPLEMENTATION_REPORT.md
?? docs/UDP_LVDS_REASSEMBLY_TTL_REPORT.md
```

`git diff --stat`（累计，两个未跟踪报告不计入）：

```text
 apps/desktop/src/app.rs                        |  20 +-
 apps/desktop/src/backend.rs                    | 673 +++++++++++++++++++++----
 apps/desktop/src/bridge.rs                     |  13 +
 crates/langmuir-core/src/checksum.rs           |  40 +-
 crates/langmuir-core/src/framing.rs            |  39 +-
 crates/langmuir-core/src/lib.rs                |   8 +-
 crates/langmuir-core/src/lvds.rs               | 280 ++++++++--
 crates/langmuir-core/src/udp_lvds.rs           | 452 ++++++++++++++++-
 crates/langmuir-transport/src/core_adapter.rs  | 134 +++--
 crates/langmuir-transport/src/metrics.rs       | 116 +++++
 crates/langmuir-transport/src/mock.rs          |  91 +++-
 crates/langmuir-transport/src/pipeline_test.rs |  28 +-
 docs/ARCHITECTURE.md                           |  15 +-
 docs/LVDS_DATA_FORMAT_20260724.md              |  42 +-
 docs/MIGRATION.md                              |  20 +-
 docs/PROTOCOL.md                               |  34 +-
 docs/UDP_LVDS_REASSEMBLY_PROTOCOL.md           |  78 ++-
 tools/protocol-cli/src/main.rs                 |  86 +++-
 18 files changed, 1908 insertions(+), 261 deletions(-)
```

`git diff --name-only` 对 Cargo.toml、Cargo.lock、storage 实现、桌面 GUI Mock、RS422 command / parser 的检查无输出。没有提交、重置、清理或删除用户文件。

## 12. 剩余问题与本阶段停止点

| 剩余工作 | 本轮状态与边界 |
| --- | --- |
| 完整帧去重 | NOT_RUN：未实现，重复完整帧仍可能再次交付 |
| 跨帧乱序、序号缺口检测 | NOT_RUN：未实现；本轮乱序仅指同帧分片 |
| 完整保留 UDP `u32` 序号 | NOT_RUN：未改模型/存储，桌面仍截为 `u16` |
| 同 sequence 复用代际隔离 | NOT_RUN：无 tombstone；迟到片可新建原序号残帧，但不能继承已释放字节 |
| 原始异常 wire 保存、科学 CSV、位同步/纠偏 | NOT_RUN：未开展 |
| TTL 在真实设备、极端抖动和长期负载下调优 | NOT_RUN：当前 2s 为可配置软件初值 |

本轮止于残帧 TTL、受控容量拒绝和自动恢复交付，不自动进入后续 UDP 可靠性开发。
