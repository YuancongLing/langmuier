# UDP LVDS 分片与重组协议

本文定义等离子体电子学 LVDS 数据经 UDP 转发后的分片封装、接收和重组规则。

> **科学数据未经 CRC 完整性验证。** 当前生产入口不计算或比较帧尾 CRC16；通过结构、时间和 ADC 解码及原有完整序号去重门禁的帧直接绘图，开启保存后进入正式 writer。CRC 模式、过滤开关、详情与独立诊断波形入口已移除。

生产程序直接接收 UDP 数据报；`.pcapng` 仅用于 Wireshark 抓包回放和离线验证，不属于实时传输协议。

## 传输概览

- 下位机采样周期：`50 ms`。
- 发送频率：`20 Hz`。
- UDP 源端点：`192.168.1.128:1534`。
- UDP 目的端点：`192.168.1.100:1534`。
- 一个 UDP 数据报由 24 字节分片头和一段 LVDS 原始数据组成。
- 多字节字段按网络字节序（大端）编码。

当前实测的单分片 UDP 数据报长度为 `293 B`：

```text
24 B UDP 分片头 + 269 B LVDS 原始数据 = 293 B
```

## LVDS 原始数据

等离子体电子学每 50 ms 产生一帧科学数据。原始 LVDS 数据的字段顺序为：

```text
同步头 | 发送地址 | 接收地址 | 有效数据长度 | 数据类型 |
UTC 时间戳 | 微秒偏移 | 数据域 | 校验码
```

| 字段 | 长度 | 编码 / 含义 |
| --- | ---: | --- |
| 同步头 | 2 B | `EB 90` |
| 发送地址 | 1 B | `55`，等离子探针 |
| 接收地址 | 1 B | `AA`，测控数传一体机 |
| 有效数据长度 | 2 B | `01 07` |
| 数据类型 | 1 B | `AA`，科学数据 |
| UTC 时间戳 | 4 B | 秒级 UTC，大端 |
| 微秒偏移 | 4 B | 大端 |
| 数据域 | 252 B | 两个通道各 125 个采样点 |
| 校验码 | 2 B | 有效数据的 CRC16，高字节在前 |

数据域编码如下：

- `DATA0..1`：通道 1 第一个点，`u16` 大端绝对值。
- `DATA2..3`：通道 2 第一个点，`u16` 大端绝对值。
- `DATA4, DATA6, ..., DATA250`：通道 1 后续点相对前一点的 `i8` 差分值。
- `DATA5, DATA7, ..., DATA251`：通道 2 后续点相对前一点的 `i8` 差分值。
- 每个通道的相邻采样间隔为 `400 us`，即 10K / 4 = 2500 samples/s，每帧 125 点占 50ms。
- 沿用 `i8` 补码差分；厂家负差值规则仍待确认。图表 mV 估算公式 `(ADC - 32768) / 6.4` 未完成厂家校准确认。

### 长度与校验约定

用户最终确认经重组后的 LVDS 原始帧为 `269 B`，帧尾保留 `2 B` CRC16 协议字段（高字节在前），当前接收不校验其值。`0x0107 = 263` 对应从 TYPE 到 CRC16 末尾的长度字段；离线 CRC 参考输入为 `frame[7..267]`，即 UTC、微秒和科学 DATA 共 260B，不含 TYPE。单分片外层 `frame_len` 和 `seg_len` 均为 269；生产科学帧入口拒绝其他总长。

269B 负载以 `EB 90 55 AA 01 07 AA` 开头。UDP `frame_seq` 不属于 LVDS 内部字段。所有正式接收入口在重组后只执行结构、时间和 ADC 验证，通过后参与正常去重、绘图及保存。保留的离线 CRC 参考参数为 `poly=0x1021`、`init=0xFFFF`、不反射、`xorout=0`；生产入口不计算或比较该值，不因尾部不匹配拒收，不修改字节或尝试位移纠偏。

260B 离线参考覆盖范围是新版“有效数据”的工作解释（`SPEC_ASSUMPTION`），厂家实际 `Calculate_CRC16(data, length)` 的起点与长度仍待确认；该范围不参与当前生产科学帧准入。

## UDP 分片头

每个 UDP 数据报的前 24 字节为分片头，后续 `seg_len` 字节为当前分片携带的 LVDS 原始数据。

```text
byte  0.. 1  magic        = 55 AA
byte  2       version      = 01
byte  3       type         = A5
byte  4.. 7  frame_seq     u32，帧序号，大端
byte  8..11  frame_len     u32，完整原始 LVDS 帧长度，大端
byte 12..13  seg_index     u16，当前分片序号，大端
byte 14..15  seg_count     u16，总分片数，大端
byte 16..19  byte_offset   u32，当前分片在原始 LVDS 帧内的字节偏移，大端
byte 20..21  seg_len       u16，当前分片的 LVDS 数据长度，大端
byte 22..23  flags         u16，状态标志，大端
byte 24..    payload       seg_len 字节 LVDS 数据
```

约束：

```text
udp_datagram_len == 24 + seg_len
seg_index < seg_count
byte_offset + seg_len <= frame_len
```

`flags` 的位定义尚未提供。接收端应保留该值用于日志和诊断，不应在未获得定义前据此丢弃数据。

## 历史 UDP 外层示例

下列为此前记录的 UDP 外层观测，仅说明封装字段。PCAP 回放仍经过相同 UDP/LVDS 生产解析链；其中结构、时间和 ADC 合法的帧可以绘图及正式保存，不能将此结果作为 CRC 或真实设备验收证据：

```text
192.168.1.128:1534 -> 192.168.1.100:1534
UDP payload length: 293 B
period: approximately 50 ms
```

一个实际 UDP 头示例：

```text
55 AA 01 A5 00 00 B5 37 00 00 01 0D
00 00 00 01 00 00 00 00 01 0D 00 00
```

其字段含义为：

| 字段 | 值 |
| --- | --- |
| `magic` | `55 AA` |
| `version` | `01` |
| `type` | `A5` |
| `frame_seq` | `0x0000B537` |
| `frame_len` | `0x0000010D`，269 B |
| `seg_index` | `0` |
| `seg_count` | `1` |
| `byte_offset` | `0` |
| `seg_len` | `0x010D`，269 B |
| `flags` | `0` |

该报文只有一个分片，`payload` 从 UDP 偏移 `24` 开始。单分片帧无需等待其他分片，但仍须执行所有头字段校验。

## 重组规则

每个重组器实例只服务一个经调用方过滤的源端点，实例内部以 `frame_seq` 为键；因此接收链路上的帧归属等价于 `(source_ip, source_port, frame_seq)`。

1. 校验 UDP 分片头的固定字段：`magic = 55 AA`、`version = 01`、`type = A5`。
2. 校验 UDP 数据报长度及各长度、偏移和分片序号约束。
3. 为该 `frame_seq` 建立长度为 `frame_len` 的重组缓冲区，并记录每个字节区间或每个 `seg_index` 的接收状态。
4. 将 `payload` 写入 `[byte_offset, byte_offset + seg_len)`。
5. 重复分片仅在内容完全一致时允许；同一字节区间内容冲突时丢弃该帧并记录错误。
6. 当所有 `seg_index` 均收到，且字节区间完整覆盖 `[0, frame_len)` 时，输出一帧重组后的原始 LVDS 数据。
7. 不能仅按 UDP 到达顺序拼接，必须按 `byte_offset` 放置；UDP 本身不保证顺序和可靠传输。
8. 不完整帧从首次分片到达起计时，单调时间年龄 `>= 2s` 时自动回收。新片或重复片均不续期；每次输入在解析和容量检查前先清理过期状态。此 TTL 为软件资源策略，不是厂家规定的通信超时。
9. 最多 64 个未完成帧。若清理后仍满，拒绝新序号并返回 `TooManyInFlightFrames`，不覆盖仍有效状态；已有序号仍可接收后续片并正常完成。过期回收后新帧可进入，无需重新监听。

### 不完整帧生命周期与诊断

默认 TTL 常量为 `DEFAULT_UDP_LVDS_REASSEMBLY_TTL`，可用 `new_with_ttl()` 显式配置非零值。采用首次创建的 `Instant`，通过 `checked_duration_since()` 比较年龄，不使用墙上时间或 `created_at + ttl`，避免校时与时间加法溢出。`push()` 使用当前时间；`push_at()` 与 `expire_at()` 允许确定性注入时间。

2s 为 40 个 50ms 周期：即使每周期产生一个残帧，稳定占用约 40 槽，64 槽还有短时突发余量，同时容许超过单帧周期的抖动。突发 64 槽全满且均未到期时仍会暂时拒绝新序号，这是明确的容量策略。

完成帧立即从重组表中移出并交付下一层，不留“完成待取”槽。冲突片延续原有删除逻辑；超时删除同时释放载荷、覆盖表及分片元数据，且只记一次。迟到分片可按其原 sequence 建立新的不完整状态，不继承过期字节，也不混入其他 sequence。完整科学帧的近期重复在下述独立门禁中处理，不改变重组层的 2s TTL；没有增加残帧 tombstone 或跨帧重排序。

桌面每次 UDP drain 开始时清理，因此空闲或持续异常输入也能回收；CLI 在接收循环及退出统计前清理，100ms socket 读超时使空闲清理无需新线程。只运行核心重组器而无任何调用时不会自行唤醒，调用方可显式使用 `expire()`。

核心 `metrics()` 提供等待帧数、预留 payload 字节、超时回收数、容量拒绝数、完整重组数和分片错误数。桌面沿用现有快照传递这些值，异常按 500ms 聚合；不逐次成功或清理输出日志。内层结构、时间或 ADC 解析错误与重组超时、容量错误分别计数；完整重组数包含随后解析失败的帧，不再产生生产 CRC 拒收计数。

预留 payload 字节不等于整个堆占用：通用默认上限仍为每帧 1MiB、最多 1024 片、64 槽，payload 至多 64MiB，覆盖表和分片元数据另有上述配置边界。桌面/CLI 在进入重组器前限制 269B，因此其未完成 payload 上限为 `64 × 269 = 17216B`。本轮未改变原有容量边界。

对于当前 `seg_count = 1` 的报文，步骤 3 至 6 在单次接收后即可完成：

```text
seg_index = 0
seg_count = 1
byte_offset = 0
seg_len = frame_len = 269
```

## 完整科学帧去重门禁（P1-2）

桌面 UDP/PCAP ingress 和 CLI capture 在完整重组后共用 `UdpLvdsScienceGate`：先执行全部 269B 结构、时间和 ADC 解码验证，通过后才登记已接受状态，不计算或比较 CRC16。结构、时间或 ADC 错误不写入缓存，所以同序号的合法重传仍可接收。原始字节流使用同一无 CRC 科学解码器，但没有可靠的 UDP `u32` 标签，本去重机制只用于保留了外层帧序号的数据报路径。

每个门禁对象只服务一个已过滤的源端点与一个接收会话；键为该会话内的完整 `u32 frame_seq`，保留全部 269B 逐字节比较，不使用截断的 `u16`、CRC 值或内容哈希替代完整内容。同序号同字节为重复，只计抑制，不调用正常 handler、不写 capture 文件。同序号但字节不同且科学验证合法为序号冲突，单独计数并拒绝交付，保留首条已接受内容。不同序号的相同字节允许分别交付。

缓存默认最多 256 帧、从首次接受起保留 5 秒（约 100 个 50ms 周期），均为软件策略。新输入及空闲轮询清理年龄 `>= 5s` 的记录，重复和冲突不续期；容量满时淘汰最早接受的记录，不按序号大小淘汰。缓存 payload 上限 `256 × 269 = 68864B`，索引与时间元数据另有同一容量上限。这是有界的近期重复防护：超过生命周期或被容量淘汰后，历史帧可以再次被接受，不承诺永久 exactly-once。

厂家尚未定义序号复位、回绕或重连复用语义（`SPEC_ASSUMPTION`）。软件将重新监听/新 CLI 采集调用视为新接收会话，销毁旧缓存；采集启停不重建会话。不依据序号倒退判断重启，不排序。`u32::MAX → 0` 的不同键正常接收；同会话短期复用已有序号时按重复/冲突规则处理，最迟原记录满 5 秒后解除。UDP 本身没有设备会话标记，重新监听后也无法辨认网络中迟到的旧会话包。

诊断区分完整重组数 `reassembled_frames`、科学解析通过数 `frames_decoded`（包括合法重复和冲突）、重复抑制数 `duplicate_frames_suppressed`、序号冲突数 `sequence_conflicts`、门禁回调数 `scientific_frames_delivered`。最后一项表示已调用正常处理回调，不等于存储落盘成功或 GUI 事件一定送达；停止保存后仍可绘图，但不提交 writer。重复不增加总拒收；冲突增加独立冲突计数与总拒收。主界面区分接收、解析成功、解析失败、绘图和实际保存数量。异常沿用 500ms 聚合日志，不逐帧刷屏。

## 完整序号质量观察（P1-3）

独立 `UdpLvdsSequenceMonitor` 只观察门禁返回 Accepted 的完整外层 `u32 frame_seq`，随后仍按原到达/重组完成顺序调用 handler 或写 CLI capture。结构/时间/ADC 错误、完整重复、合法序号冲突及错误来源均不推进观察状态；CRC 尾部值不影响 Accepted 或观察。它不读取科学模型的低 `u16`，不拒收、不重排序、不插入样本，也不修改重组 TTL 或去重缓存。

每个已过滤来源与接收会话使用独立监测器。首帧建立参考序号；用 `wrapping_sub()` 计算模 `2^32` 的距离，不转为有符号整数。前进 1 计连续；前进 2–256 且缺口总容量足够时，计一次向前跳号，并登记中间尚未观察到的序号。向前推进跨零同时计一次回绕，例如 `FFFFFFFE → FFFFFFFF → 0 → 1` 为三次连续、一次回绕。

缺口先等待 2 秒；期间收到该序号，移除候选，计一次迟到补齐及一次乱序，参考序号不后退。达到 2 秒边界后，累计一次等待到期事件，另保留 5 秒未解决历史；这期间补齐会降低当前未解决到期量，并增加“到期后补齐”，不会撤销历史到期事件。首次发现后达到 7 秒仍未补齐则遗忘记录。不续期，空闲轮询也维护状态。待补与到期历史合计最多 256 条。

先查询已有缺口，再判断方向。未匹配缺口且向后距离不超过 256 的帧计乱序；超出局部范围、半圈歧义或新增缺口将超出容量时，计无法判定异常，单独累计被遗忘的旧记录，并以当前帧重建观察参考，不猜测设备复位、不生成巨量缺口。门禁窗口外再次接受相同参考序号计异常，但不清除现有缺口。上述 2 秒等待、5 秒额外历史和 256 步范围均为软件策略（`SPEC_ASSUMPTION`），不是厂家序号或网络时延保证。

`ReaderMetricsSnapshot.udp_sequence` 和 UI 桥接中的同名嵌套指标包含累计观察/连续/跳号/候选/到期/迟到/乱序/回绕/异常/遗忘事件，以及当前等待、当前未解决到期两个 gauge。迟到补齐是乱序的子集，到期后补齐是迟到补齐的子集，不能将这些列相加作为帧总数。到期不等于永久缺失，更不证明 UDP 网络丢包：结构/时间/ADC 拒收、设备行为、会话变化等都可能造成有效科学帧未被观察。超出有界历史后无法继续关联原缺口。详细字段定义与历史回归证据见 [序号质量报告](UDP_LVDS_SEQUENCE_QUALITY_REPORT.md)。

## 接收伪代码

```rust
let socket = std::net::UdpSocket::bind("192.168.1.100:1534")?;
socket.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
let expected_source = "192.168.1.128:1534".parse::<std::net::SocketAddr>()?;
let mut reassembler = UdpLvdsReassembler::default();
let mut science_gate = UdpLvdsScienceGate::default(); // 与当前 source/session 一一对应
let mut sequence_monitor = UdpLvdsSequenceMonitor::default();
let mut buffer = vec![0_u8; 65_535];

loop {
    reassembler.expire(); // 空闲读超时也会回到这里
    let now = std::time::Instant::now();
    science_gate.expire_at(now);
    sequence_monitor.expire_at(now);
    let (size, source) = match socket.recv_from(&mut buffer) {
        Ok(result) => result,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock) => continue,
        Err(error) => return Err(error.into()),
    };
    if source != expected_source {
        continue;
    }

    let datagram = &buffer[..size];
    let Ok(parsed) = parse_udp_lvds_datagram(datagram) else {
        continue; // 实际接收器同时记录 UDP 分片错误
    };
    if parsed.header.frame_length != 269 {
        continue;
    }

    if let Ok(Some(raw)) = reassembler.push(datagram) {
        let now = std::time::Instant::now();
        let Ok(result) = science_gate.accept_at(&raw, now) else {
            continue; // 实际接收器分类计数，科学验证错误不登记缓存
        };
        if let Some(scientific) = result.into_frame() {
            sequence_monitor.observe_at(raw.frame_sequence, now);
            handle_scientific_frame(scientific); // 重复/序号冲突不会进入此处
        }
    }
}
```

运行中的接收器必须保留 UDP 数据报边界。`.pcapng` 回放工具也应逐个还原 UDP 负载，再调用同一套 `parse_udp_lvds_datagram` 和重组逻辑。

## 实时接收工具

工作区的 `langmuir-protocol-cli` 可直接接收、重组并保存原始 LVDS 帧：

```powershell
cargo run -p langmuir-protocol-cli -- `
  receive-udp-lvds 192.168.1.100:1534 192.168.1.128:1534 `
  20 .\data\udp-lvds-raw.bin 10
```

该命令保存两份文件：

- `udp-lvds-raw.bin`：按完成顺序连续写入、通过科学验证和近期去重门禁的重组原始帧。
- `udp-lvds-raw.bin.index.csv`：每帧的 `frame_sequence`、字节偏移和长度。

成功采集 20 个合法帧时生成 `20 * 269 = 5380 B`；结构、时间或 ADC 解析失败帧不计入目标数量、不写入该文件，CRC 尾部值不影响准入。命令结束或超时会报告科学帧拒收计数和最后错误，并标注 `CRC_NOT_CHECKED`。本命令不是保留全部异常 wire 数据的录制器。

命令结束或超时还报告分片错误、完整重组数、超时残帧、容量拒绝、等待帧和预留 payload 字节，以及科学解析通过、重复抑制、序号冲突、实际唯一帧写入数、去重缓存状态和 Accepted 科学帧序号质量。CLI 目标帧数只在通过去重后写入成功时增加，重复和冲突不占目标数量。序号观察不改变 raw/index 文件的写入顺序或列定义。桌面正式 `.lpd` v2 仍为 32B 头和1010B科学记录，HDF5 schema 不变；这些文件均未经 CRC 完整性验证。

去重与序号观察使用完整 UDP `u32`，但桌面科学模型和存储中的序号仍截为 `u16`，格式没有改变。跨帧排序、自动补样本、wire 原始保存和科学 CSV 继续留待独立任务。此前阶段证据见 [去重实施报告](UDP_LVDS_DEDUP_REPORT.md)、[TTL 实施报告](UDP_LVDS_REASSEMBLY_TTL_REPORT.md) 和 [CRC16 实施报告](LVDS_CRC16_IMPLEMENTATION_REPORT.md)。
