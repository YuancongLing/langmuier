# LVDS 55/AA 最终协议与 lvds.txt 验收

日期：2026-10-24。依据本轮最新文字规范 `f2665c3b-8c21-4dae-b201-b9f5189f9f8e/Pasted text.txt`。

本报告保留协议验收时的程序名称、构建哈希和 Git 状态。最新软件名称 **Langmuier Probe Ground Control**、提交前验证和 Windows 构建记录见 [当前发布记录](qa/LVDS_55AA_RELEASE_CURRENT.json)。

**当前结论：用户提供的 20 帧最终 55/AA 协议验收 PASS；真实探针通信 NOT_RUN。** 用户最新聊天直接提供了 20 行原始 HEX，已保持字符和行序，按 ASCII、LF 换行及末尾换行转存工程根目录 `lvds.txt`。该转存文件为 10780B，SHA-256 为 `b5a2eee986fbe4f26da33da67bfeab17b8f9185f72ed0c6f81f232f3c2f15add`；此值标识本次聊天转存文件，不声称是此前未知磁盘文件的原始哈希。独立 Python/Rust 解码、20 帧地址负例、真实 Windows 非 Mock GUI 回放，以及实际生成的 2000 行 CSV 和 8220B LPD v3 全量读回均通过。

## 1. 状态与证据范围

| 验收项 | 状态 | 实际证据或限制 |
|---|---|---|
| 生产科学帧仅接受 `EB 90 55 AA 01 07 AA` | PASS | 共享 decoder 常量、逐地址负例、流式与 ScienceGate 回归 |
| 合成固定帧的 `55/AA` 正例、`04/FF` 负例 | PASS | 核心单元测试和桌面回放诊断测试；非用户数据 |
| 用户 HEX 来源、转存 SHA-256、20×269B 逐行读取 | PASS | 最新聊天 20 行原始 HEX，ASCII/LF 转存 `lvds.txt`，10780B |
| Python 独立解码、黄金值、20 帧时间/seq8 基线 | PASS | 每路 2000 点，完整参考 CSV/manifest 已生成 |
| 全部 4000 个 code10 的 Python/Rust 比较 | PASS | 生产 decoder 与独立 11 列参考全部一致，首帧黄金值正确 |
| 用户 TXT 派生 20 包 PCAPNG 生成 | PASS | `target/qa/lvds-55aa-user/lvds-from-txt-derived.pcapng`，人工网络封装 |
| 派生 PCAPNG 的真实 Rust Reader 逐字节核对 | PASS | 20 包端点、外序号、50ms 和全部内层字节一致，正常 EOF |
| 用户 TXT 仅改地址的 20 帧拒收、零绘图和零保存 | PASS | 内存仅改 offsets 2/3，20 地址错误，0 Accepted/绘图/记录 |
| 用户 20 帧 Windows 非 Mock GUI 双通道回放 | PASS | 20 接收/解析/Accepted/绘图/保存，0 失败，两路各 2000 点 |
| GUI CSV 2000 行全部 11 列逐点读回 | PASS | 2001 总行，22000 个字段全部一致，实际 GUI 文件 |
| GUI LPD v3 8220B、20 条、4000 码值读回 | PASS | 独立 Python 与真实 BinaryV3Reader 均正常读至 EOF |
| 最终地址 Windows 非 Mock 程序合成回放及双格式保存 | PASS | 明确标记的 200 帧合成样例，仅作应用流程验证 |
| Windows x64 static CRT 非 Mock Release | PASS | 最新构建和包校验，见下文 |
| 真实探针 RS422/UDP 通信、连续采集 | NOT_RUN | 未连接硬件、未发送命令 |

本轮用户 20 帧输入已完成生产链和真实 GUI 验收，无 FAIL 项。合成数据与历史 PCAP 仅保留为独立证据；本轮数据首末采样跨度为 0.9995 秒，没有进行或宣称 60 秒连续采集。

## 2. 生产修改与保护边界

- `crates/langmuir-core/src/lvds.rs`：`LVDS_SENDER_ADDRESS = 0x55`、`LVDS_RECEIVER_ADDRESS = 0xAA`。`decode_scientific_frame` 独立验证两个地址。流式解析、UDP ScienceGate、桌面正式接收、离线 PCAP 和 CLI 继续共享这一规则，没有兼容 `04/FF` 分支。
- `apps/desktop/src/backend.rs`：将错误地址观察字段改为 `wrong_address_frames`，仅识别完整 `EB9004FF0107AA` 候选以提供拒收原因。零可绘制帧的 EOF 显示失败；有效 `55/AA` 的其他字段错误仍显示实际错误。
- `apps/desktop/src/app.rs`：更新当前协议提示及错误回放说明。
- 核心、桌面、transport、CLI 和既有 Python 合成脚本中的科学头夹具按语义修正；保留全部拒收断言。没有全局替换十六进制字节。

269B、每路 125B→100 个 code10、原有 10bit 解包公式、`code10 * 64 - 32768`、500µs、设备首采样时间和 DATA250 seq8 均保持。DATA251 必须为 `00`，CRC 尾字段保留但不参与接收门禁，科学完整性为 `CRC_NOT_CHECKED`。24B UDP、2s TTL、5s/256 去重、完整 u32 序号监测、RS422 六条黄金指令、绘图交互、CSV 11 列和 LPD v3 40B/409B 布局不变。HDF5 对新科学协议仍禁用。

Git 初始即存在上一阶段的大量未提交迁移；本轮没有还原、暂存、提交或推送这些修改。`git diff` 相对 HEAD 的总量包含上一阶段，不能解释为本轮地址修正的范围。

## 3. 原始输入与本地测试工具

用户最新聊天粘贴的 20 行 HEX 是本轮数据来源；没有重新生成载荷、改动字段或用黄金值反向构造数据。转存格式为 ASCII、每行 LF、末尾保留 LF；每行 538 个 HEX 字符还原 269B，20 行合计 10780B 文本和 5380B 内层科学字节。下表哈希均对应本轮实际落地文件：

| 输入或参考产物 | 大小 | SHA-256 |
|---|---:|---|
| 根目录 `lvds.txt`（聊天 HEX 转存） | 10780B | `b5a2eee986fbe4f26da33da67bfeab17b8f9185f72ed0c6f81f232f3c2f15add` |
| `target/qa/lvds-55aa-user/lvds-from-txt-derived.pcapng` | 7516B | `e082a83fd079df35484f42b891b59e294849b747874be7410aedc8fee5c42813` |
| `target/qa/lvds-55aa-user/expected.csv` | 157266B | `307b5501af8205f93ad08be0e7d08e432e2f2bdb9b33eb042c1d289f906ee1eb` |
| `target/qa/lvds-55aa-user/reference.json` | 69698B | `d22426e035d89ecd50e83fc38042a084fcdee3dbd042949eb6300a5b4d24edb3` |

另一次独立全文转录审计逐行比较用户最新聊天的完整 20 行 HEX，转存字符及全部 5380 个科学数据字节完全一致，包括每帧原始 CRC 尾字节；未重算或修复 CRC。结果 PASS，证据为 `target/qa/lvds-55aa-user/pasted-input-audit.json`。验收后的 `lvds.txt` SHA-256 与上表一致。

Python `--assert-supplied-baseline` 已通过：20 帧固定头均为 `EB9055AA0107AA`，DATA251 为 `00`；设备秒数字段为 1700000000，微秒为 0–950000、逐帧增加 50000，seq8 为 0–19。每帧每路 100 点，两路各 2000 点，码值均为 0–1023；每路所有相邻采样时间（含跨帧）间隔为 500µs。首帧 CH1 前四码值为 `[886,805,723,642]`，CH2 为 `[846,784,721,659]`，首点有符号重建为 23936 和 21376；当前 Rust 生产 decoder 得到了相同值及全部 4000 个码值。完整参考 CSV 为 2000 数据行、2001 总行；该参考与下文实际 GUI 保存文件分别读取、逐字段比较。

`tools/qa/lvds_txt_to_pcapng.py` 仅作离线测试转换。逐行只去除首尾空白，验证连续合法 HEX、偶数字符及精确 269B；空行、非 HEX、内部空白和长度错误带具体行号。原始输入只读，记录 SHA-256，保持行序及科学帧所有字节。

转换时显式设置源/目标端点和 u32 起始序号，沿用既有 24B UDP 封装，生成标准 Ethernet/IPv4/UDP PCAPNG，包时间间隔 50ms。产物限制在工程 `target/qa`，拒绝覆盖抓包和来源旁车。`.provenance.json` 标明“用户原始 LVDS HEX + 人工 UDP/PCAPNG 封装”，没有将它称为原始网卡抓包。外层时间和序号是人工测试信息，内部科学数据不变。

另使用真实 `langmuir_transport::PcapNgReader` 读取转换器生成的两个明确标记的临时合成 UNIT 帧，实际验证来源注释可读、端点、293B UDP 负载、外序号、50ms 间隔、内层逐字节一致及正常 EOF，结果 PASS；证据 `target/qa/lvds-55aa-final/converter-actual-rust-reader.json`。本轮随后对用户 20 帧派生 PCAP 执行同一真实 Reader 保真验收，全部通过，见第 4 节真实数据日志。

`tools/qa/lvds_txt_reference.py` 使用 Python 标准库独立参考：每 5B 作为大端 40bit 整数，按 30/20/10/0 位移提取 4 个 code10，不调用 Rust 待测函数。已从本次 `lvds.txt` 生成完整参考 CSV/manifest，可逐字段验证 GUI CSV 和 LPD v3。`--assert-supplied-baseline` 验证上述 20 帧、时间、序号及第一帧黄金值，退出码为 0。

测试用临时文件仅是明确标记的单元测试夹具，不是用户文件。工具的正例及篡改输出负例覆盖 HEX 错误、顺序和字节保真、时间、码值、seq8、有符号 ADC、完整性、文件头及截断。

操作步骤见 [WINDOWS_BUILD.md](WINDOWS_BUILD.md#从原始-lvdstxt-回放)。本轮在仓库根目录使用以下输入和参数；工具拒绝覆盖现有输出，重跑时应使用新文件名：

```powershell
$lvdsInput = (Resolve-Path .\lvds.txt).Path
python tools/qa/lvds_txt_to_pcapng.py $lvdsInput target/qa/lvds-55aa-user/lvds-from-txt-derived.pcapng --source 192.168.1.128:1534 --destination 192.168.1.100:1534 --start-sequence 305419896
python tools/qa/lvds_txt_reference.py --txt $lvdsInput --reference-csv target/qa/lvds-55aa-user/expected.csv --manifest target/qa/lvds-55aa-user/reference.json --assert-supplied-baseline
```

上述来源端点与当前桌面回放来源过滤一致，目标为 UI 默认配置，仅为测试显式选值，没有假定真实硬件身份或修改本机 IP。本机 `python` 命令别名不可用时使用 `C:\Users\Administrator\miniconda3\python.exe`。

## 4. 原始数据生产链验收入口

`apps/desktop/src/backend_10bit_tests.rs` 中的显式原始数据验收不在默认测试中运行。缺少必须路径会失败，不会静默通过。先生成独立参考，再执行：

```powershell
$env:LANGMUIR_USER_LVDS_TXT = $lvdsInput
$env:LANGMUIR_USER_REFERENCE_CSV = (Resolve-Path target/qa/lvds-55aa-user/expected.csv).Path
cargo test -p langmuir-desktop --features mock --locked final55_original_txt_decoder_and_address_gate_contract -- --ignored --nocapture
```

该入口已通过（1 passed、0 failed、退出码 0），日志 `target/qa/lvds-55aa-user/rust-original-positive-negative.log`。原始科学帧送入生产 decoder 和真实 Ingress/Controller：20 帧解析成功、20 Accepted/绘图/保存事件、每路 2000 点、0 解析失败；逐点比较独立参考 CSV、采样时间、黄金值、seq8 和双通道。仅在内存改动 offsets 2/3 为 `04/FF` 的 20 帧负例全部返回固定地址错误，0 Accepted、0 绘图、0 入队和0 科学记录；只产生空文件头。原始 `lvds.txt` 未改动。

正式 GUI 在可写工作目录启动最新 EXE，选择 `LPD + CSV`，打开派生 PCAPNG，开启“回放时保存科学数据”，在第一帧前建立 writer。等待 EOF，记录双通道波形和保存状态截图。用实际生成的 CSV/LPD 路径执行：

```powershell
$env:LANGMUIR_USER_PCAP = (Resolve-Path target/qa/lvds-55aa-user/lvds-from-txt-derived.pcapng).Path
$env:LANGMUIR_USER_PCAP_SOURCE = '192.168.1.128:1534'
$env:LANGMUIR_USER_PCAP_DESTINATION = '192.168.1.100:1534'
$env:LANGMUIR_USER_OUTER_SEQUENCE_START = '305419896'
$env:LANGMUIR_GUI_SCIENCE_CSV = (Resolve-Path target/qa/lvds-55aa-final/native-run/data/scientific-20261024-181541.169.csv).Path
$env:LANGMUIR_GUI_SCIENCE_FILE = (Resolve-Path target/qa/lvds-55aa-final/native-run/data/scientific-20261024-181541.169.lpd).Path
cargo test -p langmuir-desktop --features mock --locked final55_derived_pcap_and_native_files_match_original_txt -- --ignored --nocapture
python tools/qa/lvds_txt_reference.py --txt $lvdsInput --assert-supplied-baseline --csv $env:LANGMUIR_GUI_SCIENCE_CSV --lpd $env:LANGMUIR_GUI_SCIENCE_FILE
```

第二个入口已通过（1 passed、0 failed、退出码 0），日志 `target/qa/lvds-55aa-user/rust-derived-pcap-native-readback.log`。真实 `PcapNgReader` 核对 20 包、显式端点、24B+269B、完整外序号 305419896–305419915、50ms 及全部内层字节；真实 `BinaryV3Reader` 读取全部 20 条并确认正常 EOF。CSV 全部 11 列逐行比较，首点无额外 200µs。LPD 检查版本 3、100 点、500µs、409B、全部双通道 code10、时间和 seq8。

上述两次 Rust 测试构建启用 `mock` feature，以复用现有自动化测试环境；实际调用的是生产 decoder、Ingress/Controller 和真实 PCAP/LPD Reader。下文原生 GUI 来自未启用 Mock 的正式 Windows Release，两者分别记录。

### 用户 20 帧原生 GUI 与文件读回

状态：PASS。实际启动交付目录的 Windows x64 非 Mock EXE（SHA-256 `9f3035c9a6687b249d6da3172f23964a628968c8ca1b08a19de2643b2b2f5eb9`），在可写目录 `target/qa/lvds-55aa-final/native-run` 回放用户 TXT 派生 PCAP。保存格式为 `LPD + CSV`，writer 在第一帧前开启；没有连接硬件，也未改变来源过滤或本机网络配置。

EOF 原生界面显示：接收 20、解析成功 20、解析失败 0、绘图 20、保存 20；关闭后的入队/写入均为 20，CSV 数据行 2000。生产门禁 Accepted 20 同时由对应 Rust 验收及全部保存帧读回确认，两通道各恢复 2000 点。实际操作验证了滚轮缩放、左键平移、重置/跟随、冻结及恢复；缩放后的原始 ADC 叠加与分屏曲线可辨。样本首末时间为 1700000000000000–1700000000999500µs，跨度 0.9995 秒。

- [用户数据 EOF 与保存状态](qa/LVDS_55AA_USER_EOF_WINDOWS.jpg)
- [双通道叠加曲线](qa/LVDS_55AA_USER_OVERLAY_WINDOWS.jpg)
- [双通道分屏曲线](qa/LVDS_55AA_USER_SPLIT_WINDOWS.jpg)
- [冻结操作](qa/LVDS_55AA_USER_FREEZE_WINDOWS.jpg)
- [实际 GUI 产物独立读回证据](qa/LVDS_55AA_USER_NATIVE_READBACK.json)

实际 GUI 输出位于 `target/qa/lvds-55aa-final/native-run/data/`：

| 原生 GUI 产物 | 大小 | SHA-256 |
|---|---:|---|
| `scientific-20261024-181541.169.csv` | 157266B | `307b5501af8205f93ad08be0e7d08e432e2f2bdb9b33eb042c1d289f906ee1eb` |
| `scientific-20261024-181541.169.lpd` | 8220B | `7cab73808160fa118329d07d1e56a971ab19357be55c1bdb03ae4a67384c9f02` |

Python 独立读回逐一比较 CSV 的 2000 数据行、全部 11 列（22000 个字段），包括两路 code10、有符号 ADC、整数时间、seq8 和 `CRC_NOT_CHECKED`；没有仅以行数判定。LPD 独立检查整个 40B 头、20 条409B记录和全部4000码值、时间、seq8及正常EOF；真实 Rust Reader 同样通过。两路未交换，DATA250/DATA251 未进入 ADC，所有采样间隔均为500µs。

## 5. 已完成的 Windows 合成流程验证

使用最终地址版交付 EXE、非 Mock、HDF5 关闭，工作目录 `target/qa/lvds-55aa-final/native-run`。没有连接串口或网络；用户主动离线回放明确命名的 `SYNTHETIC-final55aa-smoke.pcapng`。开始前选择 `LPD + CSV` 并建立 writers，实际观察两路波形，EOF 显示 200 Accepted/入队/已写，CSV 20000 数据行，两个文件关闭。

- [保存及曲线截图](qa/LVDS_55AA_SYNTHETIC_SAVING_WINDOWS.jpg)
- [EOF 及写入状态截图](qa/LVDS_55AA_SYNTHETIC_EOF_WINDOWS.jpg)
- [Python 全量读回](qa/LVDS_55AA_SYNTHETIC_NATIVE_READBACK.json)
- Rust 真实 `BinaryV3Reader` 证据：`target/qa/lvds-55aa-final/native-synthetic-rust-reader.json`，200 条记录、40000 个 code10、20000 行 CSV 全部 11 列及时间/seq8/ADC 一致。

| 合成流程产物 | 大小 | SHA-256 |
|---|---:|---|
| SYNTHETIC PCAPNG | 73648B | `338daee1c42126b585cce74ef7794e74e67c26773c5c5f5270b0bb2ad695d742` |
| GUI 科学 CSV | 1338897B | `170760edd4c885c97ca0074e8527aeabf7d3994986f1bb9c91dd964784c07520` |
| GUI 科学 LPD v3 | 81840B | `351ee8f2f67f18a64c24c51acc9e0aaf9f2e9489445e0be73a12781f3b381828` |

这些是合成流程证据；用户 TXT 的 2000 行/8220B 实际验收单列于第 4 节。

## 6. 历史 PCAP 的重新审查

独立固定字段审计保存在 `target/qa/lvds-55aa-final/existing-capture-independent-audit.json`。

| 文件 | 目标 UDP / 完整候选 | 当前固定字段通过 | 实际说明 |
|---|---:|---:|---|
| `102301.pcapng` | 153 / 153 | 1 | 全部地址为 55/AA，但 152 帧 DATA251 非零；当前生产路径实际接受 1 帧，双路各 100 点、CSV 100 行、LPD 449B 正常读回 |
| `20261019--朗缪尔.pcapng` | 795 / 795 | 0 | 705 候选固定头不符、794 保留字节非零、359 微秒超范围（分类可重叠） |

不能再以地址 `55/AA` 为旧协议拒收原因；也不能仅因地址匹配就宣布旧载荷符合最终 10bit 科学语义。上述文件不是最新规范描述的 20 帧 TXT。

原始 PCAP SHA-256 保持：

- `102301.pcapng`：`c0fe65e69ca8efe4f98fedd2758d6cb0a043e840ccf6e0814ad6acb1f7b76e6f`
- `20261019--朗缪尔.pcapng`：`3f7d3bb2c78f7757c3f04d6bd946e6f6710a76271b245a065e8aac7624d1a2ea`

## 7. 自动化回归与发布

加入 TXT 工具与验收入口后重新执行，所有下列命令退出码均为 0：

| 命令 | 结果 | 原始日志（`target/qa/lvds-55aa-final/`） |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | `fmt-current.log` |
| `cargo check --workspace --all-targets --locked` | PASS | `check-current.log` |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | **287 passed / 0 failed / 4 ignored** | `workspace-tests-current.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS，无诊断 | `clippy-default-current.log` |
| `python -m unittest discover -s tools/qa -p 'test_lvds_txt_*.py' -v` | **28 passed / 0 failed** | `python-txt-tests-current.log` |
| `git -c core.safecrlf=false diff --check` | PASS | `git-diff-check-final.txt` |

Rust 通过项为 core 92、desktop（Mock feature 构建测试）140、CLI 6、storage 24、transport 25。相比上一轮 285，增加两个严格输入/参考检查辅助测试；4 个 ignored 为既有两个显式用户 PCAP 验收及新增两个原始 TXT 验收，不计入通过数量。显式运行新增验收而缺少必需 TXT 环境变量时，两项都会失败并提示缺少输入；这是缺输入契约测试，不是原始数据测试失败或通过。默认非 Mock desktop 测试另通过 133 项、0 失败、4 ignored。

两项新增用户数据验收随后以 `--ignored` 显式运行，分别 1 passed、0 failed；这不改变默认工作区仍显示 4 ignored 的计数。回归包含 UDP 重组/TTL/去重/u32 序号，RS422 黄金指令，CRC 不检查，绘图时间和交互状态，CSV/LPD 布局与存储关闭错误路径。Mock feature 仅用于自动化回归；交付构建不启用它。用户 20 帧的原生验收见第 4 节，合成流程证据单列于第 5 节。

正式构建命令：

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

最终地址新构建日志 `target/qa/lvds-55aa-final/release-static.log` 记录成功（37.68s）；新增仅测试代码后再次运行上述正式构建，`release-static-current.log` 记录成功（Cargo 0.74s、总计 0.843s）。当前 EXE 与第 5 节实际启动并回放的最终地址 EXE 字节一致，SHA-256 相同，没有复用旧 `04/FF` EXE。

交付位置：`target/delivery/windows-x64/LangmuirGroundControl.exe`、`target/delivery/LangmuirGroundControl-windows-x64.zip`，最新 README 来自 `docs/WINDOWS_BUILD.md`，含原始 TXT 转换与逐点读回步骤。复用现有包检查器验证：PE32+ x64、与当前构建 EXE 字节一致、无动态 MSVC CRT 导入、ZIP 仅含 EXE/README 且内容/CRC 完全匹配。

| 交付文件 | 大小 | SHA-256 |
|---|---:|---|
| LangmuirGroundControl.exe | 9266176B | `9f3035c9a6687b249d6da3172f23964a628968c8ca1b08a19de2643b2b2f5eb9` |
| LangmuirGroundControl-windows-x64.zip | 3899908B | `bb3f8cef8f62b0b438a0442ccd101c6133ae3a7dfbafb7fc3ca02986483beafc` |
| README.md | 9458B | `d72e9728ed53466d612f5cce55a440d2d8835d65955b499992eb10e35526eae1` |

机器可读证据：[最终状态](qa/LVDS_55AA_FINAL_STATUS.json)、[Windows 包检查](qa/LVDS_55AA_WINDOWS_PACKAGE.json)、[用户 GUI 产物读回](qa/LVDS_55AA_USER_NATIVE_READBACK.json)。用户聊天 HEX 的转存来源和独立审计单列于第 3 节，不把人工网络封装或合成夹具解释为原始网卡抓包。

## 8. Git 修改清单与待完成事项

本轮最终地址相关文件：`crates/langmuir-core/src/{lvds.rs,framing.rs,lvds_protocol_tests.rs,lvds_test_support.rs}`；`apps/desktop/src/{backend.rs,app.rs,backend_10bit_tests.rs,backend_replay_tests.rs,backend_e2e_tests.rs,backend_scientific_csv_tests.rs}`；`crates/langmuir-transport/src/mock.rs`；`tools/protocol-cli/src/main.rs`；`tools/qa/lvds_10bit_reference.py`；新增 TXT 转换/参考脚本及各自单元测试；本报告、构建 README、历史报告当前版本链接、实施计划及明确标记的合成 GUI 证据。最新用户 HEX 已按上述来源说明转存根目录 `lvds.txt`；未暂存或提交该数据文件。

起始状态另有上一阶段存储、模型、绘图与 UDP 迁移及原始 PCAP 等未提交文件，全部保留。没有自动 Git 提交。

完整当前修改列表与相对 HEAD 统计分别保存在 `target/qa/lvds-55aa-final/git-status-final.txt`、`git-diff-stat-final.txt`；二者包含保留的上一阶段修改，不能用于声称本轮重写了全部文件。两个历史原始 PCAP 的 SHA-256 已在本轮结束前重新计算并与前值一致。

用户聊天 HEX 转存、完整字节审计、严格读取、Python/Rust 独立逐点比较、20 帧正/负例、派生 PCAP 真实 Reader 保真、正式 Windows GUI 和实际 CSV/LPD 双读回均已完成，转存输入 SHA-256 未变。本轮用户数据验收 PASS，按要求停止，不进入其他开发阶段、不自动提交 Git。真实探针通信与持续网络采集仍为 NOT_RUN。
