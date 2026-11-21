# LVDS-MULTITYPE-S02：AB 激励信号验收

日期：2026-11-18。软件实现与自动化验证记录如下。Windows 原生 GUI 操作/截图尚未运行，真实 AB 设备输入尚未提供；这些项目不计为通过。本阶段止于 AB，未增加 AC 业务解码。

## 1. 协议依据与字段

直接读取原始 `协议 (PH20261115).xlsx` 的 `LVDS_激励信号数据包!I17:J31`、`F34:F38` 和 `LVDS_公共格式`。工作簿 SHA256：`916a0f154ad71e2325a5484cb1345de0fbe18319133282c83c7b73ea3368c11f`。逐单元格原文见 [protocol-cells.json](qa/lvds-multitype-s02/protocol-cells.json)，读取脚本为 `tools/qa/lvds_excitation_workbook_audit.py`，使用标准库直接读取 XLSX XML。

未发现提示与 AB 字段表冲突。J24 明确三角/正弦 scan_freq 为 0–100；J25 另要求非 DC 模式 scan_voltage_mV 填 0，均已落实。J21 仅给出 interval_us 示例 10800，没有非零或固定值约束，因此不增加 interval=0 拒收条件；DC 的 scan_freq 无定义，保留原字节、不赋予含义或据此拒绝。公共格式 F24 虽定义 CRC 算法，本轮依用户要求不执行生产 CRC 门禁，仍保留原 CRC 并标记 `CRC_NOT_CHECKED`。

DATA 起于整帧偏移 7；多字节全为大端。

| DATA 偏移 | 长度 | Rust 字段/类型 | 单位与校验 | AB 工作表依据 |
|---|---:|---|---|---|
| 0 | 4 | timestamp.utc_seconds / u32 | 2018-01-01 起秒，wave[99] 时间 | I18:J18 |
| 4 | 4 | timestamp.microseconds / u32 | µs，0–999999 | I19:J19 |
| 8 | 4 | f_act_mhz / u32 | mHz，除 1000 为 Hz | I20:J20 |
| 12 | 2 | interval_us / u16 | 采样间隔 µs | I21:J21 |
| 14 | 2 | points / u16 | 固定 100 | I22:J22 |
| 16 | 1 | work_mode / u8 | 55 三角、AA 正弦、BB DC | I23:J23 |
| 17 | 1 | scan_freq / u8 | 三角/正弦设定 0–100；DC 保留 | I24:J24 |
| 18 | 2 | scan_voltage_mv / u16 | DC 0–5000mV；其它模式 0 | I25:J25 |
| 20 | 1 | ad9833_status / u8 | 原始 0/1，无好坏推断 | I26:J26 |
| 21 | 1 | dac8563_status / u8 | 原始 0/1，无好坏推断 | I27:J27 |
| 22 | 1 | sci_frame_seq / u8 | 最近 AA 包序号，独立于 PA0 | I28:J28 |
| 23 | 1 | pa0_seq / u8 | AB 序号，可 255→0 | I29:J29 |
| 24 | 36 | 校验后不另复制 | 全 0 | I30:J30 |
| 60 | 200 | wave_mv / Box<[u16;100]> | mV，0–3300 | I31:J31、F36 |
| 整帧 267 | 2 | raw_crc / [u8;2] | 原样保留，不校验 | F34、F38 |

## 2. 模型、路由与时间

`crates/langmuir-core/src/excitation.rs` 提供 `decode_excitation_frame(&LvdsCommonFrame)`、`ExcitationFrame`、`ExcitationRecord` 和分类错误。模型携带设备字段与原 CRC；记录另携带完整外层 u32、原始记录 ID、来源、采集会话和原接收 Unix 纳秒时间。

沿用 S01 公共帧解析与 `LvdsPacketType::Excitation`，没有创建新 UDP Socket 或独立接收线程。既有辅助重复/冲突门禁先执行，只有 Accepted AB 才解码和正式保存；重复/冲突独立计数，AB 字段错误不污染 AA 解析失败计数。原始录制仍在来源过滤及解码前保存原 UDP。

按 Excel J18、F35，`sample_time_us(i) = latest_us - (99-i)*interval_us`；u64 运算、checked_sub 防下溢，不借用接收时刻。Golden triangle 最新 9.250000s、interval=10800µs，首点 8.180800s，跨度 1.0692s；sine 首点 8.980800s，窗口与前帧重叠 269200µs。测试同时覆盖跨秒、边界索引、最大设备秒、interval=0 与时间下溢拒收。

实际频率 931mHz→0.931Hz；AB 电压 3300mV→3.300V。AB 不调用 AA code10 电压公式。频率零或全零波形只要结构合法就接纳；全零显示“全零数据 / 有效性未确认”，不推断通信故障、厂商 valid 位或状态好坏。

## 3. 独立保存与队列

`crates/langmuir-storage/src/excitation_csv.rs` 为独立 CSV Sink；AA 的 11 列 CSV 与 LPD v3 40B/409B 格式不变，LPD 不写 AB。AB 表头为用户指定 19 列，后附 `source,session,received_unix_nanos` 共 22 列。每 Accepted AB 写 100 行，保留精确 mV、整数微秒、三位 V/Hz、模式、状态、PA0/科学序号与 `CRC_NOT_CHECKED`。

保存按接纳/到达顺序，不按设备时间排序，不裁剪重叠、逆序或重复时间窗；只有外层重复/冲突包被门禁抑制。文件独占创建，禁止覆盖；UTF-8 CSV 对逗号、引号及换行来源字段进行转义。写入/flush 错误会停止该 Sink，避免重试半条记录。

`apps/desktop/src/backend_excitation.rs` 复用 `BatchWriter`。独立 AB 业务队列上限 1024，正式存储队列 512，非阻塞 submit；视觉队列 128 与显示事件均不参与正式存储供给。界面切换和图表冻结不影响后台保存。状态分别显示入队、成功写帧、行数、丢失、关闭及错误。

过载测试：200 个 AB 挤掉视觉队列 72 项，正式 CSV 仍为 200 帧/20000 行；1100 个未消费 AB 将业务缓存限定在 1024，明确计数 76 次业务丢失；确定性暂停 Sink 测试覆盖正式队列拒收。关闭超时不会把仍在写的记录提前算作确定丢失，后台完成后重新核对 written 与 dropped，保留超时错误。

EOF、主动停止和有效来源断开/切换关闭 CSV；断开状态可预先开启 CSV，再启动第一份回放。实时原始录制会将原始 Header.session_id 传入 AB 记录，与历史重扫会话一致。失败重开不会覆盖旧文件。

## 4. 界面和历史

新增 `app_excitation.rs`，复用原中央工作区，提供“科学波形 AA / 激励波形 AB”选择。AB 保留单帧全部 100 点、独立 plot ID/跟随/手动/冻结状态；平移或缩放后保留当时完整帧，新数据不会抢回视图。恢复跟随后显示最近帧。AA 缓存与视图状态独立，切换不销毁波形。

“AB 详情”展示实际模式、实际频率、设定值、原始状态、序号、最新设备/接收时间、来源及统计；“AB CSV”提供独立保存/导出。窗口尺寸测试覆盖 live/history 及不同宽度的固定工具栏和底部布局。egui 输入测试实际注入鼠标拖动、滚轮与游标事件，校验平移缩放、帧保留和点值；这些不是 Windows 原生操作或截图。

`backend_history.rs` 和 `backend_excitation_history.rs` 在后台扫描 PCAP/LVDSRAW，沿用同一重组/去重/AB decoder。只保留一份所选 AB、128 项错误页、128 项辅助原始页以及有界索引；每原始记录消费 AB 队列，不把整份 AB 历史加载到 egui。

AB 上一帧/下一帧/指定帧选择使用独立可取消子任务，按需从文件前缀重扫以保持完整分片和门禁上下文，时间复杂度 O(前缀长度)，不是常数时间随机访问。AA 导航不会取消 AB 选择，AB 命令队列满时不会取消已接纳选择；CSV 导出独立后台任务、显式关闭。Raw 会话来自原 Header.session_id；PCAP 使用稳定文件来源标识。错误页显示原始记录 ID、外层 u32 与字段原因。

实时 AB 批次立即发布权威来源/会话状态，迟到显示事件必须与其匹配；避免旧源事件覆盖新帧或断开后复活旧波形。两项顺序回归均先观察到失败，修复后通过。历史导出在进行中/失败时只标注“输出路径”，不会误称已经导出。

## 5. 独立向量与混合流证据

标准库 Python 编码器：`tools/qa/lvds_excitation_reference.py`；固定向量位于 `crates/langmuir-core/tests/fixtures/excitation/`。四份 SYNTHETIC 二进制为 triangle/sine/DC/all_zero，附逐点时间及 mV manifest 和独立预期 400 行 CSV。Rust 实际 decoder 读取固定字节，Sink 输出与 Python 400 行预期全文相等。

覆盖三种模式、0/3300mV、931mHz、10800µs、PA0 255→0、独立 science seq、跨秒倒推、全零、保留全零、原始状态 0/1 和任意 CRC。错误覆盖 points、reserved、波形越界、mode、scan_freq、DC/非DC 电压、微秒、时间下溢与状态越界。

SYNTHETIC `20AA+1AB+1AC.pcapng` 共 22 UDP：AA Accepted20，AB decoded1/100点/CSV100行，AC raw1，外层假缺口0。另测试缺 AB、缺 AC、交换辅助顺序不影响 AA20/seq8。分片、过期、来源/目标变化、重复/冲突、业务与保存队列过载均有专项；S01 的全 B3 不合法 AB 仍只作为公共分流样本，未当作新波形向量。

非 Mock `non_mock_synthetic_pcap_replay_closes_ab_and_preserves_aa_files` 在真实回放/Controller/BatchWriter 链路运行，AA CSV2000行、LPD8220B，独立 AB CSV100行且 EOF 关闭、dropped0。记录位于 `target/qa/lvds-multitype-s02/nonmock-replay/`，日志 [nonmock-mixed-replay.log](qa/lvds-multitype-s02/nonmock-mixed-replay.log)。Python 独立读回逐点100项时间、mV、V、Hz、模式/状态/完整性，均 PASS；来源为 `192.168.1.128:1534`、raw_record_id=21、外层 u32=305463308，session 非空且不是 unavailable。证据：[读回](qa/lvds-multitype-s02/ab-csv-independent-readback.json)、[来源核对](qa/lvds-multitype-s02/ab-csv-provenance.json)。最终源码另重跑非 Mock 回放并保留 `nonmock-mixed-replay-final.log`；新 CSV 位于 `target/qa/lvds-multitype-s02/nonmock-replay-final/`，[最终独立逐点读回](qa/lvds-multitype-s02/ab-csv-final-independent-readback.json) 同为 PASS/100行。

## 6. 回归、发布与验证边界

四项最终检查：

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，退出码 0 |
| `cargo check --workspace --locked` | PASS，退出码 0 |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS，469 passed / 0 failed / 7 ignored |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS，退出码 0 |

日志位于 `docs/qa/lvds-multitype-s02/`。469 包含原有 AA、原始归档、历史、RS422 星务模拟与传输回归；本轮较 S01 增加 49 项默认通过测试。Clippy 首轮发现三处测试断言风格，已修正后通过。

命令、退出码、测试总数及发布信息汇总：[verification-summary.json](qa/lvds-multitype-s02/verification-summary.json)。

另显式运行真实 AA 五项：原始 TXT/address gate、真实 PCAP 科学 LPD 读回、科学 CSV 单/双输出读回、原始全量归档读回、独立恢复审计。共 5 passed / 0 failed，不重复计入默认 469。原始 PCAP `D:/RUST/2026102401.pcapng` SHA256 `1bfe2ff956ad4e372bb15c2da04910c3d6a469b85d772ccbf83e4da4e48cca07`：467 UDP、136830 payload bytes、237 原生 AA、229 recovery candidate、1 rejected；AB/AC=0。

真实 AA 保存为 237 科学帧、每通道23700点、CSV23700行、LPD96973B；原始新归档 `target/qa/lvds-multitype-s02/real-AA-roundtrip.lvdsraw` 全467记录逐字节/元数据一致、正常 footer/EOF、dropped0。详见 `real-AA-raw-tests.log`、`real-AA-science-tests.log`、`original-AA-TXT-tests.log`。

以下项目明确不计 PASS：

| 项目 | 状态与原因 |
|---|---|
| Windows 原生 GUI 点击、模式/时间轴截图、平移缩放、冻结恢复及 GUI触发CSV读回 | NOT_RUN：当前会话未提供 `node_repl`/Windows Computer Use 工具；不以 egui 或后台测试代替 |
| 真实 AB 波形、硬件状态语义、真实混合调度和长期设备负载 | BLOCKED：未提供真实 AB/AC 抓包或设备输入；现有真实 AA 抓包不能证明 AB 真机行为 |
| 派生 PCAP 对本轮原生GUI保存文件专项 | NOT_RUN：本轮没有原生GUI生成文件，不复用旧文件冒充 |
| CPU benchmark | NOT_RUN：默认 ignored 的性能项未作为正确性验收 |

[Computer Use SKILL.md](<C:/Users/Administrator/.codex/plugins/cache/openai-bundled/computer-use/26.928.31416/skills/computer-use/SKILL.md>) 要求 “Run this once per fresh `node_repl` JavaScript session:” 初始化 `@oai/sky`，其 [guidance.md](<C:/Users/Administrator/.codex/plugins/cache/openai-bundled/computer-use/26.928.31416/docs/guidance.md>) 指定 “Use `node_repl` JavaScript for all Computer Use actions.”。工具目录两次查找均无对应可调用工具，未自行拼装 UI Automation 替代流程，也未生成虚构截图。

## 7. Windows EXE

最新源码构建命令（退出码0，41.51秒）：

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

构建日志：[windows-release-build.log](qa/lvds-multitype-s02/windows-release-build.log)。正式交付仅新建单个 EXE，遵循用户“不要压缩包，只要exe软件”的偏好，没有创建新 ZIP，也没有覆盖 S01 EXE。

| 项目 | 结果 |
|---|---|
| EXE | `target/delivery/lvds-multitype-s02-20261118/LangmuierProbeGroundControl.exe` |
| 大小 | 10787328 字节 |
| SHA256 | `8ab70f1d12991deedcea554e0f20f749160046ec1074a5e637fb7b79b29ec419` |
| PE / CRT | PE32+ AMD64；无动态 CRT 导入；交付文件逐字节等于 release 构建 |
| 非 Mock | `--no-default-features` 构建；EXE 的 `--mock` guard 实际拒绝，未启动GUI或模拟数据 |
| 新 EXE 混合 PCAP CLI | 22UDP、AA20、ABdecoded1、AC1、无结构错误/假缺口；不执行文件保存 |
| 新 EXE 真实 AA CLI | 467UDP、科学237、AB/AC0；不执行文件保存 |

PE/imports/hash：[windows-exe-verification.json](qa/lvds-multitype-s02/windows-exe-verification.json)；运行证据为 `release-mixed-pcap-audit.log`、`release-real-AA-audit.log`、`release-nonmock-guard.log`。CLI 的 `file_frames=NOT_RUN` 保持真实功能边界，不以 CLI 审计冒充 GUI CSV 保存。程序默认不自动连接设备、发送 RS422 控制或生成模拟波形。

## 8. Git 与使用

没有自动 Git 提交、丢弃修改或删除原始输入。保留 S01、星务模拟器、旧阶段交付物及用户原有文档替换。阶段新增/修改主要为：

- Core：`excitation.rs`、lib 导出及固定向量/核心测试。
- Storage：`excitation_csv.rs`、lib 导出及完整 CSV/故障测试。
- Backend/bridge：`backend.rs`、`backend_excitation.rs`、`bridge.rs`、`mock.rs` 的新请求拒绝分支；`backend_excitation_tests.rs`。
- History：`backend_history.rs`、`backend_excitation_history.rs`、`backend_excitation_history_tests.rs`。
- UI：`app.rs`、`app_history.rs`、`app_excitation.rs`、`app_excitation_tests.rs`、`app_layout_tests.rs`。
- QA/说明：本报告、S02计划、Python独立编码器/工作簿读取器、QA证据及 `docs/WINDOWS_README.md`。

完整工作区清单见最终 `git-status.txt`；它同时包含用户既有未提交修改，不能把所有变动归于 S02。

使用：双击新 EXE → 打开 PCAPNG/LVDSRAW 历史 → 中央选择“激励波形 AB” → “AB 详情”逐帧查看 → “AB CSV”导出。实时使用原 UDP 监听，不需要额外 AB 端口；要从回放第一帧正式保存，先停止旧来源、开始 AB CSV，再启动离线回放。AA 保存仍用原“开始保存”。完整说明见 [WINDOWS_README.md](WINDOWS_README.md)。
