# UDP LVDS 分片与重组协议

本文定义等离子体电子学 LVDS 数据经 UDP 转发后的分片封装、接收和重组规则。

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
- 每个通道的相邻采样间隔为 `400 us`，即 `50 ms / 125`。

### 长度与校验约定

当前网口协议的固定约定是：经重组后的 LVDS 原始帧为 `269 B`，帧尾校验码为 `2 B` CRC16（高字节在前）。`0x0107 = 263`，表示从数据类型到 CRC16 末尾的有效数据长度；CRC 覆盖 CRC 字段之前的有效数据。实测 UDP 分片头中的 `frame_len` 和 `seg_len` 均为 `0x0000010D`（269），接收端必须以该长度作为帧边界。

实包重组后的 269 字节负载以 `EB 90 55 AA 01 07 AA` 开头。UDP 分片头中的 `frame_seq` 用于重组和采集序号，不属于 LVDS 原始帧字段。CRC16 参数为 `poly=0x1021`、`init=0xFFFF`、不反射、`xorout=0`。按本文“有效数据”范围复算时，实包尾部 CRC 尚不匹配，因此接收端解析并保留 CRC 字段，待下位机确认 `Calculate_CRC16(data, length)` 的实际起点和长度后启用实时拒收。

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

## 当前抓包示例

Wireshark 已验证连续数据报：

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

接收端以 `(source_ip, source_port, frame_seq)` 作为一帧重组状态的键。

1. 校验 UDP 分片头的固定字段：`magic = 55 AA`、`version = 01`、`type = A5`。
2. 校验 UDP 数据报长度及各长度、偏移和分片序号约束。
3. 为该 `frame_seq` 建立长度为 `frame_len` 的重组缓冲区，并记录每个字节区间或每个 `seg_index` 的接收状态。
4. 将 `payload` 写入 `[byte_offset, byte_offset + seg_len)`。
5. 重复分片仅在内容完全一致时允许；同一字节区间内容冲突时丢弃该帧并记录错误。
6. 当所有 `seg_index` 均收到，且字节区间完整覆盖 `[0, frame_len)` 时，输出一帧重组后的原始 LVDS 数据。
7. 不能仅按 UDP 到达顺序拼接，必须按 `byte_offset` 放置；UDP 本身不保证顺序和可靠传输。
8. 超时未完成的帧应丢弃。推荐超时为 `250 ms`（五个采样周期），并记录源端点、`frame_seq`、已收分片数和缺失区间。

对于当前 `seg_count = 1` 的报文，步骤 3 至 6 在单次接收后即可完成：

```text
seg_index = 0
seg_count = 1
byte_offset = 0
seg_len = frame_len = 269
```

## 接收伪代码

```rust
const UDP_HEADER_LEN: usize = 24;

let socket = UdpSocket::bind("192.168.1.100:1534").await?;
let mut buffer = vec![0_u8; 65_535];

loop {
    let (size, source) = socket.recv_from(&mut buffer).await?;
    if source.ip() != "192.168.1.128".parse()? || source.port() != 1534 {
        continue;
    }
    if size < UDP_HEADER_LEN {
        continue;
    }

    let datagram = &buffer[..size];
    let header = parse_udp_lvds_header(&datagram[..UDP_HEADER_LEN])?;
    if datagram.len() != UDP_HEADER_LEN + usize::from(header.seg_len) {
        continue;
    }

    reassembler.push(source, header, &datagram[UDP_HEADER_LEN..])?;
}
```

运行中的接收器必须保留 UDP 数据报边界。`.pcapng` 回放工具也应逐个还原 UDP 负载，再调用同一套 `parse_udp_lvds_header` 和重组逻辑。

## 实时接收工具

工作区的 `langmuir-protocol-cli` 可直接接收、重组并保存原始 LVDS 帧：

```powershell
cargo run -p langmuir-protocol-cli -- `
  receive-udp-lvds 192.168.1.100:1534 192.168.1.128:1534 `
  20 .\data\udp-lvds-raw.bin 10
```

该命令保存两份文件：

- `udp-lvds-raw.bin`：按完成顺序连续写入的重组原始帧。
- `udp-lvds-raw.bin.index.csv`：每帧的 `frame_sequence`、字节偏移和长度。

当前实测的单分片流中，采集 20 帧应生成 `20 * 269 = 5380 B` 的原始帧文件。
