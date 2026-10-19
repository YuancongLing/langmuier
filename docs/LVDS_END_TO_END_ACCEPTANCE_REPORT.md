# LVDS 接收、CRC16 可控诊断、实时波形与科学数据保存验收报告

日期：2026-10-20。HEAD：`3c60b1d05c4e3a924953ae25528241cf59dd7603`。源码行号指本轮工作区。

**软件协议、诊断隔离、回放及 `.lpd` 文件闭环 PASS。最终含 Mock workspace 为 206 passed、0 failed、1 ignored；该 ignored 项为依赖用户抓包的 E7，已经单独显式执行 PASS。** 原 170 项保留通过，新增 36 项默认测试及 1 项外部抓包专项。严格 workspace Clippy 仍为 **FAIL**，既有问题未被放宽诊断结果覆盖。可选 HDF5 执行 **BLOCKED**，原生 GUI 完整操作验收及真实设备验收仍有未执行项。

本轮按用户最新说明暂停 P1-4 完整序号存储升级，未实施科学文件 Reader、Sidecar、新包络存储或文件版本迁移。完整 UDP u32 仅用于原有门禁、监测及新增诊断详情，不改变现有科学模型和磁盘兼容策略。

状态：`PASS` 为实际通过；`FAIL` 为实际检查失败；`BLOCKED` 为环境阻断；`NOT_RUN` 为未执行；`SPEC_ASSUMPTION` 为尚未获厂家确认的协议解释或软件策略。

## 1. 开始状态与已有能力

首先执行 `git status --short`、`git diff --stat`。已有 18 个已跟踪修改文件，累计 2522 insertions、270 deletions，以及两个未跟踪核心模块、五份历史报告和用户 PCAP。对开始时 63 个现有 Rust、Cargo、文档及抓包文件保存哈希和副本，最终核对其中仅本轮明确修改的 8 个文件改变，其余 55 个完全一致。

| 功能 | 修改前事实 | 本轮结果 |
| --- | --- | --- |
| UDP 监听、来源过滤、24B 解析、269B 重组 | 已有生产实现 | 复用，同一路径用于离线数据报 |
| CRC16、时间、ADC 严格校验 | 已有强制门禁 | 保持；新增独立诊断检查接口 |
| 2 秒 TTL、5 秒/256 FIFO 去重、P1-3 | 已有实现与回归 | 原模块未改，全部回归通过 |
| 正式实时曲线 | 已有双通道事件、缓存和图表 | 修复只有正在保存才发波形事件的限制；不保存也能实时显示 |
| 科学存储 | 已有 BatchWriter、BinarySink、可选 HDF5 | 复用；增加入队、sink 写入、关闭及错误状态展示 |
| CRC 操作界面 | 无完整诊断操作闭环 | 增加确认、详情、HEX 复制和隔离预览 |
| PCAPNG 回放 | 无生产读取/定时回放入口 | 增加有界只读读取器、后台文件线程及生产 ingress 回放 |

没有连接真实设备，没有发送 RS422 控制命令，也没有修改或移动用户抓包。

## 2. 严格校验与独立诊断

`crates/langmuir-core/src/lvds.rs:200` 的严格解码仍按“结构/固定字段 → 原 CRC16 → 时间 → 全部 ADC”执行。抽出共享的结构检查和 `decode_scientific_body():260`，严格入口没有宽松参数，也没有替代 CRC 路径。

新增 `diagnose_scientific_frame():128` 返回 `ScientificFrameDiagnosis`（`:106`）：收到 CRC、计算 CRC、原严格错误、被 CRC 错误遮挡的次级时间/ADC 错误，以及可选 `UnverifiedScientificFrame`（`:92`）。后者是独立类型，只提供借用科学字段的访问器，不直接作为正式 handler 的输入。

诊断行为：

- 完整 269B 才读取/计算两个 CRC 值；算法与输入保持 `CRC16-CCITT-FALSE`、`frame[7..267]`、260B。
- 严格合法帧继续走原 ScienceGate；诊断接口不会额外生成正式交付。
- 仅 CRC 不一致、其他结构/固定字段/时间/250 个 ADC 全部合格时，才生成未验证预览。
- 帧头、长度、固定字段、时间或 ADC 不合格时，只保留错误和原始字节，不能生成诊断曲线。
- 原始字节不改写，不尝试其他 CRC 覆盖范围，不执行 bit 位移纠正，不更新错误候选的去重或序号观察状态。

`UdpLvdsIngress::inspect_complete()` 位于 `apps/desktop/src/backend.rs:540`。每个重组完成候选均更新有界的最新详情；正式门禁仍独立调用原 `UdpLvdsScienceGate::accept_at()`。诊断只保留最新待显示事件，允许合并视觉更新，诊断候选计数不因此降低。正式交付顺序不重排。

本轮没有新增原始异常文件 writer。实时界面保留最近一帧 HEX，可复制；用户 PCAP 本身保留原始异常证据。没有声称这相当于持续保存所有实时异常报文。

## 3. 数据链路与状态边界

```text
实时 UDP socket                     PCAPNG 后台只读文件线程
  来源过滤                          保留原来源/目的端点、payload、时间
       └────────── 同一 UdpLvdsIngress ──────────┘
                    24B 解析 → 原重组 / 2 秒 TTL
                                   ↓
                      最新 CRC/错误/原始字节详情
                     ↙                         ↘
 原严格科学解码 → 原去重 → 原 P1-3     CRC 失败且其余全部合格
             ↓                                  ↓
 Controller::handle_scientific_frame       DiagnosticSamples
             ↓                                  ↓
 正式 ScientificSamples（不依赖保存状态）   独立诊断缓存/未验证波形
             ↓
 仅正在正式保存：原 BatchWriter<ScientificFrame>
             ↓
 原 BinarySink / Hdf5Sink
```

`Controller::handle_scientific_frame():1674` 先发送合法实时波形；只有正在保存才提交 writer。GUI 已显示与 `submit_timeout()` 成功均不代表磁盘持久化。新显示行为不改变正式存储字段和接受顺序；文件仍按完整帧重组完成、通过 Accepted 的顺序写入。

`receive_state():606` 区分已绑定等待、UDP 到达但尚无完整科学帧、LVDS 校验失败、有效科学帧及恰 2 秒起的流超时；Controller 另报告未监听及接收 I/O 异常。绑定成功本身不表示设备正常输出。字节流入口单列为无 UDP 外层序号来源，不伪造 UDP 0。

接收来源使用进程内接收会话后缀区分同地址重连；这只是 UI/回放隔离身份，不是设备会话号，也未持久化。UDP 与离线来源切换销毁旧 ingress、门禁和监测实例。正式保存期间拒绝启用新的 PCAP 来源及切换诊断模式；停止回放会通过原 disconnect/停采路径关闭当前保存。切换实时监听也沿用先断开旧来源、停止旧保存的边界。

## 4. CRC UI、波形及保存操作

中央控制区增加 CRC 严格/诊断状态、详情和“波形 / 回放”菜单。诊断预览须点击确认，其提示为：

> 诊断模式允许查看 CRC 未通过的数据。显示结果未经完整性验证，不得作为正式科学测量结果。

确认后才发送 `SetDiagnosticPreview`；正式保存期间禁用切换，backend 也独立校验。每次启动 backend 默认为严格模式，不写入持久配置。

CRC 详情展示算法参数、当前覆盖范围及未确认说明、模式、最新完整性结果、收到/计算 CRC、完整 u32（未知与实际 0 分开）、分类计数、错误和原始 HEX/复制。无数据时显示“尚无可校验数据”，没有伪造 CRC 值。浮动详情自身可滚动，不增加中央整体滚动条。

正式与诊断波形分别使用两个通道缓存和不同事件。菜单可选择正式或“未验证诊断波形（CRC 失败）”。恢复严格会清除诊断数据及相关冻结状态；延迟到达的诊断事件在严格模式被丢弃。来源改变清除旧来源波形，桥接层同时丢弃旧视觉队列，防止离线与实时数据残留混合。

原通道开关、叠加/分屏、冻结、清空、10 秒窗口及下采样缓存保留。横轴取设备 UTC+微秒并按 400μs 生成样本时间。既有 ADC→mV 系数未变，纵轴继续明确标为“电压估算（mV，未校准）”，不宣称厂家标定。

保存状态区分别显示入队数、sink API 写入数、关闭状态、路径与错误。`stop_acquisition_with_reason():1342` 使用真实 writer shutdown 结果更新关闭/错误状态，失败保留 `.incomplete` 提示；`sink_written` 也不是断电持久化回执。原 BinarySink 仅 BufWriter flush，无新增 fsync 保证。

UI 使用 `ui-ux-pro-max` 的稳定布局、明确状态/错误和原生控件指导，沿用现有主题。固定三栏、中央 136px 操作区、图表/日志停靠尺寸保持，窄窗口缩短标签；没有中央整体滚动条。

## 5. PCAPNG 回放实现与使用

新增 `crates/langmuir-transport/src/pcapng.rs`，只负责捕获包提取，不实现第二套 LVDS/CRC 解码。支持 PCAPNG 的节端序、接口时间分辨率/偏移、Enhanced/旧 Packet Block、Ethernet/VLAN、IPv4/IPv6 UDP；合法非 UDP 包计入总包数后按过滤规则不进入 ingress。

块最多 16MiB、接口最多 4096；一次寻找下一个包最多扫描 4096 个元数据块/64MiB。截断、长度/trailer 不符、未知链路、IP 分片及无时间戳 Simple Packet Block 明确报错，不静默跳过以制造成功。经典 `.pcap` 文件不是本读取器支持的格式，会明确拒绝；本轮实际文件为 PCAPNG。

`PcapReplay`（`backend.rs:81`）后台线程读取，64 项有界通道传递原端点、payload、捕获时间；Controller 按抓包相对间隔推进，每 tick 最多处理 64 包。首包及后续包保留原顺序；时间倒退或溢出明确停止。等待下一包期间仍维护原 2 秒残帧及序号缺口到期；不在尚未处理的包之前超前过期。正常 EOF 有显式完成消息，异常线程断开按错误处理。停止发出取消信号，不等待长时间磁盘读取阻塞 UI。

目标过滤沿用实际来源 `192.168.1.128:1534`，没有修改硬件地址或 UDP 协议。回放只读原文件、不建立 socket、不向网络发送数据。结束正常关闭当前保存，解析失败标记不完整。

桌面使用：正常启动（不要带 `--mock`）→“波形 / 回放”→“离线回放 PCAP”→输入捕获路径→开始/停止。诊断须先确认启用，再启动回放。弹窗显示进行/结束、已处理总包和目标 UDP 数及错误。Mock bridge 明确拒绝真实 PCAP 回放并提示正常启动，合成数据不被记成真实 CRC 验证结果。

另提供无需 GUI、无需 sleep 的只读审计命令，仍调用同一生产 ingress：

```bash
cargo run -p langmuir-desktop --locked -- --audit-pcap '2026101902--朗缪尔.pcapng'
cargo run -p langmuir-desktop --locked -- --audit-pcap '2026101902--朗缪尔.pcapng' --diagnostic
```

两条命令本轮均实际执行 PASS。该命令不创建科学文件，其输出 `file_frames=NOT_RUN`；文件写入结论来自下节的真实 sink 专项，二者不混淆。

## 6. 用户异常抓包实际结果

原件路径：项目根目录 `2026101902--朗缪尔.pcapng`。大小 389712B，SHA-256：

```text
5937b2da0cc833946bc082c03f9ea9c1ab1424427769c4a071ed893ead50eb2f
```

捕获包时间单调，持续约 52.7979949 秒。读取器从原件独立统计，未使用旧报告数字作固定预期。

| 实际指标 | 严格回放 | 诊断回放 |
| --- | ---: | ---: |
| PCAP 总包 | 1059 | 1059 |
| 目标来源 UDP | 1057 | 1057 |
| 合法 24B 外层 | 1057 | 1057 |
| 完整 269B 候选 | 1057 | 1057 |
| 按原范围计算 CRC 相等 | 0 | 0 |
| 按原范围计算 CRC 不等 | 1057 | 1057 |
| 帧头错误 | 3 | 3 |
| 长度 / 固定字段错误 | 0 / 0 | 0 / 0 |
| 次级时间错误 | 1 | 1 |
| 检出 ADC 错误 | 0 | 0 |
| 去重抑制 / 合法冲突 | 0 / 0 | 0 / 0 |
| 正式 Accepted / 正式波形事件 | 0 / 0 | 0 / 0 |
| 独立诊断候选/事件 | 0 | 1053 |
| 正式 writer 入队 / sink 写入 | 0 / 0 | 0 / 0 |
| 正式文件科学记录 | 0 | 0 |
| 关闭后 `.lpd` 长度 | 32B | 32B |

另外两包是 mDNS 与 DHCPv6，保留在总包数中，来源过滤后不进入科学 ingress。目标 UDP payload 均为 293B（24+269）。

分类计数有重叠，不能相加当作互斥包数：CRC 不等 1057 包包含 3 个头错误；严格解码先拒绝错误头，所以原 `checksum_failures` 为 1054、`header_failures` 为 3。其余 CRC 失败中有 1 个次级时间错误，故 1053 个可预览；结构/时间错误会阻断后续 ADC 解码，“检出 ADC 错误 0”不宣称这些被提前拒绝的候选也已通过所有 ADC 检查。

最近一包完整序号为 50579，收到 CRC `0x61E0`，当前范围计算为 `0x9898`。P1-3 observed 为 0，去重缓存为空；没有将诊断候选推进正常状态。

外部 E7 专项实际执行：同一原件分别经过严格/诊断 ingress、Controller、真实 BinarySink 和异步 BatchWriter，关闭重开后两个文件字节相等，仅含原 32B 头。原 PCAP 前后字节比较相等。结果是软件异常处理 PASS，**不是该抓包科学数据合格，也不是已定位硬件故障原因**。

## 7. 自动化测试证据

所有窗口、TTL 和 pacing 测试使用注入 `Instant`/捕获时间，无长时间真实 sleep。后台线程完成测试只使用有界通道超时。

| 场景 | 实际结果 |
| --- | --- |
| E1 标准合法帧 | PASS：固定厂家参考 CRC 黄金帧，两路各 125 点正确 |
| E2 CRC 错误 | PASS：改 DATA 不改 CRC，严格无正式/诊断事件，正式文件仅 32B |
| E3 诊断预览 | PASS：仅 CRC 失败且其他全部合格进入独立事件；两路首末 ADC 上下溢、固定字段、时间、长度错误均不能伪造曲线 |
| E4 400 帧 / 50ms | PASS：生产 ingress→Controller，不保存也产生 400 个正式事件；每路 50000 点，UTC 跨秒及每个 400μs 样本时间核对；观察 400、连续 399，无缺口误报 |
| E5 正式文件 | PASS：真实 `.lpd` 关闭重开，独立固定 v2 头、每帧 UTC/微秒/u16 和全部 250 ADC 比较一致 |
| E6 混合异常 | PASS：8 候选中合法 Accepted 2、重复 1、冲突 1、头错误 1、CRC 不等 3、时间错误 1、ADC 错误 1、诊断事件 1；文件仅 2 条科学记录 |
| E7 用户 PCAP | PASS：专项显式执行，两模式分类与真实文件结果见第 6 节；默认 workspace 中该项 ignored 以免缺少用户外部文件时伪造通过 |
| E8 UDP 可靠性 | PASS：64 槽恢复、新诊断混合回归；MAX→0、迟到补齐、重复/冲突仍按原规则；原 TTL/P1-2/P1-3 测试全部保留通过 |
| E9 RS422 | PASS：既有六条黄金指令、SUM、遥测及流式恢复；未发送真实控制命令 |
| E10 UI 状态 | PASS：确认按钮真正点击前不启用、保存期间不可切换、严格模式丢弃延迟诊断、来源变化清空、同来源刷新保留冻结；真实 themed egui 控件在 800×600、1120×720、1360×860 无控制区越界 |
| 回放控制 | PASS：50ms pacing、静默期间精确 TTL/缺口到期、原来源过滤、正常 EOF/异常断开、时间倒退、取消、后台截断错误及源文件保护 |
| 写入故障 | PASS：sink 故障注入，GUI 已显示/入队 1 不被当成功，sink_written 0、closed false、明确错误和 incomplete 标记；门禁缓存及 P1-3 观察不回滚 |

主要新增证据文件：`apps/desktop/src/backend_e2e_tests.rs`、`backend_replay_tests.rs`、`crates/langmuir-transport/src/pcapng.rs` 测试、`lvds.rs` 诊断测试及 `app.rs` 状态/交互测试。内存 sink 用例只证明事件和异步调用隔离；真实文件结论来自 BinarySink 临时文件关闭后重新读取。

## 8. 命令结果、Clippy 与环境限制

| 实际命令/检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS：206；core 89 / desktop 78 / CLI 5 / storage 9 / transport 25；另 1 ignored |
| `cargo test -p langmuir-desktop --features mock --locked e7_user_capture_strict_and_diagnostic -- --ignored --nocapture` | PASS：用户 PCAP 专项 1 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | FAIL：退出 101，原有 lint |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| `cargo test -p langmuir-storage --features hdf5 --locked --offline` | BLOCKED：缺少缓存依赖 `aho-corasick v1.1.5`；未执行 HDF5 测试 |
| `pkg-config --modversion hdf5` | 未找到可发现的 HDF5 包；本轮未安装库、更新锁文件或扩大依赖 |

首次沙箱内 workspace 测试的两个既有 localhost UDP 测试因 `Operation not permitted` 失败；随后仅本机回环权限下完整重跑通过，无遗留协议失败、无真实设备连接。

严格 Clippy 修改前及最终均实际 FAIL：未改的 `framing.rs:289,291` 的 `assert_is_empty`，CLI `main.rs:325` 的 `chunks_exact_to_as_chunks`，storage `binary.rs:143,146` 的 `explicit_iter_loop`。其他既有 UI 类别仍存在。本轮发现的过多 bool、MSRV API、长函数等新增问题已修复，没有新增全局 lint 禁用。

仅用于分离既有问题的最终含 Mock 诊断 PASS；不能代替上述严格 FAIL：

```text
cargo clippy --workspace --all-targets --features langmuir-desktop/mock --locked -- -D warnings
  -A clippy::chunks_exact_to_as_chunks -A clippy::explicit_iter_loop
  -A clippy::assert_is_empty -A clippy::too_many_lines
  -A clippy::semicolon_if_nothing_returned -A clippy::match_result_ok
  -A clippy::uninlined_format_args -A clippy::if_not_else
  -A clippy::manual_clamp -A clippy::items_after_statements
  -A clippy::assertions_on_constants
```

保留 `too_many_lines` 的额外诊断只报告原有五个 UI 函数：command_panel、telemetry_polling_control、telemetry_panel、fault_panel、frequency_parameter。新增 backend、回放、Mock、诊断函数没有该诊断。上面换行仅便于阅读。

HDF5 的源码和 schema 前后哈希一致，名称、dtype、形状和 `committed_frames` 未改；本轮不能把此静态保护核对说成 HDF5 运行兼容测试 PASS。

## 9. 原生 GUI 验收范围

原生 `--mock` 窗口实际启动成功，取得屏幕截图并检查初始三栏、CRC 按钮、零值统计、无数据等待和固定日志区域：**PASS（初始布局范围）**。第一次沙箱启动有 macOS 窗口服务错误；本机图形会话重新启动后可见。

弹窗、确认按钮、模式隔离和窄窗口控件操作通过实际 egui headless 指针/状态测试。原生窗口中 CRC 详情、复制、离线弹窗、真实回放图表及保存按钮的完整人工点击验收 **NOT_RUN**；不以编译或 headless 测试冒充已完成这些视觉操作。GUI 截图未加入仓库；本轮启动的 Mock 测试进程已关闭。

400 帧时间模拟不是设备实收 20 秒，也不是 UI 帧耗时/硬实时基准。原生连续帧率、长时间显示压力和真实网络断流恢复的人工验收仍为 NOT_RUN。

## 10. Git 变更与保护

本轮修改 8 个原有文件，新增 3 个 Rust 文件和本报告：

| 文件 | 本轮职责 |
| --- | --- |
| `crates/langmuir-core/src/lvds.rs`、`lib.rs` | 共享严格检查、独立未验证诊断类型/API、6 项新测试及导出 |
| `crates/langmuir-transport/src/pcapng.rs`（新增）、`lib.rs` | 有界只读 PCAPNG、8 项测试及导出 |
| `apps/desktop/src/backend.rs` | 复用生产 ingress、后台定时回放、详情计数、接收/保存状态、实时显示解耦 |
| `apps/desktop/src/bridge.rs` | 最小请求、状态与独立诊断事件 |
| `apps/desktop/src/app.rs` | 必要控件/弹窗、分离缓存、响应式行高及 5 项新增测试 |
| `apps/desktop/src/mock.rs` | 合成来源、拒绝伪造真实 CRC/回放成功、2 项新测试 |
| `apps/desktop/src/main.rs` | 无 GUI 审计命令入口 |
| `apps/desktop/src/backend_e2e_tests.rs`（新增） | 8 项默认闭环/故障测试及 1 项外部抓包专项 |
| `apps/desktop/src/backend_replay_tests.rs`（新增） | 7 项定时/后台/错误/取消测试 |
| `docs/LVDS_END_TO_END_ACCEPTANCE_REPORT.md`（新增） | 本报告 |

最终 `git diff --stat` 为四阶段及本轮累计 **21 个已跟踪文件、4493 insertions、387 deletions**，不是本轮独立差异，也不包含未跟踪的模块、测试、报告和用户 PCAP。

原重组 `udp_lvds.rs`、去重 `udp_lvds_dedup.rs`、序号监测 `udp_lvds_sequence.rs`、CRC 算法 `checksum.rs`、RS422、存储 crate 全部实现、CLI、Cargo 文件、五份历史报告及用户抓包与开始时哈希一致。历史修改仍保留，不因本轮停止 P1-4 而撤销。没有 Git 提交、重置、清理或删除用户文件。

## 11. 厂家待确认项与停止点

| 项目 | 状态与边界 |
| --- | --- |
| CRC16 算法参数 | 按用户提供的厂家算法保持 CCITT-FALSE |
| CRC 输入 `[7..267]` | SPEC_ASSUMPTION：当前软件协议解释，厂家尚未最终确认，未尝试替代范围 |
| `i8` 差分解释、ADC→mV 标定 | SPEC_ASSUMPTION：保留原解释及系数，mV 明示未校准 |
| PCAP CRC 全部不匹配的根因 | 未确认，不能仅凭该结果确定硬件、固件、覆盖范围或传输哪一环故障 |
| u32 的复位/回绕/重传及设备会话 | SPEC_ASSUMPTION：原 P1-3 只观察，不推测设备重启 |
| 真机端点可达性、CRC 正常硬件样本、实际断流/恢复、长时落盘 | NOT_RUN：需后续真机验收，不能用 Mock 代替 |
| 可选 HDF5 执行与完整原生 GUI 操作 | BLOCKED / NOT_RUN，范围见第 8、9 节 |
| P1-4 科学 Reader / Sidecar / 磁盘迁移 | NOT_RUN：本轮未实施 |

本阶段停止于软件接收、严格/诊断隔离、双通道显示与兼容科学保存闭环。没有改变 269B、24B、CRC 算法/覆盖、TTL、去重、P1-3、RS422、`.lpd` v2 32B/1010B 或 HDF5 schema；不继续 P1-4 或其他新阶段。
