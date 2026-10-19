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

此命令使用真实后端，不启用 Mock，也不启用可选 HDF5；默认科学文件保存使用原 BinarySink。静态 CRT 编译参数用于避免额外分发 MSVC 动态运行库。

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

构建成功只表示生成了 Windows 程序；Windows 原生启动、界面、回放和硬件验收仍需在目标电脑实际执行。Mac Release 的验证结果不等于 Windows 发布验收。
