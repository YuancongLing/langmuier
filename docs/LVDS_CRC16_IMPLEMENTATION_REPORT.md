# LVDS CRC16 实施与回归报告

日期：2026-10-19。基线提交：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。

软件 CRC 门禁已实施，最终包含 Mock 的 workspace 回归为 **118 passed**。严格 Clippy 仍为 **FAIL**，原因是基线已有 lint，详见第 9 节。因此本报告不宣称所有质量门槛或真实硬件验收已经通过。

本轮开始时 `git status --short` 和 `git diff --stat` 均为空。只修改 LVDS 校验及直接接收、诊断、测试和说明；未读取或回放旧异常 PCAP，未连接真实设备、发送控制指令或执行 Git 提交。

状态含义：`PASS` 为实际执行通过；`FAIL` 为实际检查失败；`BLOCKED` 为环境阻断；`NOT_RUN` 为未执行；`SPEC_ASSUMPTION` 为尚需厂家确认的协议解释。

## 1. 修改前缺陷与代码位置

以下行号指基线提交，可用 `git show <基线>:<文件>` 复查：

| 文件与位置 | 缺陷 | 本轮修复 |
| --- | --- | --- |
| `crates/langmuir-core/src/lvds.rs:110,175` | `decode_scientific_frame_with_sequence()` 只读取尾部 CRC，不比较便生成科学帧 | 结构检查后强制 CRC，随后才解码时间和 ADC |
| 同文件 `:211,219` | 独立校验函数使用旧 `[6..267]` 范围，且正式解码未调用 | 统一为 `[7..267]`，从正式解码入口调用 |
| `apps/desktop/src/backend.rs:144` | UDP 重组后依赖上述宽松解码；科学帧错误只增加总拒收数 | 同一严格入口、分类统计、最近计算/接收 CRC |
| `crates/langmuir-transport/src/core_adapter.rs` | 字节流虽调用核心解码，但 CRC 门禁缺失；分类不完整 | 继承严格解码，统一错误分类并保留重同步 |
| `crates/langmuir-transport/src/mock.rs:169,186` | 科学 Mock 帧尾固定 `0000` | 按统一范围生成合法 CRC，另增加独立固定黄金测试 |
| `tools/protocol-cli/src/main.rs` | `decode-lvds` 继承宽松解码，UDP capture 在重组后直接写文件 | 严格单帧解码；capture 校验后才写入并计为完成 |

现有协议说明中“等待确认后再启用实时拒收”也与本次要求冲突，已同步更正。历史 UI 验收报告未重写。

## 2. CRC 算法、布局与覆盖范围

复用唯一生产算法 `crc16_ccitt_false()`：Poly=`1021`、Init=`FFFF`、RefIn/RefOut=false、XorOut=`0000`，结果按大端存储。未新增另一套生产 CRC 算法；独立参考函数只存在于测试中。

| Offset | Size | 字段 |
| --- | ---: | --- |
| 0–1 | 2B | `EB 90` |
| 2 | 1B | SRC=`55` |
| 3 | 1B | DST=`AA` |
| 4–5 | 2B | LEN=`01 07` |
| 6 | 1B | TYPE=`AA` |
| 7–10 | 4B | UTC，大端 `u32` |
| 11–14 | 4B | 微秒，大端 `u32` |
| 15–266 | 252B | 科学 DATA |
| 267–268 | 2B | CRC16，大端 |

帧长仍为 269B，无 LVDS 内部序号。集中常量为：

```rust
LVDS_CRC_CALC_START = 7;
LVDS_CRC_CALC_END = 267;       // 独占终点
LVDS_CRC_FIELD_OFFSET = 267;  // 与计算终点分开定义
LVDS_CRC_INPUT_LEN = 260;
```

CRC 输入严格为 `frame[7..267]`，即 UTC 4B + 微秒 4B + DATA 252B，不含 TYPE 和 CRC。旧 `[6..267]` 或 DATA-only `[15..267]` 均不作为回退范围。

**SPEC_ASSUMPTION**：该范围来自新版表格“有效数据”的工作解释。厂家 C 确定算法，不证明实际调用起点；仍需厂家提供 `Calculate_CRC16(data, length)` 的真实 `data` 指针和 `length`。当前依此解释强制拒收，未来若确认范围不同，应单独修订协议与测试。

## 3. 修改后的完整接收链路

```text
桌面 UDP 数据报（保留边界，过滤源端点）
  → 原有 24B 头解析与长度约束
  → 原有分片重组
  → 统一科学帧解码：269B / EB90 / LEN / SRC / DST / TYPE
  → CRC16 [7..267] 与尾部 BE 值比较
  → 微秒合法性 → 两路 ADC 恢复
  → frames_decoded + 1，输出 ScientificFrame
  → Controller::handle_scientific_frame（采集运行时）
  → 原有 BatchWriter 存储与 ScientificSamples 显示事件
```

CRC 失败在回调前返回，既不调用正常 handler，也不产生有效帧计数。生产 socket 使用提取出的 `UdpLvdsIngress`，协议测试直接向同一入口注入内存数据报，不复制一套测试专用解析器。

旧字节流 `LvdsStreamDecoder` → 同一核心解码 → `ReaderWorker` → 原有处理路径；保留任意分块与坏候选后的逐字节重同步。CLI `decode-lvds` 调用同一解码，错误经 `main` 返回非成功退出码。CLI `receive-udp-lvds` 在同一校验通过后才写 raw/index，坏帧不计入目标数量；结束或超时汇总拒收数及最后错误。

科学存储本身接受已解码的 `ScientificFrame`，不是额外的 wire 解析入口；文件编码和 schema 未修改。桌面 GUI Mock 仍可直接注入绘图事件，仅用于 UI 工作流，不作为协议校验依据。

## 4. 修改的 Rust 文件与函数

| 文件 | 修改对象 |
| --- | --- |
| `crates/langmuir-core/src/lvds.rs` | CRC 常量；`decode_scientific_frame_with_sequence()`、`validate_scientific_frame_crc16()`、`reconstruct_sample()`；独立黄金帧、变异和时间/ADC 测试 |
| `crates/langmuir-core/src/checksum.rs` | 测试专用 `manufacturer_crc16()`、厂家固定向量测试；生产算法未变 |
| `crates/langmuir-core/src/lib.rs` | 导出 CRC 范围常量 |
| `crates/langmuir-core/src/framing.rs` | LVDS CRC 拒收与重同步测试、LVDS 测试迭代写法；生产流式框架与 RS422 测试未变 |
| `crates/langmuir-transport/src/metrics.rs` | `DecodeDelta`、`ReaderMetricsSnapshot` 的 `record_frame_error()`；`ReaderMetrics::record_decode_delta()`、`snapshot()` 及新增计数/最近 CRC |
| `crates/langmuir-transport/src/core_adapter.rs` | `delta()` 与 adapter macro 的 `push()`；坏 CRC 零输出及恢复测试 |
| `crates/langmuir-transport/src/mock.rs` | `lvds_packet()` 合法 CRC；协议级固定黄金帧 UDP 测试 |
| `crates/langmuir-transport/src/pipeline_test.rs` | `packet_at_50ms_interval()`；有效/坏 CRC 混合输入到 `.lpd` 的回归 |
| `apps/desktop/src/backend.rs` | `UdpLvdsReceiver::{bind,drain,metrics}`；`UdpLvdsIngress::{new,push}`；`report_metric_growth()`、`ui_metrics()`；内存正常处理链及分类/聚合测试 |
| `apps/desktop/src/bridge.rs` | `LinkMetricsSnapshot` 增加分类与最近 CRC 字段 |
| `apps/desktop/src/app.rs` | `header_device_group()` 区分未监听/监听中/有效帧；`plot_panel()`、`plot_cursor_readout()` 标注未校准估算，数值公式不变 |
| `tools/protocol-cli/src/main.rs` | `run()`、`decode_lvds_output()`、`receive_udp_lvds()`；CLI 固定黄金 CRC 测试 |

同步文档：`LVDS_DATA_FORMAT_20260724.md`、`UDP_LVDS_REASSEMBLY_PROTOCOL.md`、`PROTOCOL.md` 的 LVDS 说明，以及 `ARCHITECTURE.md`、`MIGRATION.md` 中冲突的 CRC 描述。

## 5. CRC 错误处理与状态反馈

复用现有错误类型，不引入重复错误：

```rust
FrameError::ChecksumMismatch {
    protocol: Protocol::Lvds,
    expected, // 计算值
    actual,   // 帧尾值
}
```

错误帧只返回错误，无 `ScientificFrame`。核心解码移除固定数组转换的 `unwrap()` 及 ADC 转换的 `expect()`；先确认总长再读取固定偏移，ADC 越界继续返回 `SampleOutOfRange`。

| 计数 | 含义 |
| --- | --- |
| `length_failures` | 总长或声明长度错误 |
| `header_failures` | 帧头错误 |
| `field_failures` | 地址、类型等固定字段错误 |
| `checksum_failures` | CRC 比较失败 |
| `timestamp_failures` | 时间字段错误，当前为微秒范围 |
| `sample_failures` | ADC 恢复越界 |
| `frames_decoded` | 完整通过结构、CRC、时间和 ADC 的帧 |

分类同时计入 `rejected_frames`，不重复累计。外层 UDP 解析/重组错误仍使用总拒收计数，不伪称为内层 CRC 错误。源端点不符的数据报继续忽略。

最近 `(expected, actual)` 保存在快照中；流式 reader 用单个原子值保存整对数据，避免读取混合值。桌面沿用 500ms 采样聚合日志与非阻塞事件发送，不逐个坏帧写日志；队列压力沿用现有丢弃/统计机制。典型日志包含 `校验失败=512`、`计算值=0x44C8`、`收到值=0x44C9`。

Header 仅在收到近期 `ScientificSamples` 时显示“有效帧”，仅绑定 UDP 时显示“监听中”。停止采集时即使协议计数增长，也不会把监听状态冒充新科学显示数据；GUI Mock 状态仍独立标识。

## 6. 人工构造的 269B 黄金帧

以下完整 HEX 无省略，共 269B。等价构造为 19B 前缀 `EB9055AA0107AA65000000000F41781234ABCD` + `01FF` 重复 124 次 + `44C8`：

```text
EB 90 55 AA 01 07 AA 65 00 00 00 00 0F 41 78 12
34 AB CD 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01 FF 01
FF 01 FF 01 FF 01 FF 01 FF 01 FF 44 C8
```

预期 UTC=`1694498816`（`65000000`），微秒=`999800`（`000F4178`）。对 `k=0..124`：通道 1=`4660+k`，通道 2=`43981-k`，末点分别为 `4784/43857`。两路各 125 点；第一帧第 2 点时间为 `1694498817.0002`，末点为 `1694498817.0494`。

第二帧仅替换 UTC 为 `65000001`、微秒为 `0000C288`（49800）、尾部为 `5EF9`，两路 DATA 相同。两个帧首点相隔 50ms，并分别从 `4660/43981` 起始，验证不跨帧累加。

黄金 CRC 来自实际编译运行厂家原始 C（`cc -std=c11 -Wall -Wextra -Werror`），并与 Rust 比对；不是用生产 CRC 给自己生成全部预期。临时参考源码/程序在 `/private/tmp/langmuir-crc-reference.2dktwS/`，固定期望已保存在仓库测试中。实际输出：

```text
empty=FFFF ASCII=29B1 golden=44C8 B=DC4E C=0A62
ascending=3FBD binary=5B5D
second_frame=5EF9
```

`ascending` 为 `00..FF`，`binary` 为 `00 FF 80 01 55 AA`；B 为 `[6..267]`，C 为 `[15..267]`，后二者仅用于验证错误范围确实被拒绝。

## 7. CRC 正确与错误测试结果

| 场景 | 结果 | 证据 |
| --- | --- | --- |
| 空输入、ASCII、递增字节、二进制向量对照厂家 C | PASS | `crc_matches_independent_manufacturer_c_vectors` |
| 269B 固定结构、260B 输入、BE CRC、两路全部 125 点 | PASS | `fixed_golden_frame_has_independent_crc_and_expected_samples` |
| 单独翻转 UTC[7]、微秒[14]、DATA[100]、CRC[267]/[268] | PASS | 每次明确断言 `ChecksumMismatch`；变异时间/ADC 仍合法 |
| 每个变异按测试参考算法重算后恢复 | PASS | `rejects_crc_changes_and_accepts_independently_recalculated_frames` |
| 错误旧范围、DATA-only、CRC 高低字节反置 | PASS | `DC4E`、`0A62`、`C844` 均拒绝，预期 `44C8` |
| 所有截断长度、270B 超长、错头/地址/TYPE/LEN | PASS | `rejects_length_and_structure_errors` 等 |
| 坏 CRC 且时间/ADC 非法时先报 CRC；CRC 正确后报具体时间/ADC 错误 | PASS | 微秒与 ADC 越界测试 |
| 两通道 ADC 上/下溢、独立帧绝对基准、400μs 跨秒 | PASS | ADC、连续黄金帧与采样时间测试 |
| 任意长度/伪随机输入不 panic | PASS | `arbitrary_wire_input_returns_errors_without_panicking` |
| 坏 CRC 后正常流式恢复，1/7/269/538B 分块 | PASS | `lvds_rejects_crc_failure_and_resynchronizes_to_next_valid_frame` |
| CLI 固定黄金帧及坏 CRC `44C9`/计算 `44C8` | PASS | CLI 新增测试 |
| 原有六条 RS422 黄金指令与 SUM 回归 | PASS | core 与 CLI 原有黄金测试，帧内容和实现未修改 |

## 8. Mock 与正常处理链验证

| 路径 | 结果 | 实际覆盖 |
| --- | --- | --- |
| transport 协议 Mock | PASS | 固定 `44C8/5EF9` → 24B UDP 头 → 重组 → 严格解码；两帧 50ms、两路全点，夹入坏 CRC 仅输出两帧 |
| 桌面生产 ingress → Controller → BatchWriter 内存 sink + 显示队列 | PASS | 第一帧分两片并倒序输入，第二帧合法，第三帧坏 CRC；正常 handler、存储、显示均只有两次 |
| 流式 ReaderWorker → BinarySink `.lpd` | PASS | 四帧以 50ms 间隔输入，夹入坏 CRC；仅写四帧、CRC 失败计数 1，二进制时间/ADC 字段核对通过 |
| 分类与日志聚合 | PASS | 八个坏候选分别计数；连续 512 个坏 CRC 精确计数、无 handler、一次快照只产生一条质量日志 |
| 既有 UDP 回环测试 | PASS | 本机 loopback 的数据报分次读取与 UDP→LVDS worker 测试；非真实设备 |
| 真实桌面 socket → 上述整个存储链 | NOT_RUN | 本轮完整协议测试以内存数据报注入生产 ingress；不声称等价于实机网络验证 |
| GUI 启动/截图/实际硬件/校准验证 | NOT_RUN | 本轮协议修复不执行硬件或视觉验收 |

依赖尚未下载时另运行真实 transport 源码模块的独立内存 harness，14 项通过；最终正式 Cargo 测试已能完整执行，因此不以 harness 代替 Cargo 验收。

## 9. Cargo 与 Clippy 实际结果

工具链：`rustc 1.99.0 (b940084d7 2026-09-28)`。没有修改依赖版本、Cargo.toml、Cargo.lock 或删除测试。

| 命令 | 最终结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test -p langmuir-core --locked` | PASS，40 tests |
| `cargo test -p langmuir-protocol-cli --locked` | PASS，5 tests |
| `cargo test -p langmuir-desktop --features mock --locked` | PASS，47 tests |
| `cargo test --workspace --locked` | PASS，115 tests：core 40 / desktop 44 / CLI 5 / storage 9 / transport 17 |
| `cargo test --workspace --features langmuir-desktop/mock --locked --offline` | PASS，最终复核 118 tests：桌面 47，其他同上 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | FAIL，基线已有 lint，见下文 |
| `cargo clippy -p langmuir-core --lib --locked --offline -- -D warnings` | PASS |
| `cargo clippy -p langmuir-transport --features mock --lib --locked --offline -- -D warnings` | PASS |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

初次桌面测试为 **BLOCKED**：沙箱内 DNS 无法解析 `static.crates.io`，缺 `errno v0.3.14` 等。沙箱外正常 Cargo 下载锁定版本后，正式桌面测试通过。初次 workspace 测试的两个既有 UDP 测试因沙箱禁止绑定本机端口而失败；沙箱外仅使用本机回环重跑，115 项全部通过。最终离线 Mock workspace 118 项再通过，以上阻断均已解除。

严格 Clippy 的实际错误包括：

- `tools/protocol-cli/src/main.rs` 既有 `parse_hex()`：`chunks_exact_to_as_chunks`。
- `crates/langmuir-storage/src/binary.rs:143,146`：`explicit_iter_loop`。
- `crates/langmuir-core/src/framing.rs:289,291` 既有 RS422 空集合断言：`assert_is_empty`。

继续排查可见 `app.rs` 既有的过长函数、缺分号、`match_result_ok`、格式化参数、`if_not_else`、`manual_clamp`、`items_after_statements` 和常量断言 lint。已用基线源码核对相关既有代码。本轮新增测试的变量命名、浮点字面量、分块迭代和过长测试函数 lint 已修复；未借本任务重写 RS422、存储或 UI。

仅为排查新增问题，额外运行下列放宽既有类别的诊断命令并通过；工程 lint 配置未修改，**该 PASS 不替代上述严格 Clippy 的 FAIL**：

```text
cargo clippy --workspace --all-targets --locked -- -D warnings
  -A clippy::chunks_exact_to_as_chunks -A clippy::explicit_iter_loop
  -A clippy::assert_is_empty -A clippy::too_many_lines
  -A clippy::semicolon_if_nothing_returned -A clippy::match_result_ok
  -A clippy::uninlined_format_args -A clippy::if_not_else
  -A clippy::manual_clamp -A clippy::items_after_statements
  -A clippy::assertions_on_constants
```

## 10. 尚未确认的厂家协议细节

| 项目 | 状态 | 当前行为 |
| --- | --- | --- |
| CRC 实际调用的 `data` 起点和 `length` | SPEC_ASSUMPTION | 固定 `[7..267]` 共 260B 强制门禁；无回退 |
| 负差分是否明确为补码 `i8` | SPEC_ASSUMPTION | 保留现有语义及回归，不擅自解释为其他编码 |
| `(ADC - 32768) / 6.4` 的物理校准依据 | SPEC_ASSUMPTION | 公式数值不变，图表标为未校准估算；不作为正式校准结果 |
| UTC 时间纪元及设备时钟实测 | NOT_RUN | 保留原有秒级 `u32` 和设备时间，不改纪元、不替换主机时间 |
| 真实硬件协议一致性 | NOT_RUN | 本轮独立人工帧和厂家算法验证，未接硬件 |

400μs 的依据为 `10000 / 4 = 2500 samples/s`，不是新增协议猜测。两路点数、ADC 原始存储、269B 布局、55/AA 地址和 UDP 24B 格式均保持。

## 11. 后续 UDP 可靠性待办

| 优先级 | 待办 | 本轮状态 |
| --- | --- | --- |
| P1 | 残帧 TTL/超时回收，64 槽耗尽时的淘汰与恢复 | 未实现，下一阶段单独处理 |
| P1 | 完整帧去重、跨帧乱序与缺口检测 | 未实现；现有分片按 offset 重组继续保留 |
| P1 | 完整保留 UDP `u32` 序号，解决桌面当前 `u16` 截断 | 未实现，涉及模型/存储兼容性单独设计 |
| P2 | 完整原始 wire 的受控诊断保存，隔离坏帧与正常科学文件 | 未实现；当前 CLI capture 仅保存合法科学帧 |
| P2 | LVDS 科学 CSV 导出 | 未实现；本轮未扩展现有 CSV 功能 |

未开发位移恢复、1bit 纠偏、硬件输出修复或 FPGA 位同步，也没有变更采集控制协议、`.lpd` 或 HDF5 格式。

## 12. 最终 Git 状态与差异统计

所有修改均未提交。`git status --short`：

```text
 M apps/desktop/src/app.rs
 M apps/desktop/src/backend.rs
 M apps/desktop/src/bridge.rs
 M crates/langmuir-core/src/checksum.rs
 M crates/langmuir-core/src/framing.rs
 M crates/langmuir-core/src/lib.rs
 M crates/langmuir-core/src/lvds.rs
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
```

`git diff --stat`（未跟踪的本报告不计入默认统计）：

```text
 apps/desktop/src/app.rs                        |  20 +-
 apps/desktop/src/backend.rs                    | 481 ++++++++++++++++++++-----
 apps/desktop/src/bridge.rs                     |   6 +
 crates/langmuir-core/src/checksum.rs           |  40 +-
 crates/langmuir-core/src/framing.rs            |  39 +-
 crates/langmuir-core/src/lib.rs                |   1 +
 crates/langmuir-core/src/lvds.rs               | 280 ++++++++++++--
 crates/langmuir-transport/src/core_adapter.rs  | 134 ++++---
 crates/langmuir-transport/src/metrics.rs       |  96 +++++
 crates/langmuir-transport/src/mock.rs          |  91 ++++-
 crates/langmuir-transport/src/pipeline_test.rs |  28 +-
 docs/ARCHITECTURE.md                           |   8 +-
 docs/LVDS_DATA_FORMAT_20260724.md              |  42 ++-
 docs/MIGRATION.md                              |  14 +-
 docs/PROTOCOL.md                               |  34 +-
 docs/UDP_LVDS_REASSEMBLY_PROTOCOL.md           |  29 +-
 tools/protocol-cli/src/main.rs                 |  69 +++-
 17 files changed, 1169 insertions(+), 243 deletions(-)
```

`Cargo.lock`、RS422 指令/解析文件、UDP 协议实现、storage 实现和桌面 GUI Mock 文件均无差异。没有执行重置或清理操作。本轮止于 CRC 门禁交付，后续可靠性与校准工作不自动开展。
