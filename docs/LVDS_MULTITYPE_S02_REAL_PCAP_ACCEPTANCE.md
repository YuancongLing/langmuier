# LVDS-MULTITYPE-S02-REAL：真实 PCAP 软件验收报告

日期：2026-11-21。项目：`D:\RUST\langmuier`。本报告使用用户真实 FPGA 抓包，不把离线回放称作真实网卡连续采集，也不把 egui 自动化称作 Windows 原生操作。

真实数据的解码、绘图数据转换、保存与历史读取软件验证通过，已提供单独的新 EXE。**原生 Windows GUI 尚未验收，因此完整交付验收仍为 BLOCKED，不能宣称全部阶段验收完成。**

| 验收项 | 结论 |
|---|---|
| AA 科学数据与软件图表数据 | **AA_SOFTWARE_PASS** |
| AB 激励数据与软件图表数据 | **AB_SOFTWARE_PASS** |
| AC 全零、无效状态及公共接收 | **AC_INVALID_STATUS_PASS** |
| 全 UDP 无损归档及独立读回 | **RAW_ARCHIVE_PASS** |
| AA CSV / LPD v3 完整保存 | **SCIENCE_STORAGE_PASS** |
| AB 22 列 CSV 完整保存 | **EXCITATION_STORAGE_PASS** |
| Windows 原生 GUI 操作与截图 | **WINDOWS_GUI_NOT_RUN** |
| 完整交付验收 | **DELIVERY_BLOCKED**（缺原生 GUI 验收；EXE 构建及身份核验通过） |
| 真实网卡、硬件连续采集 | **REAL_HARDWARE_LIVE_NOT_RUN** |

机器可读汇总：[verification-summary.json](qa/lvds-multitype-s02-real/verification-summary.json)。当前 EXE：[LangmuierProbeGroundControl.exe](../target/delivery/lvds-multitype-s02-real-20261121/LangmuierProbeGroundControl.exe)。仅交付 EXE，未制作 ZIP。

## 1. 原始输入、工作区与进程保护

- 原始文件：`C:\Users\Administrator\Downloads\2026112101.pcapng`。
- 长度：461444 字节。
- 验收前 SHA-256：`22ec9d91789eb674f6b3cddf9bf5935bd81ea7d92030c0a1941e8f2becec4b77`，与用户提供基线相同。
- 原始修改时间 UTC：`2026-11-21T13:11:45.6086013Z`。验收工具只读原件，输出采用独立新路径；结束时重新核对哈希和修改时间。
- 工作区开始时已有大量 S01、S02、星务模拟器未提交改动，以及旧协议 DOC 删除和替换 DOC 新增；全部保留，不重置、不提交。
- 用户已有进程 PID 43960：`target\delivery\lvds-multitype-s02-20261118\LangmuierProbeGroundControl.exe`。
- 用户已有进程 PID 44796：`target\delivery\lvds-multitype-s01-20261118\LangmuierProbeGroundControl.exe`。
- 本轮不关闭以上进程，不自动连接硬件，不发送 RS422 控制或星务广播。

基线证据：[baseline.json](qa/lvds-multitype-s02-real/baseline.json)、[Git 初始状态](qa/lvds-multitype-s02-real/git-status-before.txt)。Git HEAD 为 `9155ea0ea8d4ea903959eadd967ca0d706f25a2e`；交付源码包含未提交改动，不能仅凭该 HEAD 复现。

## 2. 验证方法与证据边界

独立 Python 工具从 PCAPNG 容器及原始字节计算期望，不调用 Rust 被测解码器。Rust 专项走现有生产公共分流、AA/AB decoder、Controller、BatchWriter 和 RawRecording；原节奏回放前建立三个 writer，之后独立逐字段读回所有输出。PCAP、LVDSRAW 和 LPD 历史也通过生产读取路径核对。

`CRC_NOT_CHECKED` 是现有生产策略：保留原始 CRC，不把 CRC 校验作为接纳门禁。所有逐字节相等结论均不代表厂家 CRC 已验证。

## 3. Windows 原生 GUI 状态

`WINDOWS_GUI_NOT_RUN`。本会话工具目录没有 `node_repl` 或可调用的 Windows Computer Use 接口，因此无法按规范初始化 `@oai/sky`，无法执行原生点击、拖动、滚轮和窗口截图。没有生成或引用伪造截图，也未把命令行或 egui 输入测试当作原生截图证据。

依据：[computer-use/SKILL.md](C:/Users/Administrator/.codex/plugins/cache/openai-bundled/computer-use/26.928.31416/skills/computer-use/SKILL.md) 要求： “Import it directly by package name from the JavaScript session. Do not spawn `codex-computer-use.exe`, search for the helper executable, or build a custom helper protocol client.” 工具缺失记录见 baseline.json。

| 原生操作或截图 | 状态 |
|---|---|
| EXE 启动窗口、可见版本、三类型最终统计截图 | NOT_RUN |
| AA 双通道叠加/分屏、真实电压与时间轴 | NOT_RUN |
| AB 三角波、0.931 Hz、模式与状态详情 | NOT_RUN |
| 左拖、滚轮、冻结/恢复、跟随/重置 | NOT_RUN |
| AB 上一/下一历史帧及详情 | NOT_RUN |
| GUI 发起 AA/AB/RAW 保存及完成状态截图 | NOT_RUN |
| GUI 重开 LPD/LVDSRAW、无效 AC 说明 | NOT_RUN |

CSV 是保存和导出格式，当前应用历史打开器支持 PCAPNG、LVDSRAW、LPD v3；本轮对 CSV 使用独立完整读回，不声称应用支持导入 CSV 历史。

## 4. 范围限制与待确认项

- `REAL_HARDWARE_LIVE_NOT_RUN`：未做真实网卡接收、长时 UDP 连续采集、物理丢包或实际串口联调。约 57 秒离线回放不能代替长期压力测试。
- AB `sci_frame_seq` 按原字节显示和保存；与相邻 AA 的关系由关联表记录。厂家“最新科学包序号”的准确时序语义仍待确认，不减 1、不因此拒收。
- AD9833/DAC8563 状态 0/1 仅保留原始值，不推断正常或故障。
- AC 仅公共接收、原始保存与统计；独立读取 valid/update 字段用于本次无效状态审计，不增加 S03 完整业务 decoder。
- S03 仍缺少有效 GNSS/姿态、更新计数变化、坐标/速度/姿态语义与硬件对应证据。
- AB 历史按需从头重扫至目标，最坏 O(N)；当前抓包通过不代表超长文件为常数时间随机访问。

## 5. 独立 PCAP 审计

参考工具：[lvds_s02_real_reference.py](../tools/qa/lvds_s02_real_reference.py)。容器读取复用已验证的 Python 标准库 PCAPNG/LVDSRAW 读取器；AA 以每 5 字节拆出 4 个 10bit code，AB 按大端字段及 100 个 u16 mV 独立解码。没有使用 Rust 解码输出作为期望。

协议工作簿 SHA-256 为 `916a0f154ad71e2325a5484cb1345de0fbe18319133282c83c7b73ea3368c11f`。依据与单元格原文见 [independent-workbook-evidence.json](qa/lvds-multitype-s02-real/independent-workbook-evidence.json)。完整期望见 [independent-manifest.json](qa/lvds-multitype-s02-real/independent-manifest.json)，SHA-256 为 `61732f287d7b32378c4f313d3641501e874bf7be2f9aa243b5fa6b0fcf770f53`。

| 项目 | 从原始字节重新计算的结果 |
|---|---:|
| PCAPNG 包 / UDP | 1262 / 1262 |
| 容器异常、截断、时间倒退 | 0 |
| FPGA 来源 UDP | 1250 |
| 其他来源 UDP | 12 |
| FPGA 来源 / 目标 | 192.168.1.128:1534 → 192.168.1.100:1534 |
| FPGA Payload | 每包 293B = 外层 24B + LVDS 269B |
| 外层片索引 / 总片数 / offset / flags | 0 / 1 / 0 / 0 |
| AA / AB / AC | 1136 / 57 / 57 |
| 外层 u32 首 / 尾 | 92402 / 93651 |
| 相邻外层步长 1 / 异常步长 | 1249 / 0 |
| 公共结构与业务字段异常 | 0 |
| 全部 UDP Payload 字节 | 366346 |
| 原捕获跨度 | 56.747293 秒 |
| AA 接收间隔中位数 | 49.997500 ms |
| AB 接收间隔中位数 | 999.935000 ms |

另外 12 个 UDP 均为 8B 广播，目的 `192.168.1.255:1534`；来源 `192.168.1.100:1534`、`:59325`、`:54740` 各 4 个。它们是其他来源报文，不是 LVDS 结构错误；主归档在来源过滤前保留它们。

完整审计：[independent-pcap-audit.json](qa/lvds-multitype-s02-real/independent-pcap-audit.json)。生产 `LvdsReceiveStats.udp_packets` 的既有定义为**来源接纳后 1250**，另有 `source_rejected_packets=12`；RAW 的 `observed=1262`。三者不可互换。

## 6. AA、AB 和 AC 真实数据结果

AA Accepted 为 1136，每通道 113600 点、共 227200 个 code10 值，采样间隔 500μs；AA seq8 连续且允许 255→0。通道 1 为 `513×113331 + 512×269`，通道 2 为 `512×113600`。转换使用 `V = code10×0.01−5.12`：通道 1 为 0–0.01V，通道 2 恒为 0V，没有修改 ADC、加入噪声或人为放大。

真实生产历史 → egui 验证逐点核对两个通道的全部电压数据。AA 自动拟合 Y 范围约为 `−0.0005～0.0105 V`，叠加/分屏均已通过自动化。AA 绘制仍按像素预算抽稀；“227200 项全部核对”指原始数据及显示转换，不表示屏幕同时绘制全部 227200 点。

57 个 AB 经公共分流器进入正式 `Excitation` decoder，逐字段、逐采样、逐时间与独立期望一致：

| 字段 | 实际结果 |
|---|---|
| work_mode | 0x55，三角波 |
| scan_freq | 1；界面设定值显示 1 Hz |
| f_act_mHz / Hz | 931 / 0.931 |
| interval_us / points | 10800 / 100 |
| scan_voltage_mV | 0 |
| AD9833 / DAC8563 原始状态 | 0 / 0 |
| pa0_seq | 23–79 连续，共 57 |
| wave_mV / V 范围 | 1655–1715mV / 1.655–1.715V |
| 36B 保留区 | 全部 0 |
| 每帧首末时间跨度 | 1069200μs = 1.0692 秒 |
| 相邻窗口重叠 | 56 对，均保留原样，不合并或伪造样本 |

首个 AB：外层 u32=92412、raw_record_id=11、PA0=23、科学序号=214。原始 wave 从 1715mV 下降至 `wave[49] / wave[50]` 的 1655mV，再升至末点 1715mV，构成真实三角波。最新设备时间为 `128.011383s`，首点为 `126.942183s`；`t_i=128011383−(99−i)×10800`，末点与原始时间戳一致，跨秒正确且无下溢。末帧 PA0=79，首末时间为 `182.942144～184.011344s`。

真实 egui 专项通过生产 `RealBackendBridge → OpenHistory` 扫描原 PCAP，逐帧选择并渲染全部 57 个 AB，核对全部 5700 点的 `mV/1000` 与设备时间；首帧 Y 自动范围约 `1.652～1.718V`。真实数据冻结/恢复、历史选择也通过。证据：[ui-real-data-evidence.json](qa/lvds-multitype-s02-real/ui-real-data-evidence.json)。这些为自动化 egui 证据，原生窗口操作状态仍见第 3 节。

全部 57 个 `sci_frame_seq` 匹配后续 AA，匹配前一 AA 为 0。未改变字段、未减 1、未拒收。完整 57 行关联表见第 12 节及 [原始关联 CSV](qa/lvds-multitype-s02-real/independent-AB-AA-association.csv)，包含双方 raw ID、外层序号、设备时间和接收时间。

57 个 AC 的全部 DATA260B 均为 0。工作簿独立核对的 DATA 偏移为：gnss_valid=0、gnss_update_cnt=1..4、att_valid=127、att_update_cnt=128..131，四项读值均为 0。生产公共接收 57，未计为结构失败。现有 UI 文案为“AC 星务：…，已接收，待解码”；没有有效定位或姿态测量结论。

## 7. 完整保存与独立读回

最终输出目录：[production-performance-final](../target/qa/lvds-multitype-s02-real/production-performance-final/)。正式生产测试首帧前创建 AA `LpdAndCsv`、AB CSV、RawRecording 三类 writer，再启动 `PcapReplay`，通过 `Controller::drain_replay` 按正常节奏运行。主 RAW 由 `drain_recorded` 在来源过滤前逐条提交。此过程由生产后端专项执行，没有冒充 GUI 按钮操作。

| 输出 | 实际全量结果 | 关闭与完整性 |
|---|---|---|
| `capture.lvdsraw` | 1262 记录；492766B；Payload366346B | observed/enqueued/written=1262；dropped=0；END 与精确 EOF 通过 |
| `science.csv` | 113600 数据行、113601 总行、11 列 | 入队/写帧=1136，正常关闭，无错误 |
| `science.lpd` | 1136 帧、464664B | 40B 头 + 1136×409B，精确 EOF |
| `excitation.csv` | 5700 数据行、5701 总行、22 列 | 入队/写帧=57；dropped=0；正常关闭，无错误 |
| `ui-entry-full.lvdsraw` | 1262 记录；492766B | 正式 `Controller::start_raw_recording` 的全 PCAP 导出入口，独立重新读回通过 |

RAW 使用正式 `RawReader` 再开，并由 Python 独立检查每条 Payload 字节、长度和顺序、源/目标、原包索引、时间纳秒、record ID、source_matches、完整 END 和精确 EOF。原始 CRC 字节也在载荷比对中原样匹配。主 RAW 动态 Header 还与预建 writer 时记录的真实元数据核对。

AA CSV 全部 113600 行的 11 列均核对：frame_index、设备秒/微秒、seq8、sample_index、sample_time_us、双通道 code10、双通道重建 ADC（`code10×64−32768`）及 `CRC_NOT_CHECKED`。LPD 独立核对完整 40B 头和每帧时间、seq8、200 个 code10，共 227200 值，未仅检查文件大小。

AB CSV 全部 5700 行、22 列逐字段比对，包含原始 record ID、完整外层 u32、PA0/科学序号、设备秒/微秒、样本索引与微秒时间、原始 mV、计算 V、mHz/Hz、模式、扫描设定、双状态、完整性、来源、会话及原接收纳秒。动态 session 的期望来自 writer 前的生产 ingress 元数据，未从待验证 CSV 自身反推。当前 PCAP replay 的 protocol session 与主 RawHeader.session_id 是不同标识，报告和校验分别保留；历史打开 RAW 时使用 Header 会话。

两份 RAW 的差别是入口和独立创建会话：`ui-entry-full` 在主回放完成后，通过已有 PCAP 全量导出路径生成，独立于当时回放位置。它不能冒充主链中的实时录制；两条生产路径分别验证。

独立最终读回：[independent-full-storage-readback-final.json](qa/lvds-multitype-s02-real/independent-full-storage-readback-final.json)，含所有输出的字节数与 SHA-256。生产最终摘要：[production-summary-final.json](qa/lvds-multitype-s02-real/production-summary-final.json)。此前正常节奏的首次成功运行及完整读回也保留，未覆盖历史输出。

## 8. 历史、显示队列与性能

生产历史分别打开原 PCAP、新 LVDSRAW、新 LPD：三者均核对 AA 1136 帧/每通道113600点；PCAP 与 RAW 同时取得 AB57，并检查 `0→1→0`、`56→55→56` 的历史选择及全部字段。LPD 不含 AB。egui 专项另遍历全部 57 AB，未将业务保存依赖于显示队列。

主回放测试故意不消费显示事件：显示队列保留 8 项、丢弃 1185 项，AA/AB/RAW 正式保存仍全量、零丢失。该数量是显示事件覆盖，不能称为 UDP 或文件数据丢失。现有回归同时覆盖左拖、滚轮、跟随、冻结及视图互不干扰；原生鼠标操作尚未执行。

最终有进程采样的复核使用同一已编译非 Mock 专项，PID40596，工作目录 `D:\RUST\langmuier`，正常回放 56.7559717 秒，捕获跨度 56.747293 秒。共采 62 个进程样本，其中正常回放阶段 56 个样本；约55.04秒采样区间 CPU 增量0.1875秒，相当于单逻辑核0.34%；工作集29,667,328～30,216,192B，Private26,451,968～26,636,288B，未观察到明显持续增长。

该性能数据来自 Debug 非 Mock 后端测试进程，并行期间有 Release 构建，排除了 EOF 后历史加载；它不代表原生 GUI 帧率、窗口稳定性或长期负载表现。完整记录：[paced-process-performance.json](qa/lvds-multitype-s02-real/paced-process-performance.json)、[performance-summary.json](qa/lvds-multitype-s02-real/performance-summary.json)。此前 PID20832 的完整回放为56.8737614秒，软件验证同样通过；复核不重复计为新增独立测试。

## 9. UI 修复与回归

用户反馈入口难找已通过 egui 测试复现：旧默认工具栏只露出 AA 下拉框，AB 和 AB CSV 无法直接看到。本轮先观察可发现性测试失败，再做最小修改：窗口及页眉标明 `S02-REAL (20261121)`；AA/AB 改为直接可见按钮；历史、AB CSV 在两类视图常驻；窄区域保留固定两行，显示选项放入“显示”菜单；扫描设定增加 Hz，AB 图例显示 PA0/TW_BACK。没有改动生产协议、解码器、存储架构或 RS422 控制。

| 最终命令 / 专项 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | exit0 |
| `cargo check --workspace --locked` | exit0 |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | **470 passed / 0 failed / 9 ignored** |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit0 |
| 旧真实 AA 科学、RAW、原始 TXT 专项 | 5 passed / 0 failed，单独运行 |
| 新真实生产回放/保存/历史专项（非 Mock） | 1 passed / 0 failed |
| 新真实生产历史 → egui 专项 | 1 passed / 0 failed |

原默认基线469/0/7，本轮增加1项默认入口回归及2项默认 ignored 的真实输入专项，因此是470/0/9。上述2项真实专项已显式执行；默认 ignored 中的原生 GUI 派生文件专项和 CPU benchmark 仍未执行。不能将专项重跑次数或 ignored 显式通过数相加为更多独立默认测试。

全部默认回归包含公共分流、269B/24B结构、AA10bit与电压、AB100点与最新点倒推、CRC_NOT_CHECKED、TTL/去重/u32/seq8、历史与保存、图表输入、RS422以及星务模拟器。日志与命令见 [regression-commands.json](qa/lvds-multitype-s02-real/regression-commands.json)、[workspace-test.log](qa/lvds-multitype-s02-real/workspace-test.log)、[workspace-clippy.log](qa/lvds-multitype-s02-real/workspace-clippy.log)。

验收工具期间的失败均保留并修正：首次后端专项错误地期望 `udp_packets=1262`，实际1250，修为独立来源计数；真实 egui 测试路径原按 Cargo 工作目录错误解析，改为工作区定位；新增测试的5项Clippy问题已清除；Python路径比较改用同一文件身份，session十六进制比较改按字节。EXE审计输出本来是文本，汇总工具修正了JSON解析假设。没有通过改变抓包、过滤异常、修改生产统计或关闭校验来凑数。

## 10. 单 EXE 构建和身份

交付：`D:\RUST\langmuier\target\delivery\lvds-multitype-s02-real-20261121\LangmuierProbeGroundControl.exe`。

- 10786304 字节。
- SHA-256：`a9f87471646dea0af2bed44d6758925652ab6ba0485bde123b963b016a31479b`。
- Windows AMD64、PE32+、MSVC、静态 CRT、Release、非 Mock。
- 与 `target/x86_64-pc-windows-msvc/release/langmuier.exe` 逐字节一致。
- PE 导入检查未发现动态 CRT；默认启动保留空接收/连接状态，不自动生成科学数据、连接设备或发送指令。

构建命令：

```powershell
$env:RUSTFLAGS='-C target-feature=+crt-static'
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

构建exit0；交付EXE实际运行 `--audit-pcap`，PID36452、工作目录 `D:\RUST\langmuier`、exit0，确认1250 FPGA、AA1136、AB57、AC57和0缺口。CLI预先筛选来源，因此其内部source_rejected=0，与全量回放的source_rejected=12不是同一个统计边界。CLI明确 `file_frames=NOT_RUN`，实际文件保存证据来自第7节，未把EXE只读审计当成GUI保存。

证据：[release-build.json](qa/lvds-multitype-s02-real/release-build.json)、[delivery-identity.json](qa/lvds-multitype-s02-real/delivery-identity.json)、[EXE 实际输出](qa/lvds-multitype-s02-real/release-real-pcap-audit.log)。97份源码/配置身份清单见 [source-identity.json](qa/lvds-multitype-s02-real/source-identity.json)，构建后重新核对均未改变。

## 11. 使用入口、保护复核和变更清单

1. 运行新目录下的 EXE，确认页眉和标题为 `S02-REAL (20261121)`。
2. 图表上方点“打开历史”，选择原始 `2026112101.pcapng` 或保存的 `.lvdsraw`。
3. 直接点击“激励波形 AB”；“AB 详情”可查看字段和历史帧，“AB CSV → 导出历史 AB CSV”导出整份历史。
4. 查看科学数据点“科学波形 AA”，“显示”菜单切换叠加/分屏。CSV 可在表格软件或文本工具打开；应用历史入口请使用 PCAPNG、LVDSRAW、LPD。
5. 若按节奏回放并同步保存，先停止旧来源、开始 AB CSV，再在“离线回放 PCAP”勾选“回放时保存科学数据”并选择 `LPD + CSV`；原始录制在 PCAP 入口会导出整份抓包。以上入口和状态机有软件自动化依据，原生逐项操作仍待验收。

末次原件大小、SHA-256和mtime均与开始完全一致；旧S02 EXE仍为原哈希 `8ab70f1d12991deedcea554e0f20f749160046ec1074a5e637fb7b79b29ec419`。用户PID43960、44796仍在运行，未被本轮结束。证据：[final-protection-check.json](qa/lvds-multitype-s02-real/final-protection-check.json)。没有新增ZIP或Git提交。

本轮源码涉及：`apps/desktop/src/main.rs`（版本），`app.rs`、`app_excitation.rs`（显式入口与频率说明），`app_history.rs`、`app_layout_tests.rs`、`app_excitation_tests.rs`（入口相关布局/测试），`backend.rs`（仅新增专项模块声明）。新增 `app_s02_real_tests.rs`、`backend_s02_real_tests.rs`；新增独立参考工具、QA证据、本报告和计划，更新 `WINDOWS_README.md`。S01/S02及星务模拟器的原有未提交改动保留。

完整工作区清单（含既有改动）见 [git-status-after.txt](qa/lvds-multitype-s02-real/git-status-after.txt)、[git-diff-stat-after.txt](qa/lvds-multitype-s02-real/git-diff-stat-after.txt)，与开始清单分别保存，不能将完整Git差异全部归为本轮新增。

## 12. 逐 AB 与相邻 AA 关联表

下表帧号为1起始显示编号，CSV里的ab_frame_index为0起始。每行均为“与后续AA匹配；与前一AA不匹配”，保持原sci_frame_seq。

| AB # | raw ID | outer u32 | PA0 | sci_frame_seq | Previous AA seq8 | Next AA seq8 |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 11 | 92412 | 23 | 214 | 213 | 214 |
| 2 | 33 | 92434 | 24 | 234 | 233 | 234 |
| 3 | 55 | 92456 | 25 | 254 | 253 | 254 |
| 4 | 77 | 92478 | 26 | 18 | 17 | 18 |
| 5 | 99 | 92500 | 27 | 38 | 37 | 38 |
| 6 | 121 | 92522 | 28 | 58 | 57 | 58 |
| 7 | 143 | 92544 | 29 | 78 | 77 | 78 |
| 8 | 166 | 92566 | 30 | 98 | 97 | 98 |
| 9 | 188 | 92588 | 31 | 118 | 117 | 118 |
| 10 | 210 | 92610 | 32 | 138 | 137 | 138 |
| 11 | 233 | 92632 | 33 | 158 | 157 | 158 |
| 12 | 256 | 92654 | 34 | 178 | 177 | 178 |
| 13 | 278 | 92676 | 35 | 198 | 197 | 198 |
| 14 | 300 | 92698 | 36 | 218 | 217 | 218 |
| 15 | 322 | 92720 | 37 | 238 | 237 | 238 |
| 16 | 344 | 92742 | 38 | 2 | 1 | 2 |
| 17 | 366 | 92764 | 39 | 22 | 21 | 22 |
| 18 | 388 | 92786 | 40 | 42 | 41 | 42 |
| 19 | 410 | 92808 | 41 | 62 | 61 | 62 |
| 20 | 432 | 92830 | 42 | 82 | 81 | 82 |
| 21 | 454 | 92852 | 43 | 102 | 101 | 102 |
| 22 | 476 | 92874 | 44 | 122 | 121 | 122 |
| 23 | 499 | 92896 | 45 | 142 | 141 | 142 |
| 24 | 521 | 92918 | 46 | 162 | 161 | 162 |
| 25 | 543 | 92940 | 47 | 182 | 181 | 182 |
| 26 | 566 | 92962 | 48 | 202 | 201 | 202 |
| 27 | 589 | 92984 | 49 | 222 | 221 | 222 |
| 28 | 611 | 93006 | 50 | 242 | 241 | 242 |
| 29 | 633 | 93028 | 51 | 6 | 5 | 6 |
| 30 | 655 | 93050 | 52 | 26 | 25 | 26 |
| 31 | 677 | 93072 | 53 | 46 | 45 | 46 |
| 32 | 699 | 93094 | 54 | 66 | 65 | 66 |
| 33 | 721 | 93116 | 55 | 86 | 85 | 86 |
| 34 | 743 | 93138 | 56 | 106 | 105 | 106 |
| 35 | 765 | 93160 | 57 | 126 | 125 | 126 |
| 36 | 787 | 93182 | 58 | 146 | 145 | 146 |
| 37 | 809 | 93204 | 59 | 166 | 165 | 166 |
| 38 | 832 | 93226 | 60 | 186 | 185 | 186 |
| 39 | 854 | 93248 | 61 | 206 | 205 | 206 |
| 40 | 876 | 93270 | 62 | 226 | 225 | 226 |
| 41 | 899 | 93292 | 63 | 246 | 245 | 246 |
| 42 | 922 | 93314 | 64 | 10 | 9 | 10 |
| 43 | 944 | 93336 | 65 | 30 | 29 | 30 |
| 44 | 966 | 93358 | 66 | 50 | 49 | 50 |
| 45 | 988 | 93380 | 67 | 70 | 69 | 70 |
| 46 | 1010 | 93402 | 68 | 90 | 89 | 90 |
| 47 | 1032 | 93424 | 69 | 110 | 109 | 110 |
| 48 | 1054 | 93446 | 70 | 130 | 129 | 130 |
| 49 | 1076 | 93468 | 71 | 150 | 149 | 150 |
| 50 | 1098 | 93490 | 72 | 170 | 169 | 170 |
| 51 | 1120 | 93512 | 73 | 190 | 189 | 190 |
| 52 | 1142 | 93534 | 74 | 210 | 209 | 210 |
| 53 | 1165 | 93556 | 75 | 230 | 229 | 230 |
| 54 | 1187 | 93578 | 76 | 250 | 249 | 250 |
| 55 | 1209 | 93600 | 77 | 14 | 13 | 14 |
| 56 | 1232 | 93622 | 78 | 34 | 33 | 34 |
| 57 | 1255 | 93644 | 79 | 54 | 53 | 54 |

