# 编辑器报红与离线回放诊断修复验收

本报告为厂家最终地址确认前的历史记录；其中 `04/FF` 接受规则及旧抓包拒收原因已被后续修正。当前 `55/AA` 协议与最新交付结果见 [LVDS_55AA_FINAL_ACCEPTANCE.md](LVDS_55AA_FINAL_ACCEPTANCE.md)。以下保留当时的观察和校验值。

日期：2026-10-24。此报告接续 [协议升级验收报告](LVDS_10BIT_PROTOCOL_MIGRATION_QA.md)，记录本次修复后的源码与 Windows 交付版；不覆盖历史结果。

## 编辑器与源码检查

用户指出的 `tools/protocol-cli/src/main.rs:57` 中 `frame.frame_seq_u8` 与 `crates/langmuir-storage/src/binary_v3.rs` 均已通过实际编译。VS Code Problems 原先还保留旧工具链的过程宏 ABI 错误：分析进程期望 Rust 1.97.1，而当前 DLL 由 1.99.1 构建。重启 rust-analyzer 并等待重新分析后，实际 Problems 面板和状态栏均显示无问题；未更换工具链或降低检查等级。

证据：[编辑器 Problems 清零](qa/REPLAY_FIX_EDITOR_NO_PROBLEMS_WINDOWS.jpg)。

## 无曲线的原因与行为修复

真实用户文件 `102301.pcapng` 包含 153 个目标 UDP 包、153 个完整候选科学帧；内层地址均为旧版 `55/AA`，与当前明确要求的新版 `04/FF` 不符，因此接受 0 帧。此前程序仍显示“离线回放完成”，容易误以为是图表故障。

现在 EOF 时若没有接受任何科学帧，程序显示“回放失败”和具体原因；区分没有匹配来源 UDP、旧协议候选帧、实际字段解析错误、外层或分片不完整。旧协议计数只观察完整候选帧，不改变源过滤、CRC 策略、重组、TTL、去重或序号算法。混合文件中存在合法新帧时仍正常完成；保存中的失败捕获沿用不完整标记。错误后可以重新选择文件并开始。

证据：[真实旧协议文件拒收提示](qa/REPLAY_FIX_OLD_PROTOCOL_WINDOWS.png)。真实文件仍不能用于验证新协议波形；没有将旧差分内容解释成新版 10bit 数据。新版厂家实测文件和硬件验收仍为 **NOT_RUN**。

## 自动化验证

新增 5 项回归：旧协议 EOF 失败及空 CSV 不完整标记、无匹配 UDP 来源、旧新协议混合流、非法设备微秒字段实际错误，以及 UI 失败原因显示和重新开始操作。修复前观察到相应失败，修复后通过。

同时清理全工作区严格 Clippy 的 39 项诊断，未添加 lint allow，未放宽等级。最终结果：

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo clippy --workspace --all-targets --features langmuir-desktop/mock --locked -- -D warnings` | PASS |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | **281 passed / 0 failed / 2 ignored** |

测试分布为 core 88、desktop 138 通过/2 忽略、CLI 6、storage 24、transport 25。中间一次并行运行的存储故障测试曾失败；单项重跑和最终全工作区运行均通过。未确认该次失败根因，没有据此修改生产超时逻辑；断言已增加实际错误输出，便于后续定位。

## 原生 Windows 交付版验证

对本次重新构建的非 Mock EXE 实际操作：

| 场景 | 结果 |
|---|---|
| 打开真实 `102301.pcapng` | PASS：明确提示旧 `55/AA`，153 帧拒收，开始按钮恢复 |
| 新协议独立合成文件，仅回放 | PASS：双通道曲线可见，200 包结束 |
| 新协议独立合成文件，回放并保存 CSV | PASS：200 帧入队/写入，20,000 数据行，EOF 正常关闭 |
| EOF 后再次开始 | PASS：从头回放 |
| 手动停止 | PASS：本次在第 182 帧停止，文件关闭，保留 18,200 行 |
| 手动停止后再次开始 | PASS：重新完成 200 帧/20,000 行 |

截图：[CSV 与 EOF](qa/REPLAY_FIX_CSV_EOF_WINDOWS.jpg)、[手动停止](qa/REPLAY_FIX_MANUAL_STOP_WINDOWS.jpg)、[停止后重播至 EOF](qa/REPLAY_FIX_RESTART_EOF_WINDOWS.jpg)。停止后“开始回放”是从头播放，不是断点续播；“冻结画面”只控制显示。

使用独立 Python 参考解包器逐行核对实际 GUI 生成的 CSV：200 帧、20,000 行、40,000 个双通道有符号 ADC 值，全部吻合。设备时间回退与原始顺序也通过读回校验。机读证据：[REPLAY_FIX_NATIVE_CSV_READBACK.json](qa/REPLAY_FIX_NATIVE_CSV_READBACK.json)。输入是独立合成夹具，不能代替厂家真实数据验收。

原生保存验证首次通过自动化启动器启动时，创建相对 `data` 目录遭到 OS 拒绝访问；改为显式可写工作目录 `target/qa/replay-fix/native-run` 后保存成功。程序依照原有约定写入启动工作目录的 `data`，没有改变保存位置或权限处理。使用说明已补充快捷方式“起始位置”应可写。没有声称首次启动器的实际工作目录已被测得。

## Windows 构建与交付

本次从最终源码重新完成 MSVC x64 非 Mock、HDF5 关闭、静态 CRT 的 Release 构建，耗时 42.38s：

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

交付 EXE 为 9,266,176B，SHA-256：`de0c356116efb9bfc96475749133370a0dff41a7b729d3b610fedaf980f6a45d`。打包校验确认交付 EXE 与构建产物字节一致，PE x64，未导入动态 MSVC CRT；ZIP 仅包含 EXE 与最新 README，内容和 CRC 全部吻合。最新包的大小和哈希见 [REPLAY_FIX_WINDOWS_PACKAGE.json](qa/REPLAY_FIX_WINDOWS_PACKAGE.json)。

交付路径：`target/delivery/windows-x64/LangmuirGroundControl.exe`、`target/delivery/LangmuirGroundControl-windows-x64.zip`。未自动暂存、提交或推送 Git；用户已有未提交变更和原始 PCAP 均保留。
