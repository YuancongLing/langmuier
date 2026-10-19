# LVDS P1-4：完整 UDP u32 序号追溯与存储兼容性设计审计

日期：2026-10-20。审计 HEAD：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。行号均指本次工作区中的源码，而非 HEAD 未修改版本。

**建议优先选择 B：保留现有 `.lpd` v2 / HDF5 科学数据布局，增加独立版本化的序号与质量索引，并由同一个 writer 协调科学数据、索引和提交状态。** 此建议降低既有文件与外部消费者的迁移风险，但不承诺两个文件天然原子，也不把增加一份 CSV 等同于可靠追溯。A 的原生 u32 新格式适合作为后续单独批准的升级路线。

本轮静态审计 **PASS**；存储实现、格式升级、新增测试执行均为 **NOT_RUN**。仅新增本报告，未修改 Rust、既有测试、协议、文件格式、Cargo.lock 或历史报告。P1-3 的 Mock workspace **170 passed** 和严格 Clippy **FAIL** 为此前已记录基线，本轮未重新执行，不能当作新方案的验收结果。

状态含义：`PASS` 为本轮实际完成的检查，`FAIL` 为实际失败，`BLOCKED` 为检查受阻，`NOT_RUN` 为未执行；`SPEC_ASSUMPTION` 为厂家尚未定义或需要批准的软件设计假设。下文“建议”“拟议”均不代表已实现。

## 1. 初始状态与审计方法

首先实际执行 `git status --short`、`git diff --stat`。存在四阶段成果：18 个已跟踪修改文件，累计 **2522 insertions、270 deletions**；另有两个未跟踪核心模块及四份历史报告。没有提交、重置、清理或覆盖这些改动。

采用源码追踪、全仓符号搜索和只读行号核对；对开始时发现的 63 个 Rust、Cargo、Markdown 等现有文件记录 SHA-256，交付前再逐项比较。未读取旧异常 PCAP、未连接设备、未启动 GUI，也未打开或改写实际科学数据文件。

## 2. 序号全链路追踪

### 2.1 接收、门禁与模型边界

| 环节 / 结构 | 当前序号与关联信息 | Rust 文件、函数与行号 |
| --- | --- | --- |
| 24B UDP 头 | `UdpLvdsHeader.frame_sequence: u32`；从数据报 `[4..8]` 大端解析 | `crates/langmuir-core/src/udp_lvds.rs:17`，`parse_udp_lvds_datagram():210`，读取位置 `:232` |
| 残帧表 | 以完整 u32 归属分片；不按低 16 位合并 | 同文件 `UdpLvdsReassembler::push_inner():395`，查找 `:420`、插入 `:433` |
| 重组完成 | `ReassembledLvdsFrame { frame_sequence: u32, bytes }`，保留完整序号与 269B；没有来源、会话 ID 或接收时间字段 | 同文件结构 `:38`，完成返回 `:459` |
| ScienceGate 缓存 | `AcceptedFrame.sequence: u32`，比较完整序号和全部原始字节，首次接受时间仅存内存 | `crates/langmuir-core/src/udp_lvds_dedup.rs:58`，`accept_at():127`，比较 `:145`、`:147`，登记 `:164` |
| **截断点** | `frame_sequence.to_be_bytes()` 丢弃前两个字节，只将低 16 位传给科学解码器 | 同文件 `UdpLvdsScienceGate::accept_at():135`、`:138` |
| ScienceGate 返回值 | `UdpLvdsScienceResult` 只含 disposition 和 `Option<ScientificFrame>`，不携带完整外层标签 | 同文件结构 `:25`，`into_frame():37`，Accepted 返回 `:170` |
| SequenceMonitor | 使用原重组对象的完整 u32；内部 frontier / gaps 也为 u32 | `apps/desktop/src/backend.rs`，`UdpLvdsIngress::push_at():201`；`crates/langmuir-core/src/udp_lvds_sequence.rs:82`、`observe_at():91` |
| 科学模型 | `ScientificFrame.sequence: u16`；时间与两路 ADC 在同一个对象中 | `crates/langmuir-core/src/lvds.rs:44`；`decode_scientific_frame_with_sequence():125` 参数 `:127`、返回 `:210` |
| ingress 回调 | 观察完整序号后只回调 `ScientificFrame`；外层标签、源地址和会话归属未进入正式处理参数 | `apps/desktop/src/backend.rs`，`UdpLvdsIngress::push_at():199` 至回调 `:205` |
| Controller | 中间 `Vec<ScientificFrame>` 保持回调顺序，再逐帧交给 handler；此处已无完整序号 | 同文件 `drain_udp_lvds():1037`，处理 `:1051`；`handle_scientific_frame():1091` |
| 摘要模型 | `ScientificFrameSummary.sequence` 也仅为 u16，没有补充完整标签 | `crates/langmuir-core/src/model.rs:34`，`DeviceState::apply_scientific_summary():58` |

因此，**完整 u32 的丢失发生在正式科学模型与回调接口之前，不是 BatchWriter 又截掉了一次**。完整标签短期仍在重组对象、去重缓存和监测内部存在，但这些内存状态没有转为每个存储记录的持久元数据。[证据：上述 `accept_at():135`、模型 `lvds.rs:46`、回调 `backend.rs:205`。]

`1` 与 `65537` 都会成为 `ScientificFrame.sequence == 1`。这不破坏当前使用完整 u32 的 P1-2 / P1-3 判断，却使下游仅凭科学模型和科学文件无法区分两者。既有桌面测试直接证明同类情形：`udp_science_dedup_distinguishes_invalid_duplicate_conflict_and_full_sequence()` 在 `apps/desktop/src/backend.rs:2007`，`:2039` / `:2040` 的两个正式帧序号都为 7，而外层分别为 7 / 65543。

### 2.2 时间、来源与会话

UTC 与微秒来自同一完整科学帧：`Timestamp` 在 `crates/langmuir-core/src/lvds.rs:31` 保存两个 u32；`decode_scientific_frame_with_sequence():178` 读取原始整数，`:192` 检查微秒，再在 `:210` 与低 u16 序号放入同一对象。因此当前异步路径不会把一帧的低序号与另一帧的时间拆开传递；问题是高位和外层身份没有进入该对象。

两路同用该基准时间，样本时间按 400μs 步进计算（`lvds.rs`，`ScientificFrame::channel_1_base_seconds():53`、`channel_2_base_seconds():58`、`channel_1_sample_time():63`）。时间不是序号的编码，也没有厂家保证“UTC + 微秒”可唯一确定完整序号。时钟重复、复位或异常都不能靠此排除。

当前科学 CRC16 覆盖 `frame[7..267]`，包含科学时间与 DATA，不含外层 UDP u32（`lvds.rs:16`、`:18`、`decode_scientific_frame_with_sequence():176`；外层另由 `udp_lvds.rs:232` 读取）。CRC 通过不意味着已经校验了“这个外层序号与科学内容”的不可替换绑定；保存索引时仍需明确配对和完整性规则。

每个 ingress 实例过滤一个源端点并拥有独立 gate / monitor（`backend.rs`，`UdpLvdsIngress::new():144`、`push_at():166` 附近）。重新监听先 disconnect，再创建 receiver（`bind_lvds_udp():616`、`:636`），旧实例在 `disconnect():675` 被销毁；采集启停只建立或关闭 writer（`start_acquisition():699`、`stop_acquisition_with_reason():779`），不重建接收会话。目前没有可落盘的显式接收会话 ID。

传入 gate / monitor 的 `now` 是本次完成数据报处理的 `Instant`（`UdpLvdsIngress::push():154`、`push_at():190`、`:201`），不是设备 UTC、首片时间或内核收包时间。未来索引应命名为“重组完成/接受处理时间”，不能事后标成未采集的首包时间。

### 2.3 异步写入与显示

| 边界 | 当前实际语义 | Rust 证据 |
| --- | --- | --- |
| Accepted / 观察 | 发生在 handler 之前；非采集期间也能观察合法帧 | `backend.rs`，`UdpLvdsIngress::push_at():201`；`handle_scientific_frame():1092` 直接忽略非采集帧 |
| 入队成功 | `submit_timeout()` 移动同一个 `T`，不改序号或时间；成功仅代表有界队列接纳 | `crates/langmuir-storage/src/service.rs`，`BatchWriter::submit_timeout():241`、成功分支 `:248` |
| batch 顺序 | 单 writer 按输入队列顺序将 item 放入 batch，再原样交 sink，不按序号排序 | 同文件 worker 输入分支 `:515`、`drain_input():557`、`write_batch():577` |
| sink 写入成功 | `written_items` / `BatchWritten` 在 sink API 成功返回后增加，尚不代表后续 flush 或断电持久化成功 | 同文件 `write_batch():589`、`:598`；`flush_all():619` |
| GUI 事件 | 入队成功后发送时间和数组，不含低序号、完整序号、会话或存储行号 | `backend.rs`，`handle_scientific_frame():1099`、`:1121`、`:1128`；`apps/desktop/src/bridge.rs`，`BackendEvent::ScientificSamples:176` |
| GUI 点缓存 | 变为时间/值点，不保留帧身份；时间回退会清空当前绘图窗口 | `apps/desktop/src/app.rs`，`handle_scientific_samples():618`；`apps/desktop/src/buffer.rs`，`SamplePoint:4`、`TimeSeriesBuffer::extend():61`、清空 `:75` |
| 存储状态反馈 | 当前进度仅有批次数量；Controller 不使用 BatchWritten / Flushed 逐帧关联 | `service.rs`，`StorageEvent:88`；`backend.rs`，`drain_storage():1229`、忽略分支 `:1249` |

科学数据在队列和 sink 中保留了**已经进入模型的低 u16、时间和样本**；没有新的高位可供异步线程保留。显示事件送出也不是写盘成功回执。乱序数据的持久顺序为“完整帧重组完成并通过 Accepted 的顺序”，既不是首片到达顺序，也不是序号或 UTC 排序。GUI 点窗口的清空/裁剪又进一步限制了屏幕可追溯性；本轮不改变这些行为。

## 3. 现有磁盘格式与读取依赖

### 3.1 `.lpd` 当前版本

`crates/langmuir-storage/src/binary.rs:10` 至 `:16` 定义 magic=`LMPROBE\0`、version=`2`、header_len=`32`、frame_len=`1010`、flags=`1`。`write_header():168` 按小端写这些字段。

`BinarySink::write_batch():133` 的每条记录布局为：

| 记录内偏移 | 类型 | 字段 |
| --- | --- | --- |
| 0 | u32 LE | UTC 秒 |
| 4 | u32 LE | 微秒 |
| 8 | u16 LE | 科学模型低序号 |
| 10 | 125 × u32 LE | 通道 1 ADC |
| 510 | 125 × u32 LE | 通道 2 ADC |

时间和序号写入见 `binary.rs:140`、`:141`、`:142`。文件没有源端点、接收会话、完整 u32、P1-3 事件或原始 wire CRC。第 r 条（r 从 0 起）偏移为 `32 + 1010 × r`，不能将 r 直接当作设备序号。

`write_all():150` 失败可能留下部分尾记录；`flush():155`、`close():160` 只执行 BufWriter flush，没有文件同步调用。完整记录尾部是可解析边界，不是断电持久化或端到端完整性证明。

### 3.2 HDF5 当前 schema

`crates/langmuir-storage/src/hdf5.rs`，`Hdf5Sink::create():78` 建立：

| 名称 | 当前类型/形状 | 位置 |
| --- | --- | --- |
| `t1_base` | f64，N | `:93` |
| `sequence` | u16，N | `:98` |
| `y1_channel` / `y2_channel` | i32，N×125 | `:104`、`:114` |
| `committed_frames` | u64 attribute | `:123` |

没有应用 schema_version，也没有完整序号或原整数时间列。`write_batch():177` 将基准时间转换为 f64（`:181`），序号收集为 `Vec<u16>`（`:185`）。不能简单改同名 `sequence` 为 u32，却继续宣称旧消费者完全兼容。

HDF5 没有显式保存原 UTC/微秒字段，不等于所有合规微秒值必然无法数值还原：在当前 u32 秒范围内，已知 writer 语义下可设计、验证整数微秒取整重建。本轮未证明或测试该重建的全域正确性；迁移应首先原样保留 `t1_base`，明确重建字段的来源，不能据此恢复序号高位。

`write_batch():214` 依次扩容、写四个 dataset 后更新 `committed_frames`；失败调用 `rollback_shapes():261` 尽力恢复。`flush():243` 才调用 HDF5 flush。该 attribute 不是两文件事务提交，也不是断电原子保证。

### 3.3 实际读取 / CLI / 回放能力

| 对象 | 实际能力与依赖 | Rust 文件、函数与行号 |
| --- | --- | --- |
| storage crate | 仅导出 sink / service，没有生产文件 reader 或版本分派接口 | `crates/langmuir-storage/src/lib.rs:3`、导出 `:12` |
| `.lpd` 测试回读 | 测试内 `File::open` 后按当前常量手工解析，不是可复用 reader | `binary.rs`，`header_and_frames_round_trip_from_documented_layout():227`、读取 `:235` |
| HDF5 测试回读 | 测试按上述 dataset 名称读取 | `hdf5.rs`，`writes_python_compatible_dataset_names_shapes_and_values():339`、读取 `:359` |
| CLI `decode-lvds` | 读取一个原始 269B 科学帧，不解析 `.lpd` / HDF5；科学序号显示为无外层信息的默认 0 | `tools/protocol-cli/src/main.rs`，`run():42`、`decode_lvds_output():52`；`lvds.rs`，`decode_scientific_frame():96` |
| CLI UDP capture | Accepted 后写连续 269B raw；CSV 索引保存完整 u32、raw 偏移和长度 | `main.rs`，`receive_udp_lvds():65`、索引头 `:111`、观察 `:187`、写 raw `:189`、写完整序号 `:195` |
| CLI 索引限制 | 无 schema 版本、显式 capture/session、内容绑定和提交前缀；raw/index 两个 BufWriter 分别 flush | 同函数 `:109`、`:111`、`:204`、`:207` |
| `replay_bench` | 合成 Mock 科学帧并写临时 `.lpd` 的基准，不读取历史 capture | `crates/langmuir-transport/examples/replay_bench.rs`，`main():16`、创建 sink `:27`、生成 `:51`、只读取文件大小 `:132` |
| GUI | 没有 Open/Replay 请求；当前仅实时样本与指标事件 | `apps/desktop/src/bridge.rs`，`UiRequest:118`；`app.rs`，科学事件分支 `:591` |
| CSV 导出 | 只导出遥测，不是科学数据或完整序号读取器 | `backend.rs`，`export_csv():1419`、`write_telemetry_csv():1679` |

全仓搜索未发现 `read_binary_capture` 或生产 Python 科学文件读取器。故“未来新 reader 继续读取旧文件”是必须实现、测试的能力，不能说当前工程已经具备。外部 Python/HDF5 工具的兼容性只能按现有名称、类型和布局设计；未取得的外部实现不能被宣称已通过兼容验收。

CLI raw+index 在两者完整且配对来源可信时，可按 CSV 偏移恢复其 capture 的完整序号。它不替桌面 `.lpd` / HDF5 补高位；没有可靠配对关系时不能凭时间把两个独立采集硬连接。CLI raw 也不含原 24B 头、分片、坏 CRC、重复或冲突，不能称为完整 UDP 证据保存。

旧字节流入口不能当作 UDP：`LvdsStreamDecoder` 使用无外层解码（`crates/langmuir-core/src/framing.rs:212`、`lvds.rs:96`），序号 0 是未知占位；UDP 的实际序号 0 则是合法值。`core_adapter.rs:41` 只是透传该模型。transport Mock 中 `u16::try_from(...).unwrap()`（`crates/langmuir-transport/src/mock.rs:267`）仅位于固定低值测试，不是另一个生产截断点；未来高序号测试应使用正式门禁。新元数据必须区分 UDP、无外层字节流和合成来源。

## 4. 长期采集风险与当前质量关联限制

以下计算以 **20Hz 且设备每完整帧递增一次序号** 为模型，后一条仍为 `SPEC_ASSUMPTION`，不能替厂家定义：

| 场景 | 量化结果 | 实际影响 |
| --- | --- | --- |
| 低 16 位重复周期 | `65536 / 20 = 3276.8s`，54分36.8秒 | 1 与 65537 在科学文件均为 1；一次长采集中会重复 |
| 1 小时 | 72000 帧，已超过一次低位周期 | 不可再将 u16 当文件内唯一键 |
| 24 小时 | 1728000 帧，约 26.37 个低位周期；v2 约 1745280032B（含头） | 读取/索引需流式处理；历史高位不能凭行号恢复 |
| 完整 u32 回绕 | `2^32 / 20 = 214748364.8s`，约2485.51天 | 原生 u32 也不是永久唯一身份；重连或复位可能更早复用 |

字段宽度依据 `udp_lvds.rs:18`、`lvds.rs:46`、`binary.rs:15`；每帧 125 点及 400μs 依据 `lvds.rs:9`、`:10`。这些是模型计算，不是新执行的长时间采集。

行号 + UTC 可以定位“某文件内的这一条记录”，但不能证明它原来属于哪一个高 16 位、来源或接收会话。即使时间完全正确，也不能排除缺帧、乱序、重传、序号复位及文件从非零序号起录。不得默认把低位回绕次数从 0 累加，冒充恢复的真实完整序号。

P1-3 当前只能做会话级观察：`UdpLvdsSequenceMonitor::observe_at()` 返回 `()`（`udp_lvds_sequence.rs:91`），`metrics():190` 只导出聚合值；缺口记录也会被到期清理（`:167`）。`backend.rs` 的 `sample_reader_metrics():1273` 按既有间隔发布，`LinkMetricsSnapshot.udp_sequence` 位于 `bridge.rs:89`，没有存储行号或每帧结果。

**Accepted / observed、入队、sink 写成功、flush、持久化必须分别统计。** `handle_scientific_frame():1092` 可在停采时忽略；`:1121` 仅入队，GUI 随后发出；后续写入仍可失败。`service.rs:645` 的进度事件可因满队列丢弃，不能作为完整审计日志；`StorageMetrics::record_written():21`（`crates/langmuir-storage/src/metrics.rs`）也不是持久化回执。

因此当前不能把一次 500ms 指标变化精确归到某个磁盘记录，也不能以 `observed_frames - written_items` 直接认定缺失序号集合。接收会话可跨多个采集文件，空闲到期可改变质量状态；计数还存在重叠类别与饱和。`.incomplete` 仅记录文件、时间、原因（`backend.rs`，`mark_capture_incomplete():878`），没有失败帧/批次范围。

## 5. 方案 A 与 B 的对比

两种方案均需要先在截断前保留完整标签并随科学帧传递。单独修改 sink 字段宽度，无法恢复已经在 `accept_at():135` 丢掉的高位。

| 维度 | A：原生完整 u32 的新科学格式 | B：旧科学格式 + 版本化独立索引 |
| --- | --- | --- |
| 既有科学文件 | 旧 `.lpd` v2 保持原样，新 reader 明确支持 v2 / 新版本 | 旧字节布局不变；缺索引按 legacy 读取，完整序号未知 |
| 旧读取工具 | 不得假定能读新版本；可能拒绝，也可能错误按 1010B 解析 | 可继续读原科学布局，但看不到 u32 与新质量信息 |
| 新旧识别 | 必须版本、header/record 长度、schema 明示 | `.lpd` 仍 v2；索引另有 magic/schema 版本和主文件类型/版本 |
| HDF5 | 新 schema；保留旧 `sequence:u16` 并增加完整列/版本，或明确另立不兼容 schema | 原 dataset、类型和 attribute 保持；新信息在 sidecar，旧工具无需改动 |
| 帧数据配对 | 单记录原生序号更直接；仍需 session、记录身份和质量链接 | 两文件关联是主要额外风险，必须同 writer、行号和内容绑定 |
| 失败恢复 | 单文件也需记录完整性/提交规则；改 u32 本身不提供事务 | 必须区分孤立科学尾部、孤立索引和共同提交前缀 |
| 回放 | 新 reader 统一 v2 / 新版本，不能只改 writer | 新 reader 合并 legacy + 索引；无索引仍可回放科学数据但无法完整追溯 |
| 实现复杂度 | 涉及核心包络、sink 编码、新 reader、外部消费者迁移 | 同样涉及包络/new reader，少改旧科学编码，多做关联与恢复 |
| 长期维护 | 单文件身份更清晰，但长期维护多版本和迁移工具 | 持续维护 bundle 生命周期、索引版本与遗失恢复，不能称零成本 |
| 当前风险判断 | 外部消费者未知，不宜直接替换默认格式 | **兼容优先的推荐**，前提是实现第 6 节的协调写入和校验 |

### 5.1 A 的必要设计边界

`.lpd` 必须使用新版本号（例如 v3，**仅候选**），禁止仍标 v2 却把字段拓宽。仅将 u16 换为 u32 时记录长为 1012B，样本偏移也变化；若再加入来源、会话或质量标识，最终长度必须重新定义，不能直接把 1012 当完整设计。

新 reader 先校验 magic、版本、header/record 长度、端序和样本约束，分别解析 v2 与新版本。v2 转为“有低位、无完整序号”的统一读取模型；不能将低 u16 零扩展后冒充完整真实 u32。未知版本明确拒绝，不猜测字段位置。旧文件继续可读的承诺依赖这个**新增 reader 的双版本测试**，不依赖旧工具自动向前兼容。

HDF5 的容器格式版本与应用 schema 版本不同。若实施 A，建议新增应用 schema/version 属性，并保留旧 `sequence:uint16`，增加带有效性/来源语义的完整序号列及原整数时间列；新增 dataset 与 `committed_frames` 必须纳入同一写入/恢复边界。仅保留旧列可减少旧工具改动，但严格检查成员列表的外部工具仍需专项验证。历史文件无应用版本时，只在现有 dataset 名称、dtype、形状校验通过后识别为已知 legacy schema。

### 5.2 为什么本阶段推荐 B

B 保留当前已工作的科学文件合同，可先验证“同一科学记录配对到真实外层 u32”，而不同时引入所有消费者的格式迁移。**这不是说 B 的事务问题更简单**；没有协调 writer、提交前缀和配对校验，就不满足该推荐条件。若业务要求完整身份必须在单个科学文件内部自足保存，或无法保证 bundle 一同归档，应改选 A，不能承诺 B 等价。

## 6. 推荐 B 的拟议合同（待批准，未实现）

### 6.1 身份、包络与生命周期

建议在 ingress 仍持有完整 `ReassembledLvdsFrame` 时，为 Accepted 科学帧建立独立存储包络，保留原 `ScientificFrame`，增加下列元数据；**数据与元数据作为同一个队列 item 移动**，不使用两条独立异步序号/科学队列。

| 拟议字段 | 含义 |
| --- | --- |
| 来源类型与 `udp_sequence: Option<u32>` | UDP 为完整标签（0 有效）；字节流未知；Mock 明示合成，不填造真实 UDP 值 |
| `receiver_session_id`、源/绑定端点 | 每次新 receiver 创建新 ID；采集启停不改它；不是设备提供的会话标记 |
| `accepted_ordinal: u64` | 每接收会话单调的 Accepted 身份，独立于会饱和的统计计数；窗口外复用 u32 时仍可区分 |
| 原整数 UTC/微秒 | 与科学对象一起携带，不能以主机时间替代 |
| 接受处理时间 | 会话内单调相对时间；另可记录主机会话时间锚点及其非设备时钟语义，不序列化裸 `Instant` |
| `observation_id` / 策略版本 | 关联本次观察事件；聚合快照另存，不能伪装成逐帧结果 |

每次 StartAcquisition 创建新的 `capture_id`；一个 receiver session 可以关联多个 capture。当前重连会停止旧采集，建议维持该行为，不借本任务把多个新会话自动拼接成一个文件。

正式存储关联主键为 **`(capture_id, record_index)`**，而非序号、时间或 `(session,u32)`。writer 按收到的包络分配记录位置，保存 Accepted 身份与物理位置的映射；未入队帧没有已提交行号。`.lpd` 用逻辑行号及字节偏移，HDF5 用 dataset 行号。标识计数用检查加法：record_index 溢出须停止当前捕获，只能在新的 capture_id 下重新分配；接收会话内 accepted/event ordinal 溢出则须终止其原 ID 域，不能仅换捕获文件就在同一 receiver_session_id 下复用。不得饱和后产生重复 ID；P1-3 统计计数仍保持原饱和语义。

### 6.2 索引格式与内容绑定

建议将 sidecar 设计为一个有 header、带长度和完整性校验的追加记录日志，包含帧映射、质量事件/快照、批次准备及提交记录。独立 schema v1 为拟议起点；具体文件后缀、编码与磁盘校验算法应在获批实施前冻结。本轮不新增任何实际索引文件。

header 至少说明索引版本、capture/session、主文件格式及版本、生产软件/解码与监测策略版本；帧映射记录完整 u32 有效性、低位、时间、记录位置及主科学记录内容摘要。对 HDF5 应定义按行的规范逻辑字段编码作摘要，不以不稳定的文件内部块偏移作键。主文件整体摘要和最终条数在成功关闭后封存；每批记录摘要和提交边界支持未关闭文件的校验。

读取器必须验证低位关系、记录位置、时间/样本摘要及批次边界，拒绝误配索引。文件名、时间或仅 CRC16 相等都不够；科学 CRC16 继续只负责原科学帧，不能充当 sidecar 完整性校验。

**B 的固有限制**：不改主文件就无法在主文件内部加入 capture_id。摘要可证明索引所配的科学内容一致，不能区分两个逐字节相同、但原始完整序号/来源不同的 legacy 科学文件。必须把主文件、索引和归档身份作为受控 capture bundle 一同交付，明确来源链；不能声称 sidecar 自证原始来源。丢失 bundle 归属时，只能报告“内容匹配、来源未证实”。需要单文件自含身份时选择 A。

### 6.3 质量记录如何关联

当前 `observe_at()` 没有逐帧返回值，不能用修改前后聚合指标差值反推事件。后续如批准逐帧质量关联，应在**同一个现有状态机**上增加观察事件输出/适配接口，保持原规则与 metrics 完全相同，不复制另一个监测器。

Accepted 观察事件链接到 `(receiver_session_id, accepted_ordinal)`，写入后再映射到 `(capture_id, record_index)`。缺口建立、到期、补齐、遗忘事件使用独立 `gap_id` 与会话内事件序位；到期可能在空闲时发生，不强行挂到某个正常科学帧。迟到补齐可以引用旧 gap，即使该候选已经等待到期，也不能把历史到期等同永久缺失。

拒收、重复、冲突没有科学数据行，可保留有明确时间范围和会话的分类快照/事件，不能生成虚构存储行。若只实现聚合快照，必须标为“会话/区间质量”，不能宣称逐帧原因已追溯。新采集记录监测基线和 Accepted 起止范围；跨文件候选引用保留会话语义，不为方便归档而清空 P1-3 状态。

质量事件通道和磁盘写入保持有界；日志/事件溢出明确标记追溯不完整，不可静默丢失后宣称日志完整。磁盘或 sidecar 故障不能回滚去重再重放 Accepted 科学帧，也不能改变 P1-3 观察值。

### 6.4 单 writer 与提交/恢复

建议一个协调 sink/writer 拥有主科学文件和 sidecar。批次状态至少分为 `accepted`、`enqueued`、`sink_written`、`flushed`、`bundle_committed`；需要断电级保证时再定义经过验证的 durable 等级。旧 `written_items` 和 `BatchWritten` 的含义不应悄悄改名成持久化成功。

拟议流程为：登记带内容摘要的 PREPARE → 写原布局科学记录和帧映射 → 按声明的 flush/同步策略完成相关文件操作 → 追加 COMMIT（批次范围、科学位置、索引边界与摘要）→ COMMIT 本身也完成该等级要求的 flush/同步 → 才发布 bundle_committed 并更新可靠提交前缀。成功关闭时排空、提交、封存条数/摘要；metadata 失败则停止捕获并标记不完整，不自动退回只写科学文件的“成功”模式。

两个文件不能因各自 flush 或一次 rename 就获得原子事务。断电级 COMMIT 需要证明数据先于提交记录持久化，处理文件及目录同步；当前 BinarySink 没有该同步合同，HDF5 flush/VFD 也需要专项设计与故障验证。建议先把 `.lpd` 的可验证恢复与同步等级做实，再单独验收 HDF5；未验证的模式只能声明相应的进程错误恢复能力。

| 故障点 | 新读取/恢复工具应有行为（设计，NOT_RUN） |
| --- | --- |
| 入队失败 | 此 Accepted 没有成功存储结论；保持原停采/错误处理，记录可确认的失败范围 |
| PREPARE 后、科学记录未完整 | 不视作已提交，保留故障材料 |
| 科学尾部存在而索引/COMMIT 缺失 | 作为孤立、追溯未知尾部；不猜高位，不自动当作成功帧 |
| 索引有记录而科学尾部不足/摘要不符 | 不采信该映射；标损坏/未完成 |
| 完整 COMMIT 之前的共同前缀可验证 | 只将该共同前缀标为已配对；提交等级另按实际同步合同说明 |
| COMMIT 日志撕裂、重复或未知版本 | 停在最后可验证边界；不通过猜测跳过损坏建立虚假关联 |
| flush/close/HDF5 rollback 失败 | 保留原件，状态不完整；若需修复，输出新恢复副本及恢复报告，不原地静默截断 |
| 进度事件队列满或进程意外退出 | 依据磁盘日志及可靠终止/提交状态核对，不能依赖 UI 日志恰好收齐 |

legacy 工具会忽略 sidecar，可能读取到崩溃前完整但尚未共同提交的科学尾部；B 无法让未升级的读取器自动理解提交边界。归档/分析应优先使用 bundle-aware reader，或明确交付经验证的恢复副本，不能把兼容布局等同于恢复语义兼容。

## 7. 历史迁移与原始帧证据

| 输入 / 操作 | 可无损保留或迁移 | 无法凭现有内容恢复 |
| --- | --- | --- |
| 旧 `.lpd` v2 | 原整数时间、低 u16、ADC、物理记录顺序；新 reader 可显式读取为 full-sequence-unknown | 原高 16 位、来源、会话、接受时刻、P1-3 事件、真实网络分片 |
| 旧 HDF5 | 原 dataset 值、dtype/形状与既有属性；原样保留 f64 时间 | 完整原序号及会话；整数时间重建需另行验证来源/算法 |
| 可信配对的 CLI raw+index | 原 269B、索引 u32、完成顺序与可核验偏移，可导入新包络并标“外部导入来源” | 未记录的显式设备会话、首片/分片历史、拒收事件；不自动等同桌面 capture |
| 只有 raw269，无 index | 时间、ADC 和重新执行科学校验的结果 | 外层 u32，因为科学帧内没有它 |
| 未来完整 bundle | 按验证后的行映射转成 A 新格式，保留身份与质量来源 | 不能因此补出从未采集的 wire 或设备会话标记 |

新 reader 可使用 `Option<u32>` / 显式有效性表达历史未知；不得简单零扩展 u16、按行号累计、按 20Hz 插值，或把估计高位伪装成真实数据。若提供估算工具，必须另列估计字段及假设，不覆盖原数据。任何迁移生成新文件，保留原件；迁移成功也不增加原证据的信息量。

**需要独立设计原始证据保存，但它不是保存 u32 的前置格式变更。** 最低序号追溯只需可信的完整标签与科学行映射；要独立复核原科学 CRC，需保留实际收到的 269B 与外层标签/来源/会话；要重演分片、TTL、重复、冲突或坏 CRC，需在相应门禁之前受控保存原数据报与接收顺序/相对时间。现有 CLI 只保 Accepted raw269，不满足后一层。

从 ADC、时间和固定协议字段重新编码、再计算 CRC，最多得到一个重建候选，不能冒充原始实收字节或证明曾收到某个坏候选。新磁盘摘要也不替代原 wire。原始异常录制需另有大小/生命周期与故障策略，本阶段不实现、不扩大为全量抓包任务。

## 8. 拟议测试与验收矩阵

下列均为 **测试设计，状态 NOT_RUN**；本轮未修改任何测试代码。时间由注入时钟推进，不用长时间 sleep；格式测试使用新建临时文件，不使用历史异常 PCAP 作黄金样本。

| 编号 | 测试设计 | 必须核对的结果 |
| --- | --- | --- |
| D01 | 相同科学内容、外层 1 / 65537 | 主文件低位同为1，索引完整值分别正确，两个记录身份不同，不能合并 |
| D02 | FFFFFFFE,FFFFFFFF,0,1 | 全值原样保存、零有效，Accepted 完成顺序保持，P1-3 连续3/回绕1 |
| D03 | 单片与多片完整重复 | 只一条科学记录及对应已提交映射，重复计数保持；无重复 GUI 科学事件 |
| D04 | 同序号不同合法内容冲突 | 仍拒绝第二条科学数据；冲突质量记录不伪造科学行 |
| D05 | 坏 CRC→同序号合法重传 | 坏帧不注册去重/正常序号观察，不写正常科学行；合法重传可接受并正确索引 |
| D06 | 100,101,104,103,102 | 文件和回放默认按完成顺序；候选2/迟到2/乱序2，不排序或补样本 |
| D07 | 2s 等待到期后补齐、7s 遗忘 | 索引质量事件/快照能区分历史到期与当前未解决；无“网络丢包已确认”误报 |
| D08 | 400帧20Hz；模拟至少24h的1728000帧 | 序号逐条或流式摘要核对，跨多个低位周期；批次/索引内存有界，无按 u32 差值分配 |
| D09 | 写入→部分批次 flush→关闭→重开读取 | 每条时间、两路125点、u16/u32、行号和摘要一致；尾批次正确处理 |
| D10 | 固定旧 v2 字节样本、无索引 | 正确读取历史字段，完整序号明确未知，不能凭旧 sequence0 声称UDP0 |
| D11 | A候选新版本（仅若选A）及未知版本 | 新 reader 分派 v2/新版本；未知版本/错误长度明确报错；旧工具行为单独核验 |
| D12 | 旧/新 HDF5 schema与B sidecar | 保持旧名称/dtype/形状；有无应用版本分别校验；HDF5 feature单独构建执行 |
| D13 | 不同内容主文件误配、索引损坏/版本未知 | 配对校验拒绝；必要时仍能只读 legacy 科学数据，但不提供虚假完整序号 |
| D14 | 科学字节完全相同、外层身份不同的两组bundle | 验证B摘要无法独自证明来源的边界；导入来源和bundle保管状态明确，不声称自动识破 |
| D15 | 不同接收器/来源/重连，采集启停多文件 | receiver session与capture独立；接收会话指标不错误清零或混入其他源 |
| D16 | UDP0、字节流未知0、合成数据 | 新读取模型明确区分来源与有效性，不以零作未知哨兵 |
| D17 | 去重历史到期后同序号再次Accepted | 新Accepted身份/新行号，原规则不变，u32不充当永久主键 |
| D18 | 主写失败、部分尾记录、sidecar写失败、flush/close失败 | 不伪造成功映射；保留可验证共同前缀、失败阶段和范围，不自动降级或重放 |
| D19 | PREPARE/数据/索引/COMMIT各边界崩溃与撕裂 | 重开后严格按可验证边界恢复；原件不改；进程退出与断电模拟分级验收 |
| D20 | HDF5每个dataset/attribute写入及rollback失败 | 主/索引前缀一致性显式检查；不能仅信 dataset长度或committed_frames |
| D21 | 进度事件丢弃、停采接收、入队失败、后续磁盘失败 | Accepted/enqueued/written/flushed/committed各有定义；GUI已显示不当作磁盘成功 |
| D22 | 标识u64极限、计数饱和、时钟逆序/跨秒 | ID检查溢出不复用；原P1-3计数饱和与时钟规则保持；时间字段不被主机时间替换 |
| D23 | `.lpd`/HDF5旧值迁移、CLI导入与缺索引 | 原字段无损；来源可信度/未知高位保留；不写造假的历史u32 |
| D24 | CRC16、64槽TTL恢复、5秒去重、P1-3及RS422全回归 | 原170项全部保留通过，再报告新增项；科学文件兼容矩阵另行验收 |

长期测试分层：日常回归用可控时间、流式模型/编码核对跨越至少24小时；约1.75GB主文件的全量写入、关闭与重开读取列为独立I/O压力测试，避免每次单元测试都生成大文件。两层目前均为 NOT_RUN。

实施阶段建议先验证 reader、坏索引和失败恢复，再启用新 writer 默认路径；外部 HDF5/Python 工具必须用实际版本/样本专项验证。除原 workspace 命令外，另执行显式 HDF5 feature 的测试；原 Mock 170 项不涵盖默认关闭的 HDF5 路径（`crates/langmuir-storage/Cargo.toml:9`、`src/lib.rs:5`）。

未来验收仍需执行 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --features langmuir-desktop/mock --locked`、严格 workspace Clippy，并分别记录已知基线 lint 与新增问题。不得用命令行放宽 lint 的诊断冒充严格 PASS。

## 9. 对前四阶段的保护与本轮实际结果

| 项目 | 本轮状态 |
| --- | --- |
| 开始时 Git 状态 / 累计 diff 审计 | PASS：18个已跟踪修改，2522 / 270 |
| 全链路源码、文件schema、实际读取能力审计 | PASS：结论按当前源码及行号记录 |
| 前后63个现有文件SHA-256核对 | PASS：全部一致，仅新增本报告 |
| Rust / 测试 / 旧文档 / Cargo.lock / 实际科学文件变更 | 无；未执行存储实现或格式迁移 |
| 新方案所有自动化测试及Cargo复测 | NOT_RUN：本轮仅设计；170通过为P1-3历史基线 |
| 严格Clippy | NOT_RUN：本轮未重跑；此前实际FAIL不被本报告改写 |
| GUI、真实设备、抓包/历史数据回放 | NOT_RUN |
| Git提交、重置、清理 | NOT_RUN |

获批实施时，完整 u32 应作为 Accepted 的伴随元数据继续下传，而非改变科学帧或外层协议；原269B、55→AA、CRC16 `[7..267]`、两路125点、24B UDP、残帧2秒TTL、去重5秒/256 FIFO和P1-3规则均作为不变回归合同。写盘失败只能按存储错误处理，不能通过清除去重或重锚监测来“补写”。RS422、GUI布局与CSV格式不纳入存储追溯实施。

最终工作区仍保留原18个已跟踪修改，`git diff --stat` 仍为2522 insertions、270 deletions；原两个未跟踪核心模块、四份历史报告全部保留，新增仅 `docs/UDP_LVDS_U32_STORAGE_COMPATIBILITY_REPORT.md`。默认diff统计不包含这些未跟踪文件，不能误解为本报告没有生成。

## 10. 待批准的实施边界

推荐下一步在独立范围内实施 **B 的最小可验收闭环：完整标签包络 → 同队列/同writer → 版本化索引与明确提交等级 → legacy+bundle读取/恢复 → 失败注入与旧数据兼容测试**。质量区间快照和逐帧事件必须明确能力等级，不能把前者冒充后者。

尚需厂家确认序号递增、复位/回绕、会话及重传语义（`SPEC_ASSUMPTION`）；这些不妨碍原样保存实际观察的u32，却限制对设备行为的解释。HDF5断电持久化合同、外部消费者验证和原始异常证据录制需分别定界，不自动扩展开发。

**本轮止于本设计审计报告。等待用户批准方案与实施范围后，才进入存储实现；本报告不构成已修改格式或已通过新存储验收的声明。**
