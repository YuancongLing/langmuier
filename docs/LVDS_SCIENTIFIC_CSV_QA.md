# LVDS 双通道科学 CSV 保存验收

本轮实现 CSV / LPD / LPD + CSV，复用原 Accepted 科学帧链和有界 BatchWriter。Windows x64 非 Mock Release 已原生启动、回放最新用户 PCAP、生成 CSV、正常 EOF 关闭，并对 GUI 实际文件完成全部字段和双通道逐点读回。

**最终验收输入按用户后续指示改为 `D:\RUST\langmuier\102301.pcapng`。该文件不是任务初始 1053 帧基线文件；本次实际结果为 153 帧、19125 行数据，未硬编码旧基线。** 严格 Clippy 仍为既有问题导致的 FAIL，未通过放宽 lint 宣称通过。真实硬件验收 NOT_RUN。初始开发验收阶段未自动提交；验收后用户明确要求“更新去github上去”，后续 Git 提交和推送按该授权执行。

## 1. 科学 CSV 与遥测 CSV

科学 CSV 直接消费已经通过结构、设备时间、ADC 解码和原去重门禁的 `ScientificFrame`，每帧保存双通道各 125 个原始 ADC 点。数据不经过图表的 60 秒窗口、峰谷抽稀、归一化、插值、平滑或 mV 估算。

“导出 RS422 遥测 (CSV)”仍是原来的遥测快照导出；科学保存区和回放窗口中的 CSV 才是本轮 LVDS 科学 CSV。原遥测导出实现不变。

## 2. 存储实现和状态含义

- `crates/langmuir-storage/src/scientific_csv.rs`：`ScientificCsvConfig`、`ScientificCsvSink`、`ScientificCsvError`，实现现有 `BatchSink<ScientificFrame>`。仅用标准库和现有依赖，Cargo.toml / Cargo.lock 无改动。
- 文件以 `create_new` 独占创建；默认 BufWriter 为 1 MiB，逐帧复用约 16 KiB 的格式化缓冲区。按批次 CSV flush，不逐点 flush；包装层也会调用 flush。
- `apps/desktop/src/backend_scientific_storage.rs`：同一个 BatchWriter 将相同的有序批次交给所选输出。未重写 BatchWriter，未改动 BinarySink 编码和 LPD v2 文件结构。
- 原队列容量 512、批次 100、定时 flush 2 秒继续使用；所有文件创建、写入、关闭均在后端/存储线程，egui 不执行大批文件 I/O。
- 入队数来自 BatchWriter 的提交行为；CSV 帧数/行数及 LPD 帧数在各输出实际 write + flush 成功后更新。双格式的公共写入数为两个已确认帧数的较小值。
- 文件关闭、错误独立显示。关闭超时后继续读取共享进度，保留超时错误和不完整标记，不把后台最终关闭冒充此前已经成功关闭。

上述 write / flush 是操作系统文件接口确认，不是断电持久化保证。失败批次可能已经留下部分末尾字节，已确认计数不包括该失败批次，不能仅凭文件存在判断保存完整。

## 3. CSV 字段与设备时间

标准 UTF-8，无 BOM，逗号分隔，一行表头：

```csv
frame_index,sequence_u16,frame_seconds,frame_microseconds,sample_index,sample_time_us,ch1_adc,ch2_adc,integrity
```

| 字段 | 含义 |
|---|---|
| frame_index | 本文件从 0 开始的科学帧记录序号 |
| sequence_u16 | 现有 ScientificFrame 的低 16 位序号，不恢复高位 |
| frame_seconds / frame_microseconds | 原设备帧起始时间字段 |
| sample_index | 0..124，两通道共同采样索引 |
| sample_time_us | 整数设备采样时间，不转换日历日期 |
| ch1_adc / ch2_adc | 解码后的双通道原始 ADC，无电压列 |
| integrity | 全部固定为 CRC_NOT_CHECKED |

时间使用 `u64`：`seconds * 1_000_000 + microseconds + sample_index * 400`。两个设备字段为 u32，125 点范围内该表达式不会溢出 u64；最大 u32 秒数和跨秒已测试。设备秒数字段的纪元尚未确认；CSV 不使用 UDP 到达时间、浮点时间或 Unix 日期推断。

## 4. 三种模式与生命周期

| 模式 | 结果 |
|---|---|
| LPD（默认） | 仅原兼容 LPD v2 |
| CSV | 仅科学 CSV，每帧 125 行 |
| LPD + CSV | 同一批次/顺序/保存区间同时输出两个文件 |

格式在启动时固定，保存期间下拉框禁用，后端也拒绝替换活动捕获。可选 HDF5 feature 保留原 schema 和单独 HDF5 选项；本次 Windows 正式包不启用 HDF5，HDF5 原生运行 NOT_RUN。

离线操作：选择 CSV → 打开离线 PCAP → 路径 `102301.pcapng` → 勾选“回放时保存科学数据” → “开始回放并保存”。writer 在处理第一帧前创建，EOF 在最后一批交付之后排空并关闭。中途停止保存后回放和波形可继续。真实 UDP 仍由用户先开始监听，再独立开始/停止保存，没有自动连接真实设备。

任何输出 write、flush 或 close 失败，当前捕获停止并显示具体错误，已写入的数据保留，对已创建文件记录 `.incomplete`。其中一个输出失败仍会尝试写入同一批的另一输出，随后终止；两文件不存在原子事务保证。创建第二文件失败时，已创建的第一文件关闭并标记不完整，同名旧文件不覆盖。

## 5. 最新真实 PCAP 结果

输入大小 **56740 B**；开始和最终 SHA-256 均为：

`c0fe65e69ca8efe4f98fedd2758d6cb0a043e840ccf6e0814ad6acb1f7b76e6f`

| 项目 | 最新实际结果 |
|---|---:|
| PCAP 总包 / 目标 UDP | 153 / 153 |
| 完整候选 / 结构合法 / 解析成功 | 153 / 153 / 153 |
| 解析失败 / 重复 / 冲突 | 0 / 0 / 0 |
| Accepted 科学帧 | 153 |
| 每通道原始采样点 | 19125 |
| CSV 数据行 / 总行数 | 19125 / 19126 |
| CSV 文件大小 | 1134157 B |
| LPD 文件大小 | 154562 B（32 + 153 × 1010） |
| EOF 文件关闭 | PASS |

非 Mock 专项 E8 对 CSV、LPD、双格式分别实际写盘并读回。E7 原 LPD 专项亦使用最新文件重新通过。输入没有设备时间倒退记录，因此真实倒退附近抽查为“不适用”；另有确定性回归验证倒退帧保持顺序、时间值和全部采样点，不删除 Accepted 帧。

用户此前提供的 `20261019--朗缪尔.pcapng`（293112 B）也实际测试通过：796 总包、795 目标 UDP、90 Accepted，CSV 11250 数据行、651587 B，其余 705 个科学候选不满足固定结构。此结果仅为补充，最终验收使用最新 `102301.pcapng`。初始请求中的 `2026101902--朗缪尔.pcapng` / 1053 帧文件不在当前输入中，未将其旧数值当作本轮结果。

## 6. 双通道逐点独立读回

E8 以独立的固定字节、整数时间、差分 ADC 和 5 秒/256 帧 FIFO 参考，验证所有 Accepted 记录；CSV 再用独立行解析逐项检查全部 8 个整数列和 integrity。另用 Python 标准库直接解析最新 PCAPNG 的块、以太网、IPv4、UDP 和科学帧字节，不调用 Rust 解析器，重建两通道并检查实际 GUI CSV 的 **38250 个 ADC 数值**。本专项适用该实际抓包的完整单 UDP 帧；生产重组仍由原有测试覆盖。

| 帧 / 样本 | sequence_u16 | 原秒 / 微秒 | sample_time_us | ch1_adc / ch2_adc |
|---|---:|---|---:|---|
| 首帧 0 / 0 | 489 | 148 / 899965 | 148899965 | 48808 / 48820 |
| 首帧 0 / 124 | 489 | 148 / 899965 | 148949565 | 47779 / 47418 |
| 固定伪随机中间帧 56 / 0 | 545 | 151 / 699965 | 151699965 | 48871 / 48873 |
| 末帧 152 / 124 | 641 | 156 / 499966 | 156549566 | 49357 / 49135 |

每个字段和全部采样均比较，上表仅列示例。原生 GUI 完整输出：`data/scientific-20261023-172854.356.csv`，SHA-256：

`6d20a6e074941b6ca55cd5636c7bcb735acef986627fc9915b4c213ed9d5692a`

[独立读回 JSON](qa/LVDS_SCIENTIFIC_CSV_INDEPENDENT_READBACK.json) 包含完整文件信息、计数及首/中/末帧记录。

## 7. 异常、停止和长期写入

| 验证 | 结果 / 证据 |
|---|---|
| 单帧 125 行 / 100 帧 12500 行 | CSV sink 单元测试 PASS |
| 通道未交换、跨秒、最大秒数、u16 回绕 | 逐点/整数断言 PASS |
| 错帧头、地址、长度、非法时间、ADC 越界不写入 | CSV ingress 集成测试 PASS |
| 重复完整帧只存一次 | 原 gate + CSV 集成测试 PASS |
| 设备时间倒退保持原 Accepted 顺序 | CSV sink 和 ingress 集成测试 PASS |
| 用户中途停止，CSV 可读且回放/绘图继续 | 确定性真实 writer 回放测试 PASS |
| EOF 排空并关闭 | 最新 PCAP 非 Mock专项及 Windows GUI PASS |
| 同名文件不覆盖、非法目标路径不虚报成功 | CSV 创建失败测试 PASS；双格式第二文件创建失败后的回滚经代码审查，未单独注入该创建分支 |
| write / flush / close 错误真实报告 | 真实编码器/worker 的外部 I/O 故障注入 PASS |
| 权限不足 / 磁盘满 / 其他 I/O 错误 | 注入 PermissionDenied / StorageFull / Other，原错误类型保留、计数不虚增、禁止重试，PASS |
| 双格式其中一份失败 | write/flush/close 注入；另一文件保留，计数与关闭状态正确，PASS |
| 关闭超时后最终状态仍更新 | 共享进度回归 PASS，错误和不完整状态保留 |
| 图表拥塞/抽稀与科学保存解耦 | 显示队列容量 1 的集成测试，显示拥塞仍保存全部 Accepted 原始帧，PASS |
| CRC 标记 | 全文件逐行确认 CRC_NOT_CHECKED |
| 长期写入 | 10000 帧 / 1250000 行，4 KiB BufWriter、每批 100 帧；每批关闭前观察文件增长，流式读回 1250001 行，PASS |

长期测试逐批仅持有当前批次，CSV 格式化仅持有一帧，不在内存累计全捕获行数；未将此测试描述为真实设备长时性能验收。没有实际填满磁盘或更改用户目录 ACL，相关 OS 错误通过 I/O 边界注入验证。

原生手动停止补充操作未获得有效中途停止样本：最新抓包约 7.6 秒已先到 EOF，随后点击原“开始保存”真实触发“请先连接 LVDS”；这不计为手动停止通过，也不是 CSV 写入错误。有效中途停止覆盖来自上述确定性真实 writer 测试。

## 8. UI 和 Windows 原生截图

保留固定三栏及已有中央区域尺寸。普通布局将格式下拉框放入原保存行，紧凑布局放入原“更多”菜单；没有新增整体滚动条或浮动主功能面板。保存、回放选项的高度沿用现有 32pt 控件基线；完整路径可查看，CSV 帧数和行数均来自文件输出状态。

| 原生截图 | 验证内容 |
|---|---|
| [三种格式](qa/LVDS_SCIENTIFIC_CSV_FORMATS_WINDOWS.png) | LPD 默认和 CSV / LPD + CSV 可选 |
| [回放前保存设置](qa/LVDS_SCIENTIFIC_CSV_REPLAY_SETUP_WINDOWS.png) | 最新路径、CSV 和首帧前启用保存 |
| [保存过程中](qa/LVDS_SCIENTIFIC_CSV_SAVING_WINDOWS.png) | 波形实时刷新，入队和实际写入分开，活动格式锁定 |
| [完整回放关闭](qa/LVDS_SCIENTIFIC_CSV_COMPLETE_WINDOWS.png) | 153 帧、19125 行、已关闭，双通道波形 |
| [详细状态](qa/LVDS_SCIENTIFIC_CSV_DETAILS_WINDOWS.png) | 文件路径、真实写入帧/行、关闭和 RS422 CSV 区分 |
| [125% 紧凑更多菜单](qa/LVDS_SCIENTIFIC_CSV_COMPACT_125_WINDOWS.png) | 格式和完整状态仍可访问，无新整体滚动条 |

截图为本轮新 Windows x64 非 Mock EXE 的实际原生窗口，1360×860 客户区，含 125% 应用缩放。紧凑截图中的“请先连接 LVDS”是前述 EOF 后再次点击保存的真实输入错误，未掩盖或将其计作正常保存证据。原有 1280/125%、1360、1600 布局矩阵、统计稳定和 60 秒数据布局测试亦通过。

## 9. 回归与构建

| 命令 / 检查 | 结果 |
|---|---|
| cargo fmt --all -- --check | PASS |
| cargo check --workspace --locked | PASS |
| cargo test --workspace --features langmuir-desktop/mock --locked | **265 passed、0 failed、2 ignored** |
| 非 Mock ignored E7 + E8，指定最新真实 PCAP 和 GUI CSV | **2 passed、0 failed**（已单独运行，非跳过验收） |
| cargo build --release -p langmuir-desktop --locked | PASS，真实后端 |
| WINDOWS_BUILD.md 的 MSVC x64 / static CRT / --no-default-features / --locked | PASS，未启用 Mock/HDF5 |
| Windows 原生启动、格式选择、最新 PCAP、波形、EOF/文件读回 | PASS |
| cargo clippy --workspace --all-targets --locked -- -D warnings | **FAIL：既有 lint** |

workspace 测试分项：core 89、desktop 128、CLI 5、storage 18、transport 25。两个 ignored 是依赖外部用户 PCAP 的 E7/E8，已通过环境变量另行实际运行。

严格 workspace Clippy 的阻断为既有 `framing.rs:288,290` 的 assert_is_empty、CLI `main.rs:325` 的 chunks_exact_to_as_chunks、`binary.rs:143,146` 的 explicit_iter_loop。为检查 desktop 新代码另跑同样 `-D warnings` 的 `--no-deps --keep-going`，仍 FAIL 于既有 app/backend/buffer/fonts 的函数长度、分号、断言等；本轮新增源文件未有剩余诊断。新增过程中发现的格式追加、clone、文档和测试长度问题已修复。未添加 lint 豁免，也未将分离诊断冒充严格通过。

完整日志在 `target/qa/scientific-csv/`：`fmt.log`、`check.log`、`workspace-tests.log`、`latest-pcap-all-modes.log`、`latest-pcap-gui-readback.log`、`clippy-strict-final.log`、`clippy-strict-final-keep-going.log`、`clippy-desktop-final.log`、`release-default.log`、`windows-static-release.log`。最终带 `--keep-going` 的严格 workspace Clippy 复核仍仅在上述未修改的 core/CLI/storage 旧代码处阻断。

复现外部专项（PowerShell，仓库根目录）：

```powershell
$env:LANGMUIR_USER_PCAP = 'D:\RUST\langmuier\102301.pcapng'
$env:LANGMUIR_GUI_SCIENCE_CSV = 'D:\RUST\langmuier\data\scientific-20261023-172854.356.csv'
cargo test -p langmuir-desktop --locked -- --ignored --nocapture
```

独立 Python 字节/CSV 比对脚本保存在本机 `target/qa/scientific-csv/independent_readback.py`。原始 PCAP 未修改，未打包进发布 ZIP。

## 10. 交付与变更清单

正式程序：`target/delivery/windows-x64/LangmuirGroundControl.exe`；发布包：`target/delivery/LangmuirGroundControl-windows-x64.zip`；README 为本轮更新的 Windows 构建和使用说明。交付 EXE 来自本轮 static CRT MSVC x64 构建，发布包检查见 `target/qa/scientific-csv/windows-package-verification.json`。

| 交付校验 | 结果 |
|---|---|
| EXE 大小 / SHA-256 | 9265152 B / `a1fac4c0a27adb36ab7f98de9a37bbfa526c0064289f43f6a8620371ad758624` |
| ZIP 大小 / SHA-256 | 3896791 B / `821cece97f3128e775ff653c891585ff88a6beb4f44a0c2918d3c4feb7a720da` |
| PE 架构 | `0x8664`、PE32+ Windows x64 |
| ZIP 内容和 CRC | 仅 EXE 与 README；CRC 无误，EXE 与新构建逐字节相同，README 与更新文档逐字节相同 |
| 动态 MSVC CRT 依赖 | 无 vcruntime / msvcp / ucrtbase / api-ms-win-crt 导入 |

修改：`app.rs`、`app_layout_tests.rs`、`backend.rs`、`backend_e2e_tests.rs`、`backend_replay_tests.rs`、`bridge.rs`、`mock.rs`、storage `lib.rs`、`docs/WINDOWS_BUILD.md`。

新增：`scientific_csv.rs`、`backend_scientific_storage.rs`、`backend_scientific_csv_tests.rs`、本报告、实施计划、上述 6 张截图和独立读回 JSON。用户提供的两个 PCAP 保持原状/未跟踪；生成的测试 CSV 归档于 target，GUI CSV 位于忽略的根 `data/`。未改动 RS422、核心 LVDS/UDP、ADC、TTL、去重、序号监测、LPD v2、HDF5 schema 或 Sidecar。

后续 GitHub 更新范围为上述源码、测试、文档及验收截图，目标为 `YuancongLing/langmuier` 的 `main` 分支。原始 PCAP、本地运行 CSV/LPD 和 `target` 下的构建产物不进入 Git 提交；Windows ZIP 继续使用本机交付路径。

## 11. 未完成的真实硬件验收

真实 UDP 设备/串口联调、真实硬件长时间吞吐与磁盘满场景、HDF5 原生 feature 运行：**NOT_RUN**。没有自动连接真实硬件；localhost 和确定性测试不冒充设备验收。本阶段到此结束，不自动开始其他开发任务。
