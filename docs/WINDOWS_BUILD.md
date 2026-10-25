# Langmuier Probe Ground Control Windows x64 构建与打包

需要 Git、Rust，以及 Visual Studio 2022 Build Tools 的“使用 C++ 的桌面开发”（MSVC x64/x86 工具和 Windows 10/11 SDK）。项目最低 Rust 版本为 1.92；以下使用最新 stable MSVC 工具链。推荐在 **Developer PowerShell for VS 2022** 中执行。

首次拉取：

```powershell
git clone --branch main https://github.com/YuancongLing/langmuier.git
cd langmuier
```

已有仓库时，在仓库根目录更新；本地如有未提交修改，应先保存并处理：

```powershell
git switch main
git pull --ff-only origin main
```

在含有 Cargo.toml 的仓库根目录构建 Windows x64 非 Mock Release：

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "Rust 工具链安装失败" }

$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
if ($LASTEXITCODE -ne 0) { throw "Release 构建失败，请勿打包旧产物" }
```

此命令使用真实后端，不启用 Mock，也不启用可选 HDF5；科学数据默认保存为新版 LPD v3，也可选择科学 CSV 或 LPD + CSV。静态 CRT 编译参数用于避免额外分发 MSVC 动态运行库。

构建成功后，继续在同一 PowerShell 窗口打包：

```powershell
New-Item -ItemType Directory -Force .\target\delivery\windows-x64 | Out-Null
Copy-Item .\target\x86_64-pc-windows-msvc\release\langmuier.exe .\target\delivery\windows-x64\LangmuierProbeGroundControl.exe -Force -ErrorAction Stop
Copy-Item .\docs\WINDOWS_README.md .\target\delivery\windows-x64\README.md -Force -ErrorAction Stop
Compress-Archive -Path .\target\delivery\windows-x64\LangmuierProbeGroundControl.exe, .\target\delivery\windows-x64\README.md -DestinationPath .\target\delivery\LangmuierProbeGroundControl-windows-x64.zip -Force -ErrorAction Stop
```

程序：`target\delivery\windows-x64\LangmuierProbeGroundControl.exe`。

压缩包：`target\delivery\LangmuierProbeGroundControl-windows-x64.zip`。

解压后运行 EXE。原始用户 PCAP 不随源码上传；回放时选择本机保存的 `.pcapng` 文件。厂家最终科学协议如下：

`LVDS SCIENCE: EB 90 55 AA 01 07 AA / 269B / 100 samples per channel / 10bit / 500us / CRC_NOT_CHECKED`

当前版本只接受内层 SRC `55`、DST `AA`；错误地址 `04/FF` 会拒收，不自动猜测协议版本。原 24B UDP 外层封装不变，其中 `55 AA` magic 与内层地址分别维护。当前版本不使用 CRC16 作为接收、绘图或保存门禁，也没有 CRC 模式开关。结构、设备时间及双通道解包合法并通过原去重门禁的帧直接绘图；用户开启保存后进入正式写入链。**科学数据未经 CRC 完整性验证**。监听地址须是 Windows 本机实际配置的 IP，程序不会自动改为其他地址；离线回放不需要修改本机绑定地址或连接串口。

## 原始录制与历史波形

1. 在中央操作区点击“打开历史数据”（窄窗口显示“历史”）。在“历史与原始数据”窗口选择 LVDSRAW、LPD v3 或 PCAPNG 文件，点击“打开”。可直接填写完整路径，无需连接串口或启动 UDP 监听。
2. 主界面进入“历史模式”，使用“播放 / 暂停”“起点”、进度条及“退出历史”控制。图表继续支持双通道叠加 / 分屏、左键平移、滚轮缩放、跟随、冻结与重置；设置窗口可指定最多 60 秒的时间窗。
3. 原始 UDP 保存使用独立的“开始原始录制 / 停止原始录制”。实时接收时保存当前 Socket 实际收到的全部 UDP Payload，包括来源不匹配和解码失败数据；PCAP 历史模式下导出整个抓包中的 UDP 记录。自动文件名位于 `data/`，也可在设置窗口指定新文件路径；同名文件不会覆盖。
4. 记录状态分别显示收到、入队、写入、丢失及关闭 / 错误。应用接收前的网卡或系统丢包无法由归档统计证明；异常退出、队列溢出或写盘失败会使归档标记为不完整。
5. “尝试恢复异常帧”默认关闭。启用后可选择显示原生、原生加已确认恢复，或明确标识的诊断候选。恢复只使用同一报文内实际存在的位；缺失尾部不会填造。恢复和候选不进入普通 LPD v3 / 11 列 CSV；需要时从历史窗口导出带版本、质量及来源信息的独立 CSV。

原始格式及验收结果见 [LVDSRAW 格式](LVDSRAW_FORMAT.md) 和 [本轮 QA 报告](LVDS_RAW_CAPTURE_RECOVERY_HISTORY_QA.md)。正式 EXE 不生成模拟科学数据，也不会自动连接或向硬件发送指令。

## 科学数据保存操作

在科学数据保存区的“保存格式”选择 `CSV`、`LPD` 或 `LPD + CSV`。窄窗口中通过原“更多”菜单设置。一次保存期间格式固定；停止保存后可以重新选择。`LPD + CSV` 使用同一 Accepted 科学帧流和保存区间，但两个文件没有原子事务保证。任何输出写入失败都会停止当前保存、显示错误并标记捕获不完整；已写入的文件保留。

完整离线导出：打开“离线 PCAP”，选择本机原始抓包路径，勾选“回放时保存科学数据”，确认格式为 `CSV`，点击“开始回放并保存”。程序先创建文件再处理第一帧，并在 EOF 正常关闭文件。原始抓包只读，不发送网络报文。如果先回放后点击“开始保存”，只保存点击之后的帧。真实 UDP 模式仍需先手动监听，再开始/停止保存；程序不会自动连接硬件。

### 从原始 lvds.txt 回放

`lvds.txt` 每行是一个 269B 内层科学帧的连续 HEX，不能直接在 PCAP 对话框中打开。在源码工程根目录使用 Python 3 标准库测试脚本进行本地封装；将输入路径替换为实际原文件路径，输出路径必须是新的 `target/qa` 测试产物：

```powershell
python tools/qa/lvds_txt_to_pcapng.py 'D:\实际目录\lvds.txt' target/qa/lvds-55aa/lvds-from-txt-derived.pcapng --source 192.168.1.128:1534 --destination 192.168.1.100:1534 --start-sequence 305419896
if ($LASTEXITCODE -ne 0) { throw 'HEX 或封装校验失败' }
python tools/qa/lvds_txt_reference.py --txt 'D:\实际目录\lvds.txt' --reference-csv target/qa/lvds-55aa/expected.csv --manifest target/qa/lvds-55aa/reference.json --assert-supplied-baseline
if ($LASTEXITCODE -ne 0) { throw '原始数据基线不符' }
```

这里显式设置的来源端点与当前桌面离线回放来源过滤一致，目标端点采用当前 UI 默认配置；仅用于测试封装，不表示已连接该硬件，也不修改 Windows IP 或监听配置。脚本保留原文件 SHA-256、逐行字节和顺序，添加既有 24B UDP 外层，以 50ms 间隔封装标准 PCAPNG，并生成 `.provenance.json` 来源记录。来源是“用户原始 LVDS HEX + 人工 UDP/PCAPNG 封装”，不是原始网卡抓包。任何现有输出均拒绝覆盖，重新执行时选择新的文件名。

用正式程序选择生成的 `lvds-from-txt-derived.pcapng`，选择 `LPD + CSV`，开启“回放时保存科学数据”，点击开始并等待结束。然后用原始 TXT 独立逐点核对实际生成的两个文件：

```powershell
python tools/qa/lvds_txt_reference.py --txt 'D:\实际目录\lvds.txt' --assert-supplied-baseline --csv 'D:\实际输出\scientific.csv' --lpd 'D:\实际输出\scientific.lpd'
```

提供的 20 帧基线应得到每路 2000 点、CSV 2000 数据行及 8220B LPD v3；脚本逐字段核对全部时间、码值、有符号 ADC 和完整性标记，遇到不一致报告具体位置。当前实际执行状态见工程文档 [LVDS_55AA_FINAL_ACCEPTANCE.md](LVDS_55AA_FINAL_ACCEPTANCE.md)。本段是操作步骤，不代表原始文件验收已经完成。

点击开始后没有曲线时，查看回放窗口中的错误原因。地址匹配仍须通过长度、类型、微秒范围和 DATA251 保留字节 `00` 等检查。历史 `102301.pcapng` 的地址为 `55/AA`，但 153 帧中有 152 帧的 DATA251 非零；不能仅凭地址相同确认载荷符合最终 10bit 协议。回放到文件末尾仍没有可绘制科学帧时，会显示“回放失败”和具体原因；错误 `04/FF` 会明确提示当前要求 `55/AA`。若同时保存失败捕获，文件会标记不完整。停止或结束后再次点击开始会从文件头重新播放。“冻结画面”只冻结显示，后台回放仍会继续。

科学 CSV 保存在启动工作目录的 `data` 目录，界面可查看文件相对路径。启动目录必须可写；使用快捷方式时，将“起始位置”设为可写目录。如果提示 `could not create parent directory data` 或“拒绝访问”，请从可写目录重新启动程序后再保存；保存无法启动时，本次回放会取消。科学 CSV 与“导出遥测 (CSV)”不同：后者是 RS422 遥测快照。科学 CSV 每帧生成 100 行，两路原始 10bit 码值及有符号重建 ADC 按相同采样索引并排保存，不经过图表缓存、抽稀、归一化、插值或电压换算。

表头为 `frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity`。`frame_seq_u8` 来自内层 DATA250；它与 UDP 外层完整 u32 序号独立，不生成旧 u16 序号。`sample_time_us` 按整数公式 `frame_seconds * 1000000 + frame_microseconds + sample_index * 500` 计算，首点直接使用设备时间戳，不重复加 200μs，不假设设备秒数字段的日历纪元。重建 ADC 为 `code10 * 64 - 32768`，范围 -32768..32704，属于有损量化重建结果，未标定为电压。`integrity` 始终为 `CRC_NOT_CHECKED`。设备时间倒退时仍保留原 Accepted 顺序和原始数值；图表开始新的显示窗口。

新版 LPD v3 使用 40B 版本化文件头和每帧 409B 记录，保存每通道 100 个原始 u16 code10、原始设备时间和内层 seq8，明确标识 500μs 采样间隔及 CRC 未检查。旧 v2 的 32B 头、1010B 记录、125 点/u32 数据布局保留在 legacy 存储接口中，旧文件不修改，新数据不会补 25 个点伪装为 v2。已有仅支持 v2 的分析程序必须适配 v3 后再读取新文件。可选 HDF5 legacy schema 仍为 `[N,125]`，本轮禁止用于新协议，界面明确禁用；请选择 CSV 或 LPD v3。

状态中的“入队”不代表已写入，“CSV 数据行”来自已成功写出的批次；错误批次可能在文件末尾留下部分数据，须结合错误和 `.incomplete` 标记处理。正常关闭会排空已入队帧并 flush；这不承诺断电持久化。CSV 可用 Excel、MATLAB、Origin 或 Python 读取。Excel 单表最大为 1,048,576 行，较长捕获请使用 MATLAB、Python 等工具读取完整 CSV。

构建成功只表示生成了 Windows 程序；Windows 原生启动、界面、回放和硬件验收仍需在目标电脑实际执行。Mac Release 的验证结果不等于 Windows 发布验收。
