# LVDS 10bit 科学数据协议升级验收报告

本报告为 `04/FF` 待定协议阶段的历史记录。厂家最终地址已确认为 `55/AA`，当前协议和交付结果以 [LVDS_55AA_FINAL_ACCEPTANCE.md](LVDS_55AA_FINAL_ACCEPTANCE.md) 为准。中间阶段报红和离线诊断修复见 [REPLAY_DIAGNOSTICS_FIX_QA.md](REPLAY_DIAGNOSTICS_FIX_QA.md)；以下保留历史测试和包哈希，不代表当前地址规则。

日期：2026-10-24。协议依据为本轮用户提供的最新科学数据协议文字附件及其字段表；本轮将生产科学链切换为 `04/FF`、269B、双通道各 100 个 10bit 码值、500μs 间隔、内层 seq8。

本报告记录整工作区回归、Windows 原生回放、独立文件读回及非 Mock 发布包验证。文中的合成黄金帧和 `SYNTHETIC` 抓包均为测试夹具，不能证明真实厂家设备兼容。新版真实设备验收与受阻的可选 HDF5 测试标为 NOT_RUN。未自动连接硬件，未发送硬件控制命令，未提交 Git。

## 1. 新版 269B 科学帧布局

下表偏移从 0 开始，范围两端均包含。协议多字节字段为大端；这与后文 LPD 文件的小端布局不同。

| 偏移 | 长度 | 字段 | 本轮要求 |
|---|---:|---|---|
| 0–1 | 2B | Sync | `EB 90` |
| 2 | 1B | SRC | `04` |
| 3 | 1B | DST | `FF` |
| 4–5 | 2B | LEN | `01 07`，即 263 |
| 6 | 1B | TYPE | `AA` |
| 7–10 | 4B | 设备秒数 | BE u32，保留原值，不推定日历纪元 |
| 11–14 | 4B | 微秒偏移 | BE u32，必须小于 1,000,000 |
| 15–139 | 125B | 通道 1 打包数据 | 25 组 × 5B，解为 100 个 code10 |
| 140–264 | 125B | 通道 2 打包数据 | 独立解为 100 个 code10 |
| 265 | 1B | DATA250 | 原始内层科学帧序号 u8 |
| 266 | 1B | DATA251 | 保留字段，必须为 `00` |
| 267–268 | 2B | CRC16 尾字段 | 保留于输入帧；生产接收不计算、不比较 |

长度关系为 `1(TYPE) + 4(seconds) + 4(microseconds) + 252(DATA) + 2(CRC) = 263`，再加最初 6B 得到 269B。生产解码先验证完整长度、同步、地址、LEN、TYPE、保留字段和时间，再访问数据区。DATA250 与 DATA251 均不属于任一通道的 125B 打包区。

本轮严格使用 `04/FF`。旧 `55/AA` 内层地址会被拒绝，不同时接受两套地址，不依据采样内容猜测协议。外层 UDP 的 `55 AA` 标记仍属于原 24B 封装，两者不能混淆。

## 2. 新旧协议与科学模型差异

| 项目 | 旧版科学模型/存储 | 本轮新版 |
|---|---|---|
| 内层地址 | `55/AA` | `04/FF` |
| 每通道点数 | 125 | 100 |
| 采样解释 | 初值加 i8 差分恢复 | 连续高位在前的 4×10bit / 5B 解包 |
| 内存采样类型 | 两路 `[u32;125]` | 两路 `Box<[u16;100]>`，保存精确 code10 |
| 输出采样率 | 旧 400μs / 2.5kHz 模型 | 500μs / 每通道 2kHz |
| 新内层序号 | 不具备 DATA250 seq8 语义 | `frame_seq_u8` 直接取 byte 265 |
| 绘图幅值 | 旧 ADC/未经厂家确认的 mV 换算 | 有符号重建 ADC，未标定 |
| 科学 CSV | 旧每帧 125 行与旧字段 | 每帧 100 行，11 列 |
| 新采集 LPD | v2：32B 头、1010B 记录 | v3：40B 头、409B 记录 |
| HDF5 | legacy `[N,125]` | 保留旧 schema，新协议采集明确禁用 |

核心生产模型为 `ScientificFrame { timestamp, frame_seq_u8, channel_1, channel_2 }`，通道数组存放原始收到的 10bit 码值。统一常量为 `SAMPLE_COUNT=100`、`SAMPLE_INTERVAL_MICROS=500`。差分算法不再用于新科学帧。

RS422 控制协议、六条黄金控制指令、24B UDP 外层字段、源地址过滤、分片重组、2 秒残帧 TTL、5 秒/256 帧去重，以及 UDP P1-3 完整 u32 序号监测保持原有职责。本轮未实现 P1-4 Sidecar。

## 3. 10bit 解包及独立黄金向量

每通道 125B 被独立分为 25 组，每组五字节 `b0..b4` 还原四个 u16 码值：

```text
v0 = (b0 << 2) | (b1 >> 6)
v1 = ((b1 & 0x3F) << 4) | (b2 >> 4)
v2 = ((b2 & 0x0F) << 6) | (b3 >> 2)
v3 = ((b3 & 0x03) << 8) | b4
```

| 字面输入字节 | 独立期望 code10 |
|---|---|
| `FF C0 08 00 01` | `[1023, 0, 512, 1]` |
| `00 3F F0 03 FF` | `[0, 1023, 0, 1023]` |
| `00 00 00 00 00` | `[0, 0, 0, 0]` |
| `FF FF FF FF FF` | `[1023, 1023, 1023, 1023]` |

核心测试直接写入这些字面字节和期望值，覆盖首组、跨组、中间组、末组、首末点及两通道边界，不使用被测函数生成自身期望。两个通道采用不同数据，确认不会交换、串扰或把 seq8/保留字节当采样点。100 帧测试确认每通道共 10,000 点。

端到端与 Python 参考采用另一条计算路径：将五字节作为一个大端 40bit 整数，分别右移 30、20、10、0 位并取 `&1023`，与 Rust 生产字节掩码实现交叉核对。独立脚本为 [`tools/qa/lvds_10bit_reference.py`](../tools/qa/lvds_10bit_reference.py)。

## 4. 有符号 ADC 重建和量化限制

接收侧完整保存 `code10: u16`，重建函数使用 i32 进行有符号运算：

```text
adc16_reconstructed = (code10 << 6) - 32768
```

| code10 | 重建 ADC |
|---:|---:|
| 0 | -32768 |
| 1 | -32704 |
| 512 | 0 |
| 1023 | 32704 |

这是对 10bit 量化结果的有损重建，不能恢复发送前 16bit ADC 的全部低位。负值以有符号数保存和显示，不转换为会发生环绕的 u32；科学 CSV 同时保留 code10 和重建 ADC，LPD v3 保存 code10，读者可以按明确公式派生重建值。本轮不使用旧 mV 系数，也不增加未经厂家标定的电压列。

厂家给出的发送侧公式为 `(signed_adc16 + 32768 + 32) >> 6`。极端正值可能按此式得到 1024，而协议规定线上的码值为 0–1023；这一饱和/截断规则仍需厂家确认。接收端不据此修改已经收到的 10bit 码值，也未额外开发发送侧量化器。

## 5. 500μs 时间轴与首点语义

协议将秒数和微秒偏移定义为两个通道第一个输出平均采样点的时间。本轮按整数微秒计算：

```text
first_sample_time_us = u64(frame_seconds) * 1_000_000 + frame_microseconds
sample_time_us(k)    = first_sample_time_us + k * 500
k                   = 0..99
```

原始 10kHz 数据每五个样本平均为一个输出点，输出为每通道 2kHz、每帧 100 点、50ms 周期。`(5k+2)/10000` 中的 200μs 描述第一个平均样本相对原始采样帧起点的中心；既然传输时间戳已指向第一个输出点，软件不再额外加 200μs。

例如秒数 `9`、微秒 `999800`：首点为 `9,999,800μs`，第 1 点为 `10,000,300μs`，第 99 点为 `10,049,300μs`。两个通道使用同一首点时间，不增加通道间偏移。核心测试也覆盖最大 u32 秒数和非法微秒偏移，使用足够宽的整数处理跨秒。

保留原始秒数与微秒字段，不用 UDP 到达时间替代，不假设其纪元等于 Unix UTC，不自动转换为日历日期。设备时间回退仍按 Accepted 到达顺序保存，不排序、不删帧、不插入或伪造点；图表可开始新的显示窗口，文件保留全部原始时间与顺序。若厂家之后正式确认时间戳表示原始帧起点，需要依据更新后的正式协议重新裁定这项语义。

## 6. 内层 seq8、外层 u32 与 legacy u16

| 字段 | 来源/用途 | 本轮处理 |
|---|---|---|
| UDP 完整 u32 序号 | 原 24B 外层；分片、去重、P1-3 监测 | 保留完整 32 位，维持原算法 |
| LVDS 内层 u8 序号 | DATA250，offset 265 | `frame_seq_u8` 原样保存，支持 255→0 |
| 旧 LPD u16 序号 | legacy v2 文件记录 | 仅保留在 legacy 类型和旧格式中 |

三者不是同一字段。新模型不会截断 UDP u32 冒充 seq8，也不会由 seq8 合成旧 u16。具有相同内层序号但不同完整外层序号的合法帧，仍由原外层去重政策处理。内层序号异常不自动等同网络丢包。

核心和存储已有 seq8 255→0、时间回退保序测试；后端另增同时覆盖外层 u32 回绕、内层 u8 回绕和回退时间的 CSV/LPD 读回用例。该用例已进入最终工作区回归并通过，日志中为 `backend::protocol_10bit_tests::inner_sequence_wrap_and_device_rollback_preserve_csv_order_independently_of_udp ... ok`。

## 7. 实时波形、缓存与交互

后端由保存的 code10 派生有符号 ADC 后向图表发送，两通道各 100 点，间隔 0.0005 秒。图表纵轴及读数使用“重建 ADC（未标定）”，横轴使用设备秒数并能分辨 500μs；不套用日历纪元和旧 mV 换算。

每通道原始图表缓存容量按 `60 * 1_000_000 / SAMPLE_INTERVAL_MICROS + SAMPLE_COUNT = 120100` 点设置，保留 60 秒有界历史及一帧余量。峰谷保留抽稀、显示点预算与原始历史分离。科学文件始终直接接收 Accepted `ScientificFrame`，不从图表缓存、抽稀结果、归一化结果、冻结副本或读数导出。

保留双通道叠加/分屏、左键平移、滚轮缩放、FollowLatest/ManualView、冻结与重置。相关单元/交互测试已按 100 点和 500μs 更新，最终 desktop 133 项通过，包含新首点/signed ADC、500μs 设备时间轴、控件文案、视口与缓存边界测试。原生操作使用本轮新构建的静态 CRT 非 Mock EXE；截图没有沿用旧版 125 点产物。

| 本轮 UI 验收项目 | 状态/证据 |
|---|---|
| 新版波形、signed ADC 与设备时间轴 | Windows 原生运行；[保存中](qa/LVDS_10BIT_SAVING_WINDOWS.png)、[EOF](qa/LVDS_10BIT_EOF_WINDOWS.png)、[文件状态](qa/LVDS_10BIT_FILES_WINDOWS.png)，均为 1362×892 |
| 叠加/分屏、平移、缩放、跟随、冻结、重置 | 原生实操滚轮缩放、左键拖动、reset/恢复 follow、分屏；[分屏截图](qa/LVDS_10BIT_SPLIT_WINDOWS.png) 为 1362×892，冻结/恢复按钮可见；冻结状态和恢复语义由自动化用例覆盖 |
| 小窗口、更多菜单及完整文件路径 | [993×892 紧凑截图](qa/LVDS_10BIT_COMPACT_WINDOWS.png)，更多菜单和完整文件路径可读 |
| 2kHz/60 秒缓存边界 | headless `sixty_second_history_and_frozen_copy_have_explicit_memory_bounds` 通过，120100 点容量与冻结副本有界 |
| Windows 原生 65 秒合成回放性能测量 | PASS，1,300 帧全部解析/绘图；[原生 EOF/缓存截图](qa/LVDS_10BIT_PERFORMANCE_WINDOWS.png)、[性能统计](qa/LVDS_10BIT_NATIVE_PERFORMANCE.json) |

性能回放使用 `SYNTHETIC-65s-protocol.pcapng`（1,300 帧，478,448B，SHA-256 `5d63240e4de7432bcbe64a262cc9e86406436b26bd5b6c2cac360cbde2d403aa`），按 20 帧/秒、每通道 2kHz 实时节奏运行约 64.951 秒，关闭科学文件保存。通过原有可选 `LANGMUIR_PLOT_MEASUREMENTS` 记录 update 与绘图准备 CPU 耗时，正常退出后写出 `target/qa/lvds-10bit/native-plot-measurements.csv`。剔除空启动和 EOF 后空闲，仅统计 `0 < plotted < 1300` 的 3,893 次更新（覆盖 64.929081 秒）：update 中位数 1.1298ms、P95 2.3515ms、最大 6.7671ms；绘图准备中位数 0.0109ms、P95 0.1422ms、最大 1.0498ms。

两通道峰值/最终缓存各 120,000 点，低于 120,100 点容量；峰值渲染各 1,156 点，低于 2,000 点预算。以上测量反映本机该合成回放的应用更新 CPU 时间，不包含 GPU、呈现等待、整进程 CPU 占用率或工作集内存测量，也不推断其他机器的帧率。60 秒缓存的数据点数量及容量均已实际核对；真实设备持续接收仍为 NOT_RUN。

本次原生回放已验证 EOF 正常关闭。中途手动停止的关闭/排空行为主要由自动化覆盖，本报告不扩称此次 Windows 原生操作已单独完成手动停止验收。冻结/恢复按钮可见也不等同额外的原生冻结操作已验收。

## 8. 科学 CSV：字段、数据流和错误行为

科学 CSV 与原“导出遥测 (CSV)”不同：前者保存 LVDS 双通道采样流，后者是 RS422 遥测快照。科学 CSV 复用现有有界后台 `BatchWriter`，不在 egui 线程批量写文件，没有重建存储框架或新增 CSV 依赖。

标准 UTF-8 表头恰好 11 列：

```csv
frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity
```

| 字段 | 含义 |
|---|---|
| `frame_index` | 本文件从 0 开始的帧记录索引，按实际保存顺序 |
| `frame_seconds` | 原始设备秒数 u32 |
| `frame_microseconds` | 原始设备微秒偏移 u32 |
| `frame_seq_u8` | 内层 DATA250 原值，不是 UDP 完整序号 |
| `sample_index` | 0–99 |
| `sample_time_us` | 第 5 节整数微秒公式 |
| `ch1_code10` / `ch2_code10` | 同一采样索引的两个原始 10bit 码值 |
| `ch1_adc16_reconstructed` / `ch2_adc16_reconstructed` | 两个有符号重建 ADC |
| `integrity` | 固定 `CRC_NOT_CHECKED` |

每个完整科学帧输出 100 行，N 帧对应 `N*100` 数据行和 `N*100+1` 总行。两通道同一行配对，无平滑、插值、归一化或电压估算。文件以 `create_new` 独占创建，不覆盖同名文件。格式化缓冲只保留一帧的文本，再由固定容量 `BufWriter` 缓冲；每批 flush，不按点 flush，也不等到采集结束才统一写盘。

保存格式行为如下：

| 格式 | 输出 | 共同规则 |
|---|---|---|
| CSV | 仅科学 `.csv` | Accepted 原始科学流，每帧 100 行 |
| LPD | 仅新版 `.lpd` v3 | 100 点/通道的 code10 和原始时间/seq8 |
| LPD + CSV | 两份文件 | 同一 BatchWriter、同一帧顺序和保存区间 |

格式在一次保存期间固定。完整离线导出在回放前勾选“回放时保存科学数据”，先创建 writer 再交付第一帧，EOF 正常关闭。若在回放过程中才开始保存，只保存之后到达的帧。真实 UDP 仍由用户先手动监听，再开始/停止保存。

入队数、各文件实际成功写入的帧/行数和关闭状态分别记录。批次完成并成功 flush 后才增加已写计数；普通 flush 不承诺断电持久化。双格式任一输出失败时仍尝试同批次的另一输出，保留各自已写结果，停止本次保存、显示错误并写 `.incomplete` 标记。两个文件没有原子事务保证，失败末尾也可能存在未计入成功批次的部分行或记录；双格式共同成功前缀以两份确认帧数的较小值表示。

存储测试覆盖权限拒绝、空间不足和其他 I/O 错误的 error kind、失败后计数不虚增、禁止重试、显式 flush/close 错误，以及 10,000 帧/1,000,000 行按有界批次逐步落盘。后端另有双文件 write/flush/close 失败仍保留另一文件的测试。

## 9. LPD v3 文件格式与读回

新科学采集使用独立的 `BinaryV3Sink` / `BinaryV3Reader`。所有文件整数均为小端，文件长度为 `40 + N*409` 字节。与协议网络帧的大端字段严格分开。

### 9.1 40B 文件头

| 文件偏移 | 长度/类型 | 字段 | 固定值 |
|---|---|---|---|
| 0–7 | 8B | magic | ASCII `LMPROBE` 后接 `00` |
| 8–9 | u16 LE | version | 3 |
| 10–11 | u16 LE | header length | 40 |
| 12–15 | u32 LE | endian marker | `0x01020304`，落盘字节 `04 03 02 01` |
| 16–19 | u32 LE | samples per channel | 100 |
| 20–23 | u32 LE | sample interval μs | 500 |
| 24–27 | u32 LE | record length | 409 |
| 28–31 | u32 LE | representation | 1 = 原始 code10，容器 u16 |
| 32–35 | u32 LE | flags | 3 = bit0 + bit1 |
| 36–37 | u16 LE | channel count | 2 |
| 38–39 | u16 LE | sequence width bits | 8 |

flags 的 bit0 (`0x1`) 为 `CRC_NOT_CHECKED`；bit1 (`0x2`) 为原始设备时间、未假定日历纪元。当前读者严格要求 flags=3，不对未知元数据或其他版本自动猜测。

### 9.2 每帧 409B 记录

以下偏移相对每条记录起点；第 i 条记录从文件绝对偏移 `40+i*409` 开始。

| 记录内偏移 | 长度/类型 | 字段 |
|---|---|---|
| 0–3 | u32 LE | 原始设备秒数 |
| 4–7 | u32 LE | 原始微秒偏移 |
| 8 | u8 | 原始 `frame_seq_u8` |
| 9–208 | 100×u16 LE | 通道 1 code10，顺序 0–99 |
| 209–408 | 100×u16 LE | 通道 2 code10，顺序 0–99 |

记录不补虚假点，不生成 legacy u16 序号，不声称保存原始 16bit ADC。CRC 尾字节并非 `ScientificFrame` 科学模型字段，CSV/LPD 不把尾字节解释为验证通过；其未校验状态由 CSV integrity 或 v3 flags 明确记录。

写入前检查整批帧的微秒合法性和 code10≤1023，成功 flush 后才增加计数，I/O 失败后终止该 sink。Reader 校验 magic、版本、头长、点数、间隔、表示方式、flags、通道数和序号位宽；逐条读回只在准确记录边界返回 EOF，截断头或截断记录报错，也拒绝非法微秒/码值。内存按单条 409B 记录和两路固定数组有界使用。

独立字节夹具验证读者，独立预期布局验证写者；测试覆盖 v2/未知版本拒绝、每项错误元数据、截断、非法值、现有文件不覆盖，以及 seq8 回绕/设备时间回退后记录顺序不变。

## 10. legacy v2 与 HDF5 的兼容边界

原 `BinarySink` 和 `BINARY_*` v2 常量保留 32B 文件头、1010B 记录、两路各 125 个 u32 ADC、400μs 元数据和旧 u16 序号。它们现在仅接收明确命名的 `LegacyScientificFrameV2`；v2 的磁盘布局及兼容测试没有改为新版布局。

新版 `ScientificFrame` 不实现到 legacy 的自动转换或补点。新采集只使用 CSV 或 v3，不把 100 点填成 125 点，不沿用版本 2 标记，也不改动用户旧文件。仅能读 v2 的外部分析程序必须按上节实现 v3 支持后才可读取新 `.lpd`；v3 reader 刻意拒绝 v2，而非宣称两者可互换。

可选 HDF5 的原 dataset 名称、标量类型及 `[N,125]` schema 保留，接口同样仅接受 legacy 类型。本轮没有实现新 `[N,100]` HDF5：UI 明确禁用并显示“尚未适配新版 100 点”原因，后端也返回 Unsupported，不能静默补点。

HDF5 feature 实际测试为 **NOT_RUN**：`hdf5-metno-src 0.10.2` 未缓存，尝试下载因 `static.crates.io` DNS 失败而受阻。保留旧 schema 的源码审计不等于此环境已执行 HDF5 回归，不记录为 PASS。

## 11. CRC 不校验策略

生产 `decode_scientific_frame`、UDP Accepted 链、实时图表和正式存储只执行结构/时间/解包检查，不计算或比较 CRC，不因改变 offset 267–268 的两个尾字节拒收。界面继续显示“科学数据未经 CRC 完整性验证”，CSV 为 `CRC_NOT_CHECKED`，v3 设置对应 flags。

独立的离线 CRC 参考函数不参与生产接受门禁。本轮未恢复严格/诊断双模式、CRC UI 开关、CRC 自动修复或字节纠正。取消 CRC 不会取消地址、长度、类型、保留字段、时间及数组边界验证；非法结构或时间仍不能进入绘图和存储。

## 12. 旧 PCAP 拒收与新协议合成文件验收

### 12.1 旧抓包只作安全拒收证据

需求中原 `2026101902--朗缪尔.pcapng` 的旧“1053 帧”结果不能复用为新协议通过标准。本轮实际执行旧文件边界检查的文件为 `102301.pcapng`，非 Mock 忽略测试 `e7_user_capture_no_crc_science_roundtrip` 的日志记录：

| 指标 | 实际值 |
|---|---:|
| 总包 / 目标 UDP / 完整科学候选帧 | 153 / 153 / 153 |
| 外层合法 | 153 |
| 新版结构合法 / parsed / Accepted | 0 / 0 / 0 |
| field errors / parse failures | 153 / 153 |
| 绘图事件 / 两路点数 | 0 / 0、0 |
| 入队 / 实际存储帧 | 0 / 0 |
| 输出 LPD 大小 | 40B，仅正常关闭的 v3 头 |

该拒收测试 **1 passed**，证明旧输入不会放宽地址后伪装成新版科学数据；不能解读为新设备采集通过。日志位于 `target/qa/lvds-10bit/old-pcap-rejection.log`。

用户文件没有改写。两份旧 PCAP 的收尾 SHA-256 复核均与操作前一致：`102301.pcapng` 为 `c0fe65e69ca8efe4f98fedd2758d6cb0a043e840ccf6e0814ad6acb1f7b76e6f`，`20261019--朗缪尔.pcapng` 为 `3f7d3bb2c78f7757c3f04d6bd946e6f6710a76271b245a065e8aac7624d1a2ea`。后者文件名与需求引用的历史名称不同，不据此混用历史包数。

### 12.2 明确标记的合成新协议回放

`tools/qa/lvds_10bit_reference.py` 使用 Python 标准库独立打包 40bit 采样，生成文件名带 `SYNTHETIC` 的 200 帧 PCAPNG，拒绝覆盖已有文件。首帧使用两组厂家字面黄金向量，后续为两通道不同的波形码值；第 100 帧（从 0 计）开始设备时间回退。CRC 尾字段有变化且未重算，测试接收链仍不以 CRC 为门禁。

非 Mock 的 `e8_user_capture_scientific_csv_all_modes_readback` 实际执行 **1 passed**，CSV 与 LPD+CSV 两种包含 CSV 的模式均报告 200 Accepted、20,000 数据行、20,001 总行、正常关闭；LPD-only 的行为及二进制逐帧检查由该验收流程和存储测试覆盖。日志为 `target/qa/lvds-10bit/synthetic-all-modes.log`。

| 产物 | 字节数 | SHA-256 |
|---|---:|---|
| `target/qa/lvds-10bit/SYNTHETIC-new-protocol.pcapng` | 73,648 | `f6ce758b9e93382eabfb1a5d8137ff8e8165fb16593ad5e6e6fb1184be0adf0a` |
| `target/qa/lvds-10bit/capture-readback/scientific-20261024-145100.419.csv` | 1,338,897 | `170760edd4c885c97ca0074e8527aeabf7d3994986f1bb9c91dd964784c07520` |
| `target/qa/lvds-10bit/capture-readback/scientific-20261024-145100.419.lpd` | 81,840 | `351ee8f2f67f18a64c24c51acc9e0aaf9f2e9489445e0be73a12781f3b381828` |

LPD 长度与 `40+200*409=81,840` 一致。Python 参考逐行检查 CSV 的 11 列、全部 20,000 对码值/40,000 个 signed ADC、整数时间、seq8 和末尾 EOF；逐记录检查 v3 头、两路各 100 点、原始时间和顺序，结果 **PASS**。机读证据为 [`docs/qa/LVDS_10BIT_INDEPENDENT_READBACK.json`](qa/LVDS_10BIT_INDEPENDENT_READBACK.json)。

回放检查的设备时间回退索引为 `[100]`；首帧 `(seq8=255, seconds=148, microseconds=999800)`，日志抽查中间帧为 `(73,152,699800)`，末帧为 `(198,156,949800)`。时间回退前后仍保存全部 200 帧、20,000 行，未按时间排序、删帧或补点。

需要重做独立读回时，可在仓库根目录运行以下只读验证命令；它不会生成或覆盖抓包：

```powershell
& C:\Users\Administrator\miniconda3\python.exe tools/qa/lvds_10bit_reference.py `
  target/qa/lvds-10bit/SYNTHETIC-new-protocol.pcapng `
  --csv target/qa/lvds-10bit/capture-readback/scientific-20261024-145100.419.csv `
  --lpd target/qa/lvds-10bit/capture-readback/scientific-20261024-145100.419.lpd
```

### 12.3 Windows 原生 GUI 回放与独立读回

本轮静态 CRT、x64、非 Mock EXE 已在 Windows 原生运行。GUI 选择同一份明确标记的 200 帧合成 PCAP、LPD + CSV，并在第一帧处理前启动保存；到 EOF 显示 200 帧、20,000 CSV 数据行，两个输出正常关闭。原生保存产生 `data/scientific-20261024-150357.221.csv` 和同名 `.lpd`，其字节数及 SHA-256 分别与上表 CSV/LPD 完全相同。

独立脚本再从该次 GUI 实际输出读回全部 20,000 行、40,000 个有符号 ADC，校验每列、两通道 code10、seq8、500μs 时间、回退索引 `[100]` 和全部 v3 记录，结果 PASS。机读证据为 [`docs/qa/LVDS_10BIT_NATIVE_READBACK.json`](qa/LVDS_10BIT_NATIVE_READBACK.json)，截图见第 7 节。这项证据确认真实 Windows 软件的回放与写文件行为，输入来源仍是合成夹具。

以上均为合成文件的真实磁盘读回，不是新版真实厂家抓包验收。**新版真实设备 PCAP、持续真实 UDP 和厂家输出对照均为 NOT_RUN**，原因是尚未提供新协议实测数据且本轮没有连接设备。

## 13. 异常、生命周期与回归覆盖

已实现的定向测试覆盖：269B 固定字段、旧地址拒绝、DATA251 非零、短/长帧、非法微秒、任意采样位模式有界解包、字面黄金向量、双通道边界、ADC 极值、500μs 跨秒、首点不重复 200μs、内层 seq8 回绕、设备时间回退、CRC 尾变化接受、UDP 完整 u32 去重/回绕/监测、残帧 TTL、100 帧/每通道 10,000 点、图表拥塞时科学文件仍完整保存。

文件与存储覆盖：单帧 100 行、100 帧 10,000 行、长期有界逐批落盘、同名不覆盖、用户停止、回放 EOF、创建失败、权限/空间不足、write/flush/close 错误、另一输出文件保留、关闭与不完整标记，以及 v3 精确布局/元数据/截断/非法值拒绝。

回归期间还定位到 transport pipeline 测试夹具的确定性容量问题：4 个帧事件外还有 Started 和 Stopped，原 `event_capacity=4` 无法容纳 6 个事件；慢消费者时 Stopped 被丢弃，RED 证据为 `Stopped dropped=1` 后接收端 Disconnected。仅将该测试容量修为 `FRAME_COUNT+2`，并先 join 生产者再 drain 以固定慢消费者顺序，未修改生产队列政策。GREEN 为 transport 25 项通过，该用例额外重复 100 次均通过；最终工作区回归也包含此修正。

最终结果、原生交互和发布状态集中记录在下一节。

## 14. 自动化验证、Clippy 与 Windows 发布

### 14.1 最终工作区与独立验收结果

| 验证 | 最终结果 | 范围/证据 |
|---|---|---|
| 工作区合计 | **276 passed、0 failed、2 ignored** | `target/qa/lvds-10bit/workspace-tests.log`，两个 ignored 是需外部 PCAP 的验收用例，另行执行如下 |
| `langmuir-core` | 88 passed | 包含协议、UDP 边界与六条 RS422 黄金指令 |
| `langmuir-desktop` | 133 passed、2 ignored | 包含后加内外序号回绕/时间回退用例、UI/布局/图表与后端存储 |
| `langmuir-protocol-cli` | 6 passed | 黄金命令、LVDS 与序号展示 |
| `langmuir-storage` | 24 passed | CSV、v3、legacy v2 和有界长期写入；不含受阻 HDF5 feature |
| `langmuir-transport` | 25 passed | 含 pipeline 测试容量修正；另对该测试重复 100 次通过 |
| 非 Mock E7：旧 `102301.pcapng` | 1 passed | 153 帧全部安全拒收，不代表新硬件通过 |
| 非 Mock E8：SYNTHETIC 全格式文件回放 | 1 passed | 合成 200 帧，文件读回，非真实设备 |
| Python 独立 CSV/LPD v3 读回 | PASS | 自动化输出和 Windows 原生 GUI 输出分别全量核对：各 200 帧、20,000 行、40,000 个 signed ADC |
| 可选 HDF5 feature | NOT_RUN | 未缓存依赖且 crates.io DNS 失败 |

### 14.2 最终命令和严格 Clippy

工具链为 rustc/cargo 1.99.1。以下命令均在本轮最终源码上执行，日志目录为 `target/qa/lvds-10bit/`。

| 命令/项目 | 最终状态 | 日志 |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | `fmt.log` |
| `cargo check --workspace --locked` | PASS，7.05s | `check.log` |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS，276 passed / 0 failed / 2 ignored | `workspace-tests.log` |
| `cargo build --release -p langmuir-desktop --locked` | PASS，非 Mock，35.81s | `release-default.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **FAIL，既有基线诊断** | `clippy-strict.log` |
| Desktop `--no-deps` 严格 Clippy 定向审计 | **FAIL，34 项，其中 19 项生产代码诊断，均对应 HEAD 基线** | `clippy-desktop.log` |

工作区严格 Clippy 失败明确包括 legacy `crates/langmuir-storage/src/binary.rs:149/152` 的两处显式 `.iter()`，`crates/langmuir-core/src/framing.rs:288/290` 的空集合断言，以及 `tools/protocol-cli/src/main.rs:330` 的 `parse_hex` / `chunks_exact(2)`。Desktop 审计另比较 HEAD 原有诊断，确认剩余 34 项均为基线；`show_scientific_plot` 的 104/100 行数诊断也与 HEAD 一致，不是本轮新增超限。

本轮新增的三处固定大小 chunks 用法及一处 is_empty 诊断已修正，未添加 allow，也未放宽 lint。因既有严格诊断仍存在，不能宣称 Clippy PASS。正常 check、测试与 Release 构建通过不替代严格 Clippy 的 FAIL 结果。测试队列竞态的确定 RED、根因与 100 次重复 GREEN 记录位于 `target/qa/lvds-10bit/pipeline-fixture-race.md`。

### 14.3 Windows x64 非 Mock 交付

构建方式依据 [`docs/WINDOWS_BUILD.md`](WINDOWS_BUILD.md)：使用原生 MSVC x64 工具链、`-C target-feature=+crt-static`，并以 `--no-default-features --locked` 构建真实后端，不启用 Mock 或可选 HDF5。

```powershell
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop `
  --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

上述原生 MSVC 静态 CRT 构建由最新源码完成，PASS，耗时 32.35s，日志为 `target/qa/lvds-10bit/release-static.log`。未复用旧版 125 点 EXE，未启用 Mock 或 HDF5。构建、实际原生回放和硬件验收分别记录。

| 交付/原生项目 | 最终状态与产物 |
|---|---|
| Windows x64 MSVC 非 Mock Release | PASS，rustc/cargo 1.99.1，静态 CRT 构建退出码 0，32.35s |
| 原生启动、双通道回放与 EOF | PASS，200 帧/20,000 CSV 行、两个文件关闭；手动停止主要为自动化覆盖，未扩称原生实操 |
| 本轮 UI 截图 | 第 7 节链接六张新截图；常规 1362×892、紧凑 993×892 |
| `target/delivery/windows-x64/LangmuirGroundControl.exe` | 9,261,056B；SHA-256 `97454c47f21cc997962e00b78353757df46657ae64af5c2da425b820a2c5bbea` |
| 交付 `README.md` | 最新 `docs/WINDOWS_BUILD.md` 的字节相同副本，6,064B；SHA-256 `668789dc37c96402e9b0026a4a7f69410b7d1cdf34a8f8082b0a07073393cf0d` |
| `target/delivery/LangmuirGroundControl-windows-x64.zip` | 3,897,240B；SHA-256 `6f81f0d3880853f8ec9cdbf5e193625b763c315789e4122948ac3c9f6bc62500`；两个条目与源文件逐字节相同 |
| 新版厂家抓包、真实硬件接收 | NOT_RUN：未提供新版实测数据，未连接硬件 |

发布包验证为 PASS，机读证据见 [`docs/qa/LVDS_10BIT_WINDOWS_PACKAGE.json`](qa/LVDS_10BIT_WINDOWS_PACKAGE.json)。PE Machine 为 `0x8664`、格式为 PE32+ Windows x64；导入表未包含动态 MSVC CRT，符合静态 CRT 构建设置。ZIP 仅含 `LangmuirGroundControl.exe` 和 `README.md`，CRC 与解压后的内容均核对，交付 EXE 与本轮 MSVC 源产物字节相同。以上不把合成回放解释为真实新硬件验收。

## 15. Git 变更清单、待确认事项和任务边界

下表为报告收尾时 `git status --short` / `git diff --name-only` 的改动范围；用户原有未提交文件全部保留，清单不表示这些文件均由本轮从零新增。没有自动 add、commit 或 push。

| 范围 | 文件 |
|---|---|
| 核心生产模型/解码 | `crates/langmuir-core/src/lvds.rs`、`lib.rs`、`model.rs`、`framing.rs` |
| 新核心测试夹具 | `crates/langmuir-core/src/lvds_protocol_tests.rs`、`lvds_test_support.rs` |
| 外层相关测试/新内层类型适配 | `crates/langmuir-core/src/udp_lvds.rs`、`udp_lvds_dedup.rs`、`udp_lvds_sequence.rs` |
| 科学 CSV/v3/legacy 存储 | `crates/langmuir-storage/src/scientific_csv.rs`、`binary_v3.rs`、`legacy.rs`、`binary.rs`、`hdf5.rs`、`lib.rs` |
| 真实后端/文件生命周期 | `apps/desktop/src/backend.rs`、`backend_scientific_storage.rs` |
| 后端验收测试 | `apps/desktop/src/backend_10bit_tests.rs`、`backend_e2e_tests.rs`、`backend_replay_tests.rs`、`backend_scientific_csv_tests.rs` |
| UI/图表/测试夹具 | `apps/desktop/src/app.rs`、`app_plot_tests.rs`、`app_layout_tests.rs`、`bridge.rs`、`buffer.rs`、`mock.rs` |
| Transport/CLI | `crates/langmuir-transport/src/core_adapter.rs`、`mock.rs`、`pipeline_test.rs`、`udp.rs`、`examples/replay_bench.rs`；`tools/protocol-cli/src/main.rs` |
| 文档/独立验证 | 本报告、`docs/WINDOWS_BUILD.md`、`docs/superpowers/plans/2026-10-24-lvds-10bit-migration.md`、`docs/qa/LVDS_10BIT_INDEPENDENT_READBACK.json`、`LVDS_10BIT_NATIVE_READBACK.json`、`LVDS_10BIT_NATIVE_PERFORMANCE.json`、`LVDS_10BIT_WINDOWS_PACKAGE.json`、六张 `LVDS_10BIT_*_WINDOWS.png`、`tools/qa/lvds_10bit_reference.py` |
| 只读用户输入与测试产物 | 用户旧 PCAP 保留；自动化文件移至 `target/qa/lvds-10bit/capture-readback/`，原生 GUI 文件在 `data/`；均为本机验证产物，不标为正式设备采集 |

本轮没有新增依赖；收尾检查 Cargo manifest/lock 和受保护的 RS422 文件 diff 无变化。两份用户旧 PCAP 哈希也复核不变。Mock 仅用于自动化夹具；非 Mock 后端不会自动生成科学数据，回放来源与实时 UDP 来源分别标识。

仍需厂家/实测确认：图片中待定的 SRC/DST 是否最终为 `04/FF`；设备秒数字段的纪元；帧时间确实表示首个输出平均点；发送侧极端正值量化的饱和规则；真实新协议抓包与硬件输出一致性。CRC 接收策略仍按用户指令保持不校验，不能将上述待确认事项作为擅自放宽字段或自动猜测版本的理由。

本轮完成协议升级、全量回归、原生合成回放、独立文件读回和 Windows 非 Mock 发布包验证。严格 Clippy 保留基线 FAIL；HDF5 feature 与新版厂家数据/硬件保留 NOT_RUN。完成本阶段后停止，不进入其他开发阶段，不提交 Git。
