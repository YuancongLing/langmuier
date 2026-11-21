# RS422 星务计算机模拟器 QA

日期：2026-11-16（Asia/Shanghai）。结果：软件实现、受控 Transport 完整链路、原生 GUI 和新 Windows 发布包验证通过。**真实探针、真实 COM/RS422 电气时序及长期硬件联调未执行**。

## 1. 原始协议与版本

依据用户补充的 `星务与等离子体密度探针单机RS422串行通信协议-513反馈-20260728（公开）.doc`，1029632 B，SHA-256 `d1d7c6bfb415836a47958e406e1fc0a70ddcc51a60200ca347c17e43f9dae050`。Microsoft Word COM 只读提取全文，统计 24 页、16 表；已经完整阅读表 2-5～2-9 及第 3、4、5 节。[完整 protocol-20260728-source.txt](qa/star-simulator/protocol-20260728-source.txt)、[完整 protocol-20260728-readable.txt](qa/star-simulator/protocol-20260728-readable.txt)及[逐字段原协议审计](qa/star-simulator/protocol-20260728-audit.md)保留审查依据。

修改历史最新为 V2.01（2026-07-22），正文标题仍 V1.00；时间广播与后续遥测应答重复使用表 2-5。本实现按表名及实际字段区分，没有自行改变线格式。W10 bit3/bit2 单位宽与 `00b/11b` 枚举矛盾，采用无歧义的默认 0；W101–104 bits25–23 划为 3 位而枚举为 2 位，模拟载噪比枚举占低两位，最高位保持 0。这些解释均已记录。

开始前 `git status --short` 与 `git diff --stat` 均为空。用户随后替换了旧协议文件；旧 `.doc` 删除和新原件新增属于用户提供输入变化，未恢复或改写原件。没有 Git 提交。

## 2. 三类广播的逐字节格式

所有外层均为 `EB 90 F0 55 | LEN:BE-u16 | FF | DATA | SUM8`；LEN 包含 TYPE、DATA、SUM，SUM 仅累加 TYPE 与 DATA。

| 广播 | LEN | 整帧长度 | DATA（零基整帧偏移） | SUM 偏移 |
|---|---|---:|---|---:|
| 整星时间 | 0008 | 14B | 7–12，W1–W6 | 13 |
| 姿态轨道 | 003D | 67B | 7–65，W0–W58 | 66 |
| GNSS | 0074 | 122B | 7–120，W1–W114 | 121 |

时间 W1–W4 为 BE-u32 整星秒；W5–W6 为 BE-u16、每单位25μs的非整秒计数。采用向下量化 `microseconds / 25`，范围0–39999，不把999999μs舍入成未归一化的40000。纳秒先向下取微秒，跨整秒自然增加秒数、非整秒归零。

姿态 W0–W3 整秒u32；W4–W6 为3B微秒；W7–15为3个i24角度，0.00005°；W16–21为3个i16角速度，0.00010°/s；W22为`转序<<5`且备用位0；W23–46依次为半长轴a[km]、e、i/Ω/ω/M[度]的BE IEEE754 f32；W47–58依次为两套磁强计XYZ的i16补码、6nT。各字段独立编码，不复制结构体内存。

GNSS 主导头为 `09 F5 | C000 OR sequence | 00 6B | 03 FF`。版本000、type0、副导头1、APID1F5；序列高两位11，低14位0–16383后回绕。编码器拒绝越界序号，实际排队接受时递增，取消可留下序号空洞；预览不消耗序号。

**GNSS W1–W114 的全部字段映射、状态位、单位、无效填充值和两组解的有效条件见本报告附录 A，直接摘自本轮原协议审计。**

## 3. Rust 实现与时间策略

| 位置 | 职责 |
|---|---|
| `crates/langmuir-core/src/star_broadcast.rs` | StarTime、三类字段编码器、WGS-84 ECEF、合成预设、14bit序号 |
| `crates/langmuir-core/tests/star_broadcast_vectors.rs` | 独立黄金帧及边界测试 |
| `crates/langmuir-transport/src/worker.rs` / `worker_broadcast_tests.rs` | 原唯一 Writer、低优先有界队列、取消门、发送时构帧和回执 |
| `apps/desktop/src/backend_star_simulator.rs` / `backend_star_simulator_tests.rs` | Controller统一调度、5s轮询保持、状态和受控链路 |
| `apps/desktop/src/star_simulator.rs` / `bridge.rs` | 配置校验、统计、可靠状态快照与请求 |
| `apps/desktop/src/app_star_simulator.rs` / `app_star_simulator_tests.rs` | 原控制区页签、点位、发送操作、HEX和布局 |
| `tools/qa/rs422_star_reference.py` / `rs422_star_validate_capture.py` | 独立Python黄金参考及实际受控写入复核 |

系统UTC/POSIX先减去 `1514764800`（2018-01-01 UTC的Unix秒），再加入2018原点后闰秒。内置策略在2026-11-16审计日为新增闰秒0；GPS−UTC总偏移18仅写入W97，不再加到累计秒。提供显式新增闰秒偏移的转换API，未来闰秒须更新内置策略；POSIX系统时钟不能独立表示插入秒，继承OS校时/平滑行为。

Writer在开始发送前调用构帧回调取系统时钟，不将队列入队时间当成发送时间。回执记录OS write/flush边界及实际完整字节；这是应用层时间，不是UART引脚测量。固定UTC、纪元前、u32溢出、24/25μs、999974/999975/999999μs、整秒进位均有测试。

姿态默认是SYNTHETIC：角度[1,−2,3]°，角速度[0.1,−0.2,0.3]°/s，转序2（2-1-3），轨道根数[7000,0.001,51.6,20,30,40]，磁场[6,−12,18,−24,30,−36]nT。它们是确定性字段测试值，不是真实姿态/磁场测量或轨道动力学输出。

GNSS ancillary模拟选择：最高信噪比45、各8颗GPS/BDS、GDOP2、版本/指令/复位/上注/错误计数0、钟漂与外推时长0、W20运行时间0。W91与W24采用同一合成时间；原文未定义W91单位/纪元，这一选择不是厂家规定。W98的+23℃是原文明确默认。Position预设禁止定轨、置定轨无效健康位；两组定位解相同。Orbit/Extrapolated时第二组全0；Invalid时位置、速度及经纬高全0。

## 4. 三个合成点位及坐标结果

三个点均为静止独立位置编码场景，ECEF速度[0,0,0]m/s，定位有效、GPS+BDS、GPS时间、PPS有效、主机；W9=`62`。无自动轮转轨迹；手动切换是离散测试，不能解释为物理合理的连续卫星轨道。

| 点位 | 纬度/经度（°） | 椭球高m | XYZ编码（0.01m） | 纬/经编码（1e-7rad） | 高度编码（0.01m） |
|---|---|---:|---|---|---:|
| SYNTHETIC A | 0.0 / 0.0 | 400000 | 677813700 / 0 / 0 | 0 / 0 | 40000000 |
| SYNTHETIC B | 35.0 / 116.0 | 450000 | -245445989 / 503238854 / 389597631 | 6108652 / 20245819 | 45000000 |
| SYNTHETIC C | -35.0 / -70.0 | 500000 | 192899459 / -529986907 / -392465513 | -6108652 / -12217305 | 50000000 |

使用WGS-84 a=6378137m、f=1/298.257223563，由同一大地坐标计算ECEF。Python参考采用半短轴形式独立计算，不调用Rust转换；预设逐分量厘米量化完全匹配。坐标量化每轴误差≤0.005m，角度量化误差≤5e-8rad，二者共同影响从经纬高反算的米级误差界。无效纬经、非有限值、定点溢出、负坐标和负速度均有测试；配置启动前也完整校验GNSS可编码性。

## 5. 完整黄金字节与两层校验

固定输入UTC `2026-11-16T00:00:00.999999+00:00`，整星秒 `280022400`，微秒999999，时间计数39999（9C3F），GNSS序号16383。所有黄金帧均由独立Python标准库参考生成，再与Rust输出逐字节比较。清单：[完整 golden-vectors.json](qa/star-simulator/golden-vectors.json)。

时间（14B，SUM=E7）：

```text
EB 90 F0 55 00 08 FF 10 B0 CD 80 9C 3F E7
```

姿态轨道（67B，SUM=73）：

```text
EB 90 F0 55 00 3D FF 10 B0 CD 80 0F 42 3F 00 4E
20 FF 63 C0 00 EA 60 03 E8 F8 30 0B B8 40 45 DA
C0 00 3A 83 12 6F 42 4E 66 66 41 A0 00 00 41 F0
00 00 42 20 00 00 00 01 FF FE 00 03 FF FC 00 05
FF FA 73
```

GNSS SYNTHETIC A（122B，W114=EE，外层SUM=44）：

```text
EB 90 F0 55 00 74 FF 09 F5 FF FF 00 6B 03 FF 62
C1 2D 2D 88 88 00 00 00 00 00 00 00 00 00 10 B0
CD 80 FF 00 00 28 66 9D C4 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 28 66 9D
C4 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 02 62 5A
00 10 B0 CD 80 00 00 12 17 00 00 01 00 00 80 00
00 00 00 00 00 00 00 02 EE 44
```

GNSS SYNTHETIC B（122B，W114=3A，外层SUM=DC）：

```text
EB 90 F0 55 00 74 FF 09 F5 FF FF 00 6B 03 FF 62
C1 2D 2D 88 88 00 00 00 00 00 00 00 00 00 10 B0
CD 80 FF 00 00 F1 5E CA 9B 1D FE D0 C6 17 38 C9
BF 00 00 00 00 00 00 00 00 00 00 00 00 F1 5E CA
9B 1D FE D0 C6 17 38 C9 BF 00 00 00 00 00 00 00
00 00 00 00 00 00 5D 35 EC 01 34 ED 3B 02 AE A5
40 10 B0 CD 80 00 00 12 17 00 00 01 00 00 80 00
00 00 00 00 00 00 00 02 3A DC
```

GNSS SYNTHETIC C（122B，W114=32，外层SUM=CC）：

```text
EB 90 F0 55 00 74 FF 09 F5 FF FF 00 6B 03 FF 62
C1 2D 2D 88 88 00 00 00 00 00 00 00 00 00 10 B0
CD 80 FF 00 00 0B 7F 69 83 E0 69 0A A5 E8 9B 73
97 00 00 00 00 00 00 00 00 00 00 00 00 0B 7F 69
83 E0 69 0A A5 E8 9B 73 97 00 00 00 00 00 00 00
00 00 00 00 00 FF A2 CA 14 FF 45 94 27 02 FA F0
80 10 B0 CD 80 00 00 12 17 00 00 01 00 00 80 00
00 00 00 00 00 00 00 02 32 CC
```

内部校验为 `SUM8(W9…W113)`，位于整帧偏移120；外层为`SUM8(TYPE + W1…W114)`，位于121。参考与测试分别重算两层，不能用外层正确替代内层检查。独立验证器也检查了损坏校验、截断帧会被拒绝。

## 6. 调度、仲裁及停止

原串口Worker仍是唯一Writer。原控制/遥测队列容量32；新增低优先广播队列容量3，不占用控制容量，每类最多一个待发任务。控制队列优先，每轮最多处理8个控制任务，最多1个广播后服务接收；每帧write_all+flush，完成后显式保留至少1μs应用层帧间隔。受控适配器每次最多写7B，仍验证所有帧完整无交错；另有2B部分写与发送中取消测试。

Controller统一tick调度，三周期独立100–60000ms，默认1000ms；错过时刻计为跳过，不补发积压。不无限重试。取消门与Writer同步：已经开始的整帧完成，停止返回后未开始的周期帧不能继续发送。数量只在成功写入回执后增加，HEX来自实际回执；队列满、取消和失败分别统计。

自动广播启动时确保遥测轮询已开启且<20s；否则启用现有5s轮询。广播期间拒绝停止轮询或配置≥20s，轮询入队失败即停止周期广播。停止广播保留遥测轮询，若要完全停止所有发送，再停止轮询或断开串口。断线立即停调度；真正销毁并重建受控Worker后也不自动恢复。广播/控制不等待设备应答，不因没有广播应答误判离线。日志标明“用户主动开启星务模拟”“SYNTHETIC”和实际串口来源。

原文100秒/断电10秒/重启≥3次的恢复条款已审计；本软件没有探针电源控制能力，未声称已实施硬件断电恢复。

## 7. 完整受控字节流与周期误差

输入与结果明确标为`CONTROLLED_TRANSPORT_SYNTHETIC`、`real_com_port=false`。本轮采用用户允许的受控串口Transport适配层，没有使用真实探针或虚拟COM驱动。

6.25s场景：同时开启三类1s广播、现有5s遥测，并发送DC1000控制；捕获24帧1457B，广播各7帧、遥测2帧、控制1帧。[完整 controlled-transport.json](qa/star-simulator/controlled-transport.json)含首次write/完成时刻、每帧HEX及完整原始wire_hex。

| 类别 | 帧数 | 配置周期ms | 平均周期ms | 实测区间ms | 最大绝对周期误差ms |
|---|---:|---:|---:|---|---:|
| 时间 | 7 | 1000 | 1000.812833 | 1000.163–1001.864 | 1.864 |
| 姿态轨道 | 7 | 1000 | 1000.809000 | 1000.035–1001.805 | 1.805 |
| GNSS | 7 | 1000 | 1000.803500 | 999.769–1001.765 | 1.765 |
| 遥测轮询 | 2 | 5000 | 5001.206000 | 5001.206–5001.206 | 1.206 |

最小完整帧间隔39μs；三类因仲裁产生的最大调度迟延分别2.439/4.382/6.440ms。时间广播相对首次受控write的时间码年龄15–28μs，姿态6–8μs；GNSS只传整秒，年龄0.640540–0.645361s。测试pump约1ms、受控read约1ms；生产Controller tick5ms、真实串口默认read超时25ms，**这些误差不能外推为真实COM保证**。

生命周期场景捕获4帧378B：遥测1帧、GNSS A/C/C，序号0/1/2。已排队A帧保持原快照，之后改为C；停止/断开/真实重建受控Worker后，观察期内无自动发送。[完整 controlled-lifecycle.json](qa/star-simulator/controlled-lifecycle.json)包含切点、断线、重连及观察结束标记。另有拥塞停轮询、全关闭、停止中在途整帧完成、重启会话计时归零的回归测试。

[完整 independent-capture-validation.json](qa/star-simulator/independent-capture-validation.json)由独立Python解析原始wire，重新按EB90+LEN切帧，核对完整流与逐帧记录、两级SUM、GNSS完整字段、ECEF、时基、序号、点位及停止/重连标记后的静默。无交错、无半帧、无尾部多余字节。

复现受控捕获与独立核验：

```powershell
$env:STAR_SIMULATOR_CAPTURE_PATH = 'docs/qa/star-simulator/controlled-transport.json'
cargo test -p langmuir-desktop --locked backend::star_simulator_tests -- --nocapture
Remove-Item Env:STAR_SIMULATOR_CAPTURE_PATH
python tools/qa/rs422_star_validate_capture.py docs/qa/star-simulator/controlled-transport.json docs/qa/star-simulator/controlled-lifecycle.json --report docs/qa/star-simulator/independent-capture-validation.json
```

## 8. 实际 Windows GUI

下列截图来自本轮最终源码重新构建的MSVC x64静态CRT非Mock程序；串口未连接，未向COM1或其他真实端口发命令。

| 原生截图 | 核验 |
|---|---|
| [默认启动](qa/star-simulator/gui-default-windows.png) | 未连接、轮询停止、默认5000ms，启动不发指令 |
| [星务页签](qa/star-simulator/gui-star-disabled-windows.png) | 三开关默认关闭、独立1000ms、未连接时发送按钮禁用 |
| [南半球合成点](qa/star-simulator/gui-synthetic-c-windows.png) | 明确SYNTHETIC、经纬高/ECEF/速度/状态/卫星/UTC预览 |
| [实际状态与HEX](qa/star-simulator/gui-hex-windows.png) | 已发送均0、无实际写入、预览标明未发送且为完整122B |
| [1280×720 / 125%](qa/star-simulator/gui-compact-125-windows.png) | 原侧栏滚动、控件无重叠、中央图表保留原区域 |

左侧页签替换原控制卡片内容，没有增加栏宽或重构中央图表。六项新增GUI测试覆盖显式点击、连接、可靠快照、编辑保持、全部关闭、长错误/大计数/244–392px栏宽；MOCK既不能点击发送，也不能通过桥接请求伪造实际写入。原生检查发现原遥测滑块会按最小值100和步长1000把5000自动量化成5100，已用无输入渲染回归测试定位并改为整数1ms步长；保留原DragValue拖动速度和预设精确值。

## 9. 验收命令与既有功能回归

| 命令 | 本轮最终结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | **391 passed、0 failed、7 ignored** |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| Windows x64 静态CRT、`--no-default-features --locked` Release | PASS |

日志与执行摘要位于`target/qa/star-simulator/`的fmt/check/tests/clippy日志、checks.json、windows-static-release.log、release-build.json、release-source-manifest.json。构建前后核对源码SHA-256，发布时再次确认一致，没有复用旧EXE。

既有RS422遥测、控制、串口错误/背压、LVDS/UDP解包、去重/重组、双通道绘图、历史、科学CSV和LPD/原始录制测试均在workspace回归中通过。LVDS科学算法、55/AA帧、CSV/LPD/LVDSRAW格式和CRC不校验策略没有修改。

7项忽略项为原有1个CPU性能基准及6个需明确外部抓包/原始TXT/GUI产物的硬件来源专项；本轮没有将其标成通过，也未用合成输入冒充原硬件采集结果。完整测试名称与原因见tests.log。

## 10. 新发布包

构建命令（环境`RUSTFLAGS=-C target-feature=+crt-static`）：

```powershell
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

| 产物 | 大小B | SHA-256 |
|---|---:|---|
| `target/delivery/windows-x64/LangmuierProbeGroundControl.exe` | 10287104 | `636f55cc6cf14413dabf577488a7cac535561e5792e475c33d8d64b924b69536` |
| `target/delivery/LangmuierProbeGroundControl-windows-x64.zip` | 4244639 | `03b82762a5957117618d61fcc960f377f055fa39325fc0bbb11f9318244a7fd0` |

[完整 windows-package-verification.json](qa/star-simulator/windows-package-verification.json)确认PE32+、Machine8664、没有vcruntime/msvcp/ucrtbase/api-ms-win-crt动态导入；ZIP只含EXE及README、CRC正确、EXE与本轮构建逐字节一致、README与当前说明一致。`--mock`在该包中被拒绝，未启用Mock或HDF5。

## 11. Git 修改清单与未完成硬件项

本轮源码变更集中在core新广播模块、transport唯一Writer扩展、desktop桥接/调度/页签/测试；另更新Windows README并新增独立参考、QA、黄金帧、受控捕获、原协议转写和截图。下面是最终工作区清单（不包括忽略的target构建产物）：

```text
M apps/desktop/src/app.rs
 M apps/desktop/src/app_parameter_tests.rs
 M apps/desktop/src/backend.rs
 M apps/desktop/src/bridge.rs
 M apps/desktop/src/main.rs
 M apps/desktop/src/mock.rs
 M crates/langmuir-core/src/lib.rs
 M crates/langmuir-transport/src/error.rs
 M crates/langmuir-transport/src/lib.rs
 M crates/langmuir-transport/src/worker.rs
 M docs/WINDOWS_README.md
 D 星务与等离子体密度探针单机RS422串行通信协议-20260724（公开）.doc
?? apps/desktop/src/app_star_simulator.rs
?? apps/desktop/src/app_star_simulator_tests.rs
?? apps/desktop/src/backend_star_simulator.rs
?? apps/desktop/src/backend_star_simulator_tests.rs
?? apps/desktop/src/star_simulator.rs
?? crates/langmuir-core/src/star_broadcast.rs
?? crates/langmuir-core/tests/
?? crates/langmuir-transport/src/worker_broadcast_tests.rs
?? docs/RS422_STAR_COMPUTER_SIMULATOR_QA.md
?? docs/qa/star-simulator/
?? docs/superpowers/plans/2026-11-16-rs422-star-computer-simulator.md
?? tools/qa/rs422_star_reference.py
?? tools/qa/rs422_star_validate_capture.py
?? 星务与等离子体密度探针单机RS422串行通信协议-513反馈-20260728（公开）.doc
```

真实探针广播接收、真实RS422电平/8O1/字节连续性/≥1μs线间隔、≤2ms遥测应答、COM拔插驱动行为、长时间周期误差及供电恢复：**NOT_RUN**。受控测试和本地OS写入成功不能证明探针已接收或执行。本阶段没有自动连接真实硬件、没有自动提交Git，完成后停止。

## 附录 A：GNSS 全字段原协议映射

## GNSS W1–W114 完整映射

GNSS W 从 1 开始：零基数据域索引 = W−1；整帧零基偏移 = W+6。

| 通道 | 参数 | 编码/单位/含义 |
| --- | --- | --- |
| W1–W2 | 包识别 | 版本 000、类型 0、副导头标识 1、APID 00111110101b=1F5；合成大端值 09F5 |
| W3–W4 | 包序列控制 | 高 2 位 11，低 14 位包计数 0–16383，值为 C000 OR count |
| W5–W6 | 包长 | 包总长度−7，固定 006B（114−7=107） |
| W7 | 服务类型 | 固定 03 |
| W8 | 服务子类型 | 固定 FF |
| W9 | 定位/模式/时间/PPS/主备 | 见下方位字段 |
| W10 | 基带/参数/定轨状态 | 见下方位字段及原文矛盾 |
| W11 | GPS 最高信噪比 | 单字节；原文未给量化单位 |
| W12 | BDS 最高信噪比 | 单字节；原文未给量化单位 |
| W13 | 捕获星数 | 高半字节 GPS，低半字节 BDS |
| W14 | 参与解算星数 | 高半字节 GPS，低半字节 BDS |
| W15 | 正确指令计数 | u8，255→0 回绕 |
| W16 | 错误指令计数 | u8，255→0 回绕 |
| W17 | 上次正确指令编号 | u8 |
| W18–W19 | 解算钟漂 | i16 补码，1m/s，超出取极值 |
| W20–W23 | 系统连续工作时间 | 4 字节；原文未给单位、符号或原点 |
| W24–W27 | UTC 累计秒 | 自 UTC 2018-01-01 00:00:00 起，包含跳秒 |
| W28 | 单点/滤波使用星数 | 高/低半字节，各 0–15，溢出饱和为 15 |
| W29 | 主核复位计数/原因 | 高半字节计数 0–15 回绕，低半字节原因 |
| W30 | 从核复位计数/原因 | 高半字节计数 0–15 回绕，低半字节原因 |
| W31–W34 | WGS-84 X | i32 补码，0.01m；定位/定轨/外推有效 |
| W35–W38 | WGS-84 Y | i32 补码，0.01m；定位/定轨/外推有效 |
| W39–W42 | WGS-84 Z | i32 补码，0.01m；定位/定轨/外推有效 |
| W43–W46 | WGS-84 Vx | i32 补码，0.01m/s；定位/定轨/外推有效 |
| W47–W50 | WGS-84 Vy | i32 补码，0.01m/s；定位/定轨/外推有效 |
| W51–W54 | WGS-84 Vz | i32 补码，0.01m/s；定位/定轨/外推有效 |
| W55–W58 | WGS-84 X1 | i32 补码，0.01m；仅定位有效 |
| W59–W62 | WGS-84 Y1 | i32 补码，0.01m；仅定位有效 |
| W63–W66 | WGS-84 Z1 | i32 补码，0.01m；仅定位有效 |
| W67–W70 | WGS-84 Vx1 | i32 补码，0.01m/s；仅定位有效 |
| W71–W74 | WGS-84 Vy1 | i32 补码，0.01m/s；仅定位有效 |
| W75–W78 | WGS-84 Vz1 | i32 补码，0.01m/s；仅定位有效 |
| W79–W82 | WGS-84 椭球纬度 | i32 补码，1e-7 rad；范围 ±90°，北正南负 |
| W83–W86 | WGS-84 椭球经度 | i32 补码，1e-7 rad；范围 ±180°，东正西负 |
| W87–W90 | WGS-84 椭球高 | i32 补码，0.01m |
| W91–W94 | 本地时间 | 接收机自我维持时间；原文未给单位、符号或原点 |
| W95–W96 | 定轨外推持续时间 | 最大 6000s |
| W97 | 当前跳秒 | 系统当前 GPS 跳秒值；注意与原点后新增跳秒数量区别 |
| W98 | 板卡温度 | i8 补码，1℃，−128～127；原文默认 +23℃ |
| W99 | boot 软件版本 | 导航定位软件，u8 原始版本值 |
| W100 | app 软件版本 | 定轨接口软件，u8 原始版本值 |
| W101–W104 | 通信/非定位原因/载噪比/健康 | 大端 32 位，完整位定义见下 |
| W105 | CAN 复位计数 | 高半字节 CAN-B，低半字节 CAN-A，0–15 回绕 |
| W106–W109 | 程序上注状态 | 大端 32 位，完整位定义见下 |
| W110 | SRAM 监控错误计数 | 高半字节从核，低半字节主核，0–15 回绕 |
| W111 | RF/位置门限/串口/PPS 状态 | 各位 0 正常、1 错误/超差，见下 |
| W112 | 定轨收敛值 | i8 补码，0.1，超出取极值 |
| W113 | PPS输出/北斗参数/CAN/GDOP | 高三位标志，低五位无符号 GDOP，超出饱和 |
| W114 | 内部校验 | SUM8(W9…W113)，不包含 W1…W8 |

所有无效位置/速度/纬经高字段按表要求全 0。W31–W54 在定位有效、定轨有效、定轨外推有效三种状态都可以报告；W55–W78 只在定位有效状态报告，不能把定轨外推结果无条件复制到第二组。

### W9

- bit7–6：00 数据无效；01 定位有效；10 定轨有效；11 定轨外推有效。原文强调这是星务判断 GNSS 定位数据是否可用的唯一标记。
- bit5–3：1 单 GPS；2 单 BDS；4 GPS+BDS。
- bit2：0 GPS 时间；1 BD 时间。
- bit1：0 PPS 无效；1 PPS 有效（定位有效、定轨有效）。
- bit0：0 主机（原文默认）；1 备机。

例如双模、GPS 时间、定位有效、PPS 有效、主机得到 `0x62`；该值只是一组可用的合成测试配置，不是协议唯一固定值。

### W10

- bit7：基带1，1 定位、0 非定位。
- bit6：基带2，1 定位、0 非定位；原文默认跟随基带1。
- bit5：0 当前基带1（默认）；1 基带2。
- bit4：0 自主选择（默认）；1 强制。
- bit3：定轨算法参数来源；原文把该单 bit 的取值写成 `00b` 默认、`11b` 上注，存在位宽矛盾。0 表示默认参数无歧义；本阶段合成默认保留 0，不扩展位宽、不猜测非零组合。
- bit2：EOP 参数来源；同样把单 bit 写成 `00b`/`11b`，默认 0。
- bit1：0 星时跳变监控关闭（默认）；1 开启。
- bit0：0 定轨开启；1 定轨禁止。

### W29/W30 复位原因

W29 低四位：0 初始、1 主从核通信错、2 指令复位、3 定轨异常、4 一秒任务超时、5 从核连续加载异常。

W30 低四位：0 初始、1 指令复位、3 TMARK 超时、4 定轨监控复位、5 非定位自主复位、6 连续10s解算异常。原文没有定义值 2，不填充为自创含义。

### W101–W104

- bits31–30：00 主从核通信正常、01 帧头/校验错、10 超时。
- bit29：Tmark，0 正常、1 超时。
- bits28–26：非定位原因 0 初始、1 未解算、2 GDOP 超差、3 未调整秒、4 地心距超差、5 位速检测超差、6 矩阵初始化错。
- bits25–23：最高载噪比标志。原文划为 3 位，但枚举使用两位 00 <30dB、01 30–40、10 40–50、11 >50。保留原位域宽度，将枚举值 0–3 放入低两位，最高位 0；记录此解释，不改成另一组字段边界。
- D22：历元观测时间小于插值前端点；D21：输入姿态类型错；D20：姿态转序错；D19：矩阵求逆失败；D18：偏心率超差；D17：半长轴超差；D16：地球/月亮/接收机位置计算错误；D15：地球引力计算值>200；D14：负数开方/除零。以上 1 错误、0 正常。
- D13–D12：滤波收敛，11 发散、01 已收敛、00 未收敛。
- D11：1 未使用观测数据；D10：1 单点测速失败；D9：1 单点定位失败；D8：1 滤波未初始化；D7：1 定轨无效；其余 0 正常。
- D6：速度小于设定值；D5：历元时间小于上一历元；D4：历元时间大于插值后端点。1 错误、0 正常。
- D3–D0：定轨运行步骤（原文没有枚举具体步骤值）。

### W106–W109

- bits31–17：程序上注增量数据长度，主/从核交替显示。
- bits16–9：程序版本号/校验字；结束上注后显示校验和，启动上注程序后显示新版本。
- bits8–7：00 初始、01 允许注入、10 注入结束、11 启动注入程序。
- bits6–4：000 初始、001 全正确、010 帧头错、011 长度零、100 校验错。
- bits3–1：001 导航定位软件、010 定轨接口软件；原文没有定义无上注时的独立枚举，模拟填充值应在 QA 中明确。
- bit0：0 boot1、1 boot2。

### W111 与 W113

W111 从 bit7 到 bit0 分别是 RF 锁定、RF 寄存器回读、定位位置门限、定轨位置门限、Soc-B 串口、Soc-A 串口、PPS-B 中断、PPS-A 中断。每位 0 正常，1 错误/超差，原文明确 bits5–0 默认正常。

W113：bit7 为 1PPS 输出禁止（0 允许）；bit6 为北斗上注参数（0 默认）；bit5 为 CAN-B（0 CAN-A）；bits4–0 是 GDOP 无符号整数，1 当量，超范围取极值 31。

### 两层校验与计数

内部 W114 = `sum(data[8..113]) & 0xFF`。外层 SUM = `(0xFF + sum(data[0..114])) & 0xFF`。按整帧偏移看，内部输入 `[15..120)`，内部结果在 120，外部输入 `[6..121)`，外部结果在 121。两层独立计算与验证。

序号为 14 位，16383 之后回到 0，序列标记仍为 11。原文没有规定入队、开始或完成哪个阶段递增。本实现仅在 GNSS 任务被发送队列接受时递增；取消可留下序号空洞；预览及入队失败不递增。

