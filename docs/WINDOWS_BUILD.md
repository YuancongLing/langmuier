# Windows x64 构建与打包

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

此命令使用真实后端，不启用 Mock，也不启用可选 HDF5；科学数据默认保存为兼容原格式的 LPD，也可选择科学 CSV 或 LPD + CSV。静态 CRT 编译参数用于避免额外分发 MSVC 动态运行库。

构建成功后，继续在同一 PowerShell 窗口打包：

```powershell
New-Item -ItemType Directory -Force .\target\delivery\windows-x64 | Out-Null
Copy-Item .\target\x86_64-pc-windows-msvc\release\langmuir-desktop.exe .\target\delivery\windows-x64\LangmuirGroundControl.exe -Force -ErrorAction Stop
Copy-Item .\docs\WINDOWS_BUILD.md .\target\delivery\windows-x64\README.md -Force -ErrorAction Stop
Compress-Archive -Path .\target\delivery\windows-x64\LangmuirGroundControl.exe, .\target\delivery\windows-x64\README.md -DestinationPath .\target\delivery\LangmuirGroundControl-windows-x64.zip -Force -ErrorAction Stop
```

程序：`target\delivery\windows-x64\LangmuirGroundControl.exe`。

压缩包：`target\delivery\LangmuirGroundControl-windows-x64.zip`。

解压后运行 EXE。原始用户 PCAP 不随源码上传；回放时选择本机保存的 `.pcapng` 文件。当前版本不使用 CRC16 作为接收、绘图或保存门禁，也没有 CRC 模式开关。结构、设备时间、双通道 ADC 解码合法并通过原去重门禁的帧会直接绘图；用户开启保存后进入原正式写入链。**科学数据未经 CRC 完整性验证**，文件格式未变。监听地址须是 Windows 本机实际配置的 IP，程序不会自动改为其他地址。

## LVDS 科学 CSV 保存

在科学数据保存区的“保存格式”选择 `CSV`、`LPD` 或 `LPD + CSV`。窄窗口中通过原“更多”菜单设置。一次保存期间格式固定；停止保存后可以重新选择。`LPD + CSV` 使用同一 Accepted 科学帧流和保存区间，但两个文件没有原子事务保证。任何输出写入失败都会停止当前保存、显示错误并标记捕获不完整；已写入的文件保留。

完整离线导出：打开“离线 PCAP”，选择本机原始抓包路径，勾选“回放时保存科学数据”，确认格式为 `CSV`，点击“开始回放并保存”。程序先创建文件再处理第一帧，并在 EOF 正常关闭文件。原始抓包只读，不发送网络报文。如果先回放后点击“开始保存”，只保存点击之后的帧。真实 UDP 模式仍需先手动监听，再开始/停止保存；程序不会自动连接硬件。

科学 CSV 保存在启动工作目录的 `data` 目录，界面可查看完整路径。科学 CSV 与“导出遥测 (CSV)”不同：后者是 RS422 遥测快照。科学 CSV 每帧生成 125 行，两路原始 ADC 按相同采样索引并排保存，不经过图表缓存、抽稀、归一化、插值或电压换算。

表头为 `frame_index,sequence_u16,frame_seconds,frame_microseconds,sample_index,sample_time_us,ch1_adc,ch2_adc,integrity`。`sequence_u16` 仅为当前科学帧低 16 位序号；`sample_time_us` 按整数公式 `frame_seconds * 1000000 + frame_microseconds + sample_index * 400` 计算，不假设设备秒数字段的日历纪元。`integrity` 始终为 `CRC_NOT_CHECKED`。设备时间倒退时仍保留原 Accepted 顺序和原始数值。

状态中的“入队”不代表已写入，“CSV 数据行”来自已成功写出的批次；错误批次可能在文件末尾留下部分数据，须结合错误和 `.incomplete` 标记处理。正常关闭会排空已入队帧并 flush；这不承诺断电持久化。CSV 可用 Excel、MATLAB、Origin 或 Python 读取。Excel 单表最大为 1,048,576 行，较长捕获请使用 MATLAB、Python 等工具读取完整 CSV。

构建成功只表示生成了 Windows 程序；Windows 原生启动、界面、回放和硬件验收仍需在目标电脑实际执行。Mac Release 的验证结果不等于 Windows 发布验收。
