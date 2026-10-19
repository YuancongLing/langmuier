# LVDS P1-2 完整科学帧去重与重复交付防护报告

日期：2026-10-19。HEAD：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。

软件回归 **PASS**：同一来源、同一接收会话中，缓存仍保留的同序号同字节合法科学帧只进入正常回调一次；重复单片和重复分片组均不产生重复存储与 GUI 科学数据事件。最终含 Mock workspace **149 passed**，较 P1-1 的 134 项新增 15 项。整个 workspace 严格 Clippy 仍为 **FAIL**；新增代码检查与基线证据分列于第 8 节。

默认去重缓存为 **5 秒、256 帧、FIFO**，使用完整 UDP `u32` 序号和全部 269B。缓存过期或容量淘汰后允许再次接受历史帧，因此这是有界的近期重复防护，不是永久 exactly-once 存储保证。

状态：`PASS` 为实际执行通过，`FAIL` 为实际失败，`BLOCKED` 为环境阻断，`NOT_RUN` 为未执行；`SPEC_ASSUMPTION` 标记未由厂家定义的会话解释。本轮未连接硬件、使用历史异常 PCAP、发送控制指令或执行 Git 提交。

## 1. 开始前检查与原始根因

首先执行的 `git status --short` / `git diff --stat` 显示：已有 CRC16 和 TTL 的 18 个已跟踪修改文件（1908 insertions、261 deletions），以及两个未跟踪历史报告。本轮在原工作区上增量实施，未重写历史报告。

审计确认：`UdpLvdsReassembler` 完成覆盖检查后立即从残帧表删除并返回完整原始字节，不保存完成历史。相同单片或一整组分片再次到达，会重新完成重组。原桌面 ingress 随后只要科学解码成功就调用回调，经 `Controller::handle_scientific_frame()` 到存储和显示队列；CLI 同样会再次写 raw/index。原重组阶段对“相同重复片”的容忍只覆盖尚未完成的同一候选，不能阻止完整帧再次交付。

外层有完整 `frame_sequence: u32`，而 `ScientificFrame.sequence` 仍为既有 `u16`。故以解码后模型序号去重会误伤低 16 位相同的不同帧。文档只定义大端 `u32`，未找到厂家关于复位、回绕或重连复用的规则。

## 2. 去重位置、Key 与合法性门禁

新增独立 `UdpLvdsScienceGate`，桌面和 CLI 共用，不把“已接受”状态混入残帧重组器：

```text
UDP recv_from → 来源过滤 → 原 24B 解析 → 原分片重组/2s TTL
  → 269B 结构 → CRC16 → 时间 → 两路 ADC 完整科学解码
  → 完整 u32 + 原始 269B 去重/冲突判定
  → Accepted：登记历史并返回 ScientificFrame
  → 正常处理回调 → Controller → 原存储与 GUI 事件队列
```

每个 gate 实例绑定一个经调用方过滤的源端点和接收会话。等价 Key 为 `(receiver session, source IP/port, full u32 sequence)`；实例内部只需保存完整 `u32`，来源过滤在查询前完成。原始 269B 逐字节比较，不以 CRC 值、低 16 位或内容哈希代替完整内容。

`accept_at(&ReassembledLvdsFrame, Instant)` 内部调用原严格科学解码器，成功后才查询和登记历史。结构、CRC、时间或 ADC 错误直接返回原 `FrameError`，不登记，也不增加科学验证成功数。即使相同序号已有记录，坏 CRC 仍按 CRC 错误处理，不被伪装成普通重复。

结果通过 `UdpLvdsScienceResult` 返回：只有 `Accepted` 的 `into_frame()` 为 `Some(ScientificFrame)`；重复和冲突为 `None`，不能经此接口进入正常 handler。

## 3. 缓存策略与序号冲突

| 场景 | 行为 | 验证 |
| --- | --- | --- |
| 首次合法序号 | 登记完整字节和首次接受 `Instant`，交付一次 | PASS |
| 缓存内同序号、相同全部字节 | `Duplicate`，只计抑制，不交付，不刷新时间或 FIFO 顺序 | PASS |
| 缓存内同序号、不同合法字节 | `SequenceConflict`，独立计数并拒绝，不替换首条内容、不续期 | PASS |
| 不同完整序号、相同字节 | 分别接受，不做内容全局去重 | PASS |
| 原记录年龄小于 5 秒 | 保留记录 | PASS |
| 年龄恰好等于或超过 5 秒 | 删除记录，之后该序号可重新接受 | PASS |
| 新合法帧进入且已满 256 条 | 淘汰最早接受的一个记录，再登记新记录 | PASS |
| 无效科学帧 | 不分配历史记录，不淘汰仍在有效期的记录 | PASS |

默认值集中为 `DEFAULT_UDP_LVDS_DEDUP_TTL` 和 `DEFAULT_UDP_LVDS_DEDUP_CAPACITY`；`new(capacity, ttl)` 支持非零配置，零值返回配置错误。5 秒约覆盖 100 个正常 50ms 周期，256 条容量为短时突发留余量。这是软件初值，不是厂家保证的重传窗口。

缓存使用 `VecDeque`，条目保存完整 `u32`、原始字节和首次接受时间。最多保留 `256 × 269 = 68864B` 原始 payload，元数据及集合占用另受 256 条上限限制；该值不是进程总内存测量。清理使用 `Instant::checked_duration_since()`，不依赖墙上时间或截止时间加法；更早的注入时间不会错误过期。清理遍历有界队列，不假定测试时钟总按时间顺序注入。

新输入前以及桌面/CLI 原有空闲轮询都会清理 gate，无新增线程。它的 5 秒历史与重组器 2 秒残帧 TTL 独立；后者源码保持逐字节不变。

## 4. 重连、设备复位与回绕边界

厂家会话/复位语义为 `SPEC_ASSUMPTION`，采用以下保守规则：

- 桌面重新监听先销毁旧 receiver/ingress，再创建新的 gate 和重组器；CLI 每次采集调用也创建独立 gate。旧会话历史不造成新会话永久误拒收。
- 采集开始/停止不等于重连，保留当前接收会话历史，避免启停后立即重复交付旧包。
- 不把序号变小自动解释成设备重启，不清空历史、不排序、不维护跨帧连续性。
- `u32::MAX → 0` 是不同完整键；零号未被缓存时正常接受。短期复用仍在历史中的序号，按重复/冲突规则暂时抑制，原记录满 5 秒后解除，重复和冲突都不能无限续期。
- 外层没有设备会话标记，显式重连后无法区分迟到旧会话包与新包；也无法在同会话中证明相同完整字节的复用一定是新帧。窗口外、淘汰后或新会话中的旧包可再次接受。

没有新增序号回绕推测算法、跨帧重排序、缺口补偿或存储序号升级。去重始终用完整 `u32`；返回科学模型时仍按原兼容规则保留低 `u16`，文件格式未改。

## 5. 指标与聚合日志

| 指标 | 精确定义 |
| --- | --- |
| `reassembled_frames` | 所有完整重组字节候选，包括随后科学验证失败、重复或冲突的帧 |
| `frames_decoded` | 复用为全部科学验证成功数，含 CRC16、结构、时间、ADC 全部成功，包括合法重复和冲突；不是单独的 CRC-only 计数 |
| `duplicate_frames_suppressed` | 缓存内同序号同字节的抑制次数 |
| `sequence_conflicts` | 同序号不同合法字节的拒绝次数 |
| `scientific_frames_delivered` | 门禁返回 Accepted 后调用正常回调的次数 |
| `dedup_cached_frames` / `dedup_cached_bytes` | 当前已接受历史条目和原始 payload 字节，非累计值 |
| 原 `checksum_failures` / `reassembly_timeouts` / `reassembly_capacity_rejections` | 分别仍表示科学 CRC 失败、2 秒残帧回收、64 槽容量拒绝 |

重复不增加总拒收或 CRC 失败；合法序号冲突增加总拒收和独立冲突计数，不计 CRC 错误。错误 CRC 不增加冲突或去重状态。累计门禁计数使用饱和加法。

沿用现有 500ms 快照、`LinkMetricsSnapshot` 和非阻塞聚合日志，增加重复/冲突的增长项；不逐帧写日志，成功数和缓存 gauge 变化保持静默。字节流 reader 的新增 UDP 去重字段为零。

`scientific_frames_delivered` 不是写盘完成回执：停止采集时正常 handler 会忽略该帧，存储/显示队列也有既有压力与错误机制。登记发生在回调前，本阶段不因后续存储失败自动撤销登记或重放。测试在采集运行且队列可用时另行核对实际存储和 GUI 事件。

## 6. 自动化测试结果

新增核心测试位于 `crates/langmuir-core/src/udp_lvds_dedup.rs`，均通过正式 Cargo 执行，使用注入 `Instant`，没有长时间 sleep。

| 新增核心测试 | 状态 | 核心断言 |
| --- | --- | --- |
| `first_valid_frame_is_delivered_and_exact_repeats_are_suppressed` | PASS | 首次交付 1，10 次重复只计抑制，黄金 ADC 不变 |
| `bad_crc_does_not_register_or_replace_accepted_state` | PASS | 坏 CRC 先到缓存为空，同序号正确帧可接受；后到坏帧不替换历史 |
| `legal_sequence_conflict_keeps_the_original_record_without_refreshing_it` | PASS | 合法不同时间字节为冲突，原内容仍是重复，5 秒后新内容可接受 |
| `equal_content_with_distinct_full_u32_sequences_is_not_a_duplicate` | PASS | `1/65537/2/65538` 同内容分别接受，不按低 16 位合并 |
| `four_hundred_frames_at_fifty_millisecond_intervals_are_all_delivered` | PASS | 400 帧全部通过，重复/冲突为零，结束时历史 100 条 |
| `capacity_evicts_oldest_acceptance_and_duplicates_do_not_refresh_fifo` | PASS | 小容量验证 FIFO，重复不使旧记录变新，淘汰后允许再次接受 |
| `default_capacity_remains_bounded_under_repeated_eviction` | PASS | 同时刻 1024 帧压力，始终不超过 256 条、68864B payload |
| `ttl_boundary_is_exact_and_duplicate_does_not_extend_lifetime` | PASS | 5 秒−1ns 仍重复，恰好边界清理，超过边界也清理，无续期 |
| `earlier_time_is_safe_and_expiry_does_not_assume_timestamp_order` | PASS | 时间逆序安全，不漏清队列中已到期的记录 |
| `fresh_sessions_sequence_reset_and_u32_wrap_allow_new_science` | PASS | 新对象、短期复用、到期解除及 MAX→0 均符合策略 |
| `invalid_structure_time_and_adc_never_poison_the_cache` | PASS | 错头、有效 CRC 的非法时间/ADC 不缓存；同序号合法帧随后成功 |
| `configuration_requires_nonzero_capacity_and_ttl_and_counters_saturate` | PASS | 拒绝零配置，累计计数在 `u64::MAX` 饱和 |

桌面新增 3 项测试均为 **PASS**：

| 测试 | 证据 |
| --- | --- |
| `udp_science_dedup_distinguishes_invalid_duplicate_conflict_and_full_sequence` | 坏 CRC→同序号正确重传、重复、合法冲突、7 与 65543 完整键区分、不同来源隔离；6 重组、5 验证成功、3 回调、重复 1、冲突 1、CRC 1 |
| `udp_science_dedup_reconnect_wrap_and_expiry_do_not_permanently_block_frames` | 同一 ingress MAX→0 正常；新 ingress 无旧历史；5 秒后同序号复用解除，重组超时仍为零 |
| `udp_science_dedup_twenty_hertz_frames_remain_lossless_and_bounded` | 真实生产 ingress 接收 400 个完整序号，50ms 注入间隔，全部回调，无误拒收或 TTL 失败 |

另扩展既有 `protocol_mock_recovers_full_capacity_then_delivers_only_valid_frames` 和日志聚合测试，未删除旧测试。前者在同一生产 ingress 中先填满 64 槽并验证容量拒绝，时间推进到原 2 秒 TTL 后输入重复多片组、重复单片及坏 CRC，再经真实 `Controller`、`BatchWriter` 内存 sink 与显示队列核对：

| 集成观测 | 实际值 | 状态 |
| --- | ---: | --- |
| 原残帧超时 / 容量拒绝 | 64 / 1 | PASS |
| 完整重组 / 科学验证成功 | 5 / 4 | PASS |
| 重复抑制 / 序号冲突 / CRC 错误 | 2 / 0 / 1 | PASS |
| 正常回调 / 存储帧 / GUI `ScientificSamples` | 各 2 | PASS |
| 总拒收 | 2，容量 1 + CRC 1 | PASS |
| 最终残帧 / 残帧 payload | 0 / 0 | PASS |

每个存储帧的两路 125 点、独立绝对基准和两帧首点 50ms 间隔继续核对。日志测试证明重复/冲突在单次快照内聚合且相同快照不重复输出；新增指标通过 UI 桥接，未更改布局。

## 7. 回归命令与验证边界

工具链：`rustc 1.99.0 (b940084d7 2026-09-28)`。

| 命令/验证 | 实际状态 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test -p langmuir-core --locked` | PASS：65 tests |
| `cargo test -p langmuir-desktop --features mock --locked --offline` | PASS：53 tests |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS：最终 149 tests，core 65 / desktop 53 / CLI 5 / storage 9 / transport 17；引用调整后再通过 |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| 原 TTL T1–T12、2 秒边界和 128 轮 64 槽压力 | PASS：原核心测试全部保留并通过 |
| 269B、CRC `[7..267]`、`44C8/5EF9` 黄金帧及坏 CRC 拒收 | PASS：原核心、CLI、transport 回归继续通过 |
| RS422 六条黄金指令、SUM 与遥测解析 | PASS：原测试继续通过 |
| 真实桌面 socket 端到端重复注入及 CLI socket→文件专项 | NOT_RUN：本轮完整存储/GUI证据使用生产 ingress 内存注入 |
| 真实设备、历史异常 PCAP、GUI 截图/交互 | NOT_RUN |

总数 `134 + 核心新增12 + 桌面新增3 = 149`，无删除测试。workspace 中既有 localhost UDP 测试实际执行并通过，使用沙箱外的本机回环权限，没有访问真实设备。

400 帧测试在核心和生产 ingress 各执行一次，模拟到达时间从 0 到 19.95 秒；不是实际设备连续运行 20 秒。完整字节流不含外层可靠 `u32` 标签，本阶段没有为旧字节流或直接 GUI Mock 注入虚构 UDP 去重语义。

## 8. Clippy：工作区严格状态与新增代码

`cargo clippy --workspace --all-targets --locked -- -D warnings` 实际 **FAIL**，退出 101。本轮实施前和实施后均实际执行；首批错误为相同既有代码：

| 文件与当前位置 | 既有诊断 | 本轮前证据 |
| --- | --- | --- |
| `tools/protocol-cli/src/main.rs:311` | `parse_hex()` 的 `.chunks_exact(2)`，`chunks_exact_to_as_chunks` | 开始前同代码在 294 行并报错 |
| `crates/langmuir-storage/src/binary.rs:143,146` | `explicit_iter_loop` | 文件无差异，开始前同诊断 |
| `crates/langmuir-core/src/framing.rs:289,291` | 原 RS422 空集合断言，`assert_is_empty` | 本轮前后 SHA-256 一致、同诊断 |

另有既有 `app.rs` 的过长函数、缺分号、`match_result_ok`、格式参数、`if_not_else`、`manual_clamp`、`items_after_statements`、常量断言等 lint；该文件本轮 SHA-256 保持一致。

新增代码检查：core lib 严格 Clippy **PASS**；core all-target 仅在命令行放宽既有 `assert_is_empty` 后 **PASS**。新增边界测试的未检查时间减法已改 `checked_sub()`。桌面新增字段使快照从 224B 增为 264B，触发的 `large_types_passed_by_value` 已通过将 `report_metric_growth()` / `ui_metrics()` 参数改为引用修复，没有禁用该 lint。另保留 `too_many_lines` 检查时仅报 `app.rs` 五个既有函数，backend 新增/扩展函数无该诊断。

最终含 Mock 的以下诊断 **PASS**，未发现其他新增诊断；它只为排查新增问题放宽已知既有类别，不替代严格 **FAIL**：

```text
cargo clippy --workspace --all-targets --features langmuir-desktop/mock --locked -- -D warnings
  -A clippy::chunks_exact_to_as_chunks -A clippy::explicit_iter_loop
  -A clippy::assert_is_empty -A clippy::too_many_lines
  -A clippy::semicolon_if_nothing_returned -A clippy::match_result_ok
  -A clippy::uninlined_format_args -A clippy::if_not_else
  -A clippy::manual_clamp -A clippy::items_after_statements
  -A clippy::assertions_on_constants
```

上面换行仅为阅读，复现时合并为一条命令。工程 lint 配置、依赖、Cargo.toml 和 Cargo.lock 未修改，没有通过全局关闭 lint 掩盖失败。

## 9. Git 变更与此前修改保护

本轮直接修改六个 Rust 文件（含一个新增模块）、三份当前说明及本报告：

| 文件 | 本轮职责 |
| --- | --- |
| `crates/langmuir-core/src/udp_lvds_dedup.rs`（新增） | 科学验证后去重门禁、完整键、FIFO/生命周期及 12 项测试 |
| `crates/langmuir-core/src/lib.rs` | 导出 gate、结果/指标类型和配置常量 |
| `apps/desktop/src/backend.rs` | 生产 ingress 门禁、空闲清理、指标/聚合日志、引用参数、生产链测试 |
| `apps/desktop/src/bridge.rs` | 新增五个 UDP 去重指标与回调语义说明 |
| `crates/langmuir-transport/src/metrics.rs` | 扩展现有快照，字节流新字段初始化为零 |
| `tools/protocol-cli/src/main.rs` | 同门禁后才写 raw/index 或累计目标数；结束/超时分类统计 |
| `docs/UDP_LVDS_REASSEMBLY_PROTOCOL.md`、`ARCHITECTURE.md`、`MIGRATION.md` | 同步已完成去重与有限会话/缓存边界 |
| `docs/UDP_LVDS_DEDUP_REPORT.md`（新增） | 本报告 |

前后实际 SHA-256 比较一致的 13 个保护文件：`app.rs`、核心 `checksum.rs/framing.rs/lvds.rs/udp_lvds.rs/command.rs/rs422.rs`、transport `core_adapter.rs/mock.rs/pipeline_test.rs`、两个历史 CRC16/TTL 报告及 `Cargo.lock`。这包括原 2 秒 TTL 的整个实现和测试文件，确认未被本阶段重写。storage 实现和桌面 GUI Mock 的 `git diff --name-only` 也无输出。

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
?? docs/LVDS_CRC16_IMPLEMENTATION_REPORT.md
?? docs/UDP_LVDS_DEDUP_REPORT.md
?? docs/UDP_LVDS_REASSEMBLY_TTL_REPORT.md
```

`git diff --stat` 为三个阶段累计的 **18 个已跟踪文件，2175 insertions、268 deletions**，不是本轮独立差异。Git 默认未包含新增的 546 行核心模块及三个未跟踪报告，故不能把默认统计当作全部交付量。历史 CRC16/TTL 报告保留其原阶段结论，没有覆盖旧记录。

没有执行提交、重置、清理或删除用户文件。

## 10. 遗留问题与停止点

| 项目 | 本轮状态 |
| --- | --- |
| 厂家序号复位/回绕/会话标识定义 | SPEC_ASSUMPTION：采用接收器会话隔离及有界历史，仍需厂家定义 |
| 窗口外、容量淘汰后、显式重连后的重复包 | 有意允许重新接受；不承诺永久唯一或跨会话去重 |
| 跨帧排序、序号缺口检测与自动补样本 | NOT_RUN：未实现 |
| 完整 `u32` 科学模型/存储格式 | NOT_RUN：只在门禁保留完整键，原存储低 `u16` 规则不变 |
| 异常 wire 保存、位错纠正、科学 CSV | NOT_RUN：未开展 |
| 真实网络重传窗口与容量参数调优 | NOT_RUN：当前为可配置的软件初值 |
| 严格 workspace Clippy 基线清理 | FAIL：留作独立工作，未扩大本任务 |

本阶段止于完整科学帧的近期重复交付防护；CRC16、269B、24B 外层、两路 125 点解码、残帧 2 秒 TTL、RS422、存储格式和 UI 布局保持。没有自动进入序号连续性或其他后续开发。
