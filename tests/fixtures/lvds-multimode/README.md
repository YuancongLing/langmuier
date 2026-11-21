# 真实多模式 LVDS 小型回归样本

`REAL-SUBSET-202611212141.pcapng` 是真实抓包的 **SUBSET（抽样子集）**，不是合成波形，也不是完整连续采集记录。文件为 **19,548 字节（约 19.1 KiB）**，可用于快速检查 AA/AB/AC 分流、AB 模式显示、无逐点时间状态及历史导航。

|内容|数量|
|---|---:|
|UDP|53|
|AA 科学帧|26，每通道 2600 点|
|AB 激励/状态帧|13，保留原始 1300 个 wave 槽位|
|AC 公共帧|13|
|其它来源 UDP|1|
|AB 连续配置阶段|11|
|有逐点采样时间的 AB|8 帧 / 800 点|
|无逐点采样时间的 AB|5 帧 / 500 槽|

原始 `202611212141.pcapng` 为 1,416,044 字节，SHA256 为：

`9a5d9a73aa46fb2fb24962b2e2b7d4081646477ae5d1f62ee2ee46c375b53907`

本样本 SHA256 为：

`3ed428dee322ebc06e7a2aef431c802bb03555476b9b3f4659528d12245cfd03`

## 查看方法

在软件中选择“打开历史”，打开本目录的 PCAPNG，再选择“科学波形 AA”或“激励波形 AB”。在“AB 详情”中使用上一帧、下一帧或指定帧号；可从“AB CSV”导出全部 AB 槽位。表中索引从 **0** 开始。

也可从仓库根目录只读检查，不连接硬件：

```powershell
cargo run --release -p langmuir-desktop --no-default-features --locked -- --audit-pcap tests/fixtures/lvds-multimode/REAL-SUBSET-202611212141.pcapng
```

|样本 AB 索引|原始 AB 索引|真实配置|实际 Hz / 间隔 μs|
|---|---|---|---|
|0|0|三角波 0Hz 状态|0 / 0，无逐点时间|
|1|21|三角波 1Hz|0.931 / 10800|
|2|34|三角波 50Hz|49.918 / 200|
|3|47|三角波 100Hz|99.930 / 200|
|4|60|正弦波 100Hz|99.930 / 200|
|5–7|70、72、73|正弦波 50Hz，含 PA0 回绕|49.918 / 200|
|8|81|正弦波 1Hz|0.931 / 10800|
|9|114|DC 1.000V 设定|0 / 0，无逐点时间|
|10|132|DC 3.000V 设定|0 / 0，无逐点时间|
|11|149|DC 4.500V 设定|0 / 0，无逐点时间|
|12|165|DC 0.000V 设定|0 / 0，无逐点时间|

样本 AB 索引 6→7 的 PA0 为 255→0。每个选中 AB 均保留最近的前一个 AA、后一个 AA 及后一个 AC；额外保留第一条其它来源 UDP。DC 电压是设定值；无时间帧的零槽不能视作实测 0V。

**抽样产生的外层 u32、AA seq8、AB PA0 和捕获时间间隔是预期现象，不代表原始抓包丢包。** 原始时间未压缩，按时间回放仍会等待原有间隔；快速检查各模式应使用历史帧选择。此样本适用于分支和显示回归，不适用于完整采集吞吐、丢包、长期连续性或真实硬件在线验收。

## 来源和复现

[provenance.json](provenance.json) 保存每个样本包对应的原始包索引（0 起始）、原文件块偏移、捕获时间、原始完整块 SHA256、UDP Payload SHA256、外层序号，以及 AB 配置字段。没有把完整逐点大 manifest 加入此目录。

提取过程原样保留 SHB、IDB 及选中 EPB 的完整字节，包含 Ethernet/IP/UDP、原始 CRC、选项和时间戳；`if_tsresol=9` 仍为纳秒。原文件末尾的 Interface Statistics Block 描述完整抓包总量，因此未收入子集。没有改写任何包来补齐序号或时间。

从仓库根目录运行下列命令可重新生成。输出路径必须不存在；工具拒绝覆盖输入或已有输出。

```powershell
python tools/qa/extract_lvds_multimode_subset.py --input 202611212141.pcapng --output target/qa/subset-check/REAL-SUBSET-202611212141.pcapng --provenance target/qa/subset-check/provenance.json
```

只使用 Python 标准库，选择规则从原始包字段动态推导：每个连续配置阶段首个 AB，加上所有 PA0 255→0 的两端，再保留前后科学帧及辅助帧。没有依赖本地绝对路径或完整验收 manifest。

[verification.json](verification.json) 记录本次核对结果：独立现有 Python 读取器逐包比较原始 UDP 载荷、端点与时间；识别全部 11 阶段；52 个 FPGA 帧的离线参考 CRC 均匹配；正式非 Mock EXE 的只读审计得到 AA26/AB13/AC13，业务错误为0。再次提取的文件字节完全一致，已有输出拒绝覆盖。

生产 CRC 策略仍为 `CRC_NOT_CHECKED`。EXE 检查仅验证命令行解码，本样本的验证记录不宣称已完成原生 Windows GUI 点击、保存操作或真实硬件联调。EXE、构建产物、压缩包及完整原始抓包不包含在此目录。
