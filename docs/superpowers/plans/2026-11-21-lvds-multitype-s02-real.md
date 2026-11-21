# S02-REAL 真实 PCAP 验收计划

> **For agentic workers:** 按下列任务并行执行独立审计、生产后端验证和最小 UI 修复；由主代理整合复核。用户已授权实施，不额外等待计划审批，不提交 Git。

**Goal:** 用只读真实抓包验证 AA/AB/AC 分流与全量文件保存，交付独立 EXE 和可追溯报告。

**Architecture:** Python 标准库独立读取原始字节生成期望；Rust 测试调用现有生产入口并完整按原节奏回放，预先建立三类 writer。独立读回所有输出；UI 仅作可发现性修复，不重构解析或存储。

**Tech Stack:** Rust、egui、PCAPNG、Python 标准库、Windows MSVC 静态 CRT。

**Spec:** `C:/Users/Administrator/.codex/attachments/cb85e04b-fc6b-4b22-8fd5-b78b61e05347/Pasted text.txt`

## Global Constraints

- 原始输入 `C:/Users/Administrator/Downloads/2026112101.pcapng`，只读，验收前后 SHA256 对照。
- 保留所有已有未提交改动、原有输出与用户运行进程；不自动连接设备或发送控制。
- 单独 EXE，无 ZIP；不进入 S03；不提交 Git。
- 原生 GUI 无工具时明确 `WINDOWS_GUI_NOT_RUN`，不把 egui 自动化或 CLI 冒充原生验收。

## Review Focus

- 1250 FPGA 与 1262 全 UDP 的原始归档边界：逐条核对全部 UDP 及 END/EOF。
- 首帧与 EOF 丢失：预建 writer，最终计数及独立全量文件比对。
- AB 与后续 AA 序号关系：保留原值并输出 57 行关联表。
- 近零 AA 电压和 AB 跨秒倒推：真实值与时间逐点比对，不伪造可见波动。
- UI 版本混淆与入口隐藏：显式版本、AA/AB/历史及 CSV 操作入口，自动化验证与原生验收分别记录。

## Task 1: 独立参考与读回

**Files:** 新建 `tools/qa/lvds_s02_real_reference.py`，输出 `docs/qa/lvds-multitype-s02-real/`。
**Interfaces:** 输入原始 PCAP、生产输出目录；输出 JSON 审计、AB 关联 CSV、逐字段验证 JSON。

- [x] 读取 PCAP、协议工作簿与现有已验证参考读取器；独立计算公共结构、时间节奏、AA/AB/AC 字段。
- [x] 输出审计基线后交由生产测试消费，不调用 Rust 解码器生成期望。
- [x] 对实际 RAW/AA CSV/LPD/AB CSV 全记录独立核对，记录错误和输出身份。

## Task 2: 生产全链路与历史

**Files:** 新建 `apps/desktop/src/backend_s02_real_tests.rs`，仅在 `backend.rs` 添加测试模块声明。
**Interfaces:** 使用现有 Controller/PcapReplay/RawRecording/HistorySession；输出 `target/qa/lvds-multitype-s02-real/` 的文件与生产摘要。

- [x] 编写独立 ignored 真实输入专项，显式调用前需输入存在，按原节奏回放且在首帧前开启三类 writer。
- [x] 验证生产解码字段、全部记录数、writer 完整关闭、零丢失，以及 RawReader 读回。
- [x] 验证 PCAP/LVDSRAW/LPD 历史与 AB 前后帧选择，并区分自动化与原生 GUI。

## Task 3: 最小 UI 可发现性

**Files:** `app.rs`、`app_excitation.rs`、`main.rs`、相关 UI 测试。

- [x] 先以失败测试确认版本与入口预期；修复为显式 AA/AB 视图和常见历史/保存操作，标明频率 Hz。
- [x] 用真实数据验证图表单位、100 点及时间范围，运行相关布局/输入测试。
- [x] 检查原生工具可用性；能用则操作与截图，否则准确记录未执行及原因。

## Task 4: 整合、回归与交付

**Files:** `docs/LVDS_MULTITYPE_S02_REAL_PCAP_ACCEPTANCE.md`、QA 日志、`target/delivery/lvds-multitype-s02-real-20261121/LangmuierProbeGroundControl.exe`。

- [x] 审查实际差异及全部证据，运行 fmt/check/workspace tests/clippy；真实专项不重复累计。
- [x] 源码改变则重建非 Mock MSVC x64 静态 CRT Release，核对 PE 与哈希并执行 EXE 审计。
- [x] 再核对原始 PCAP 哈希、原有进程和 Git 差异，报告逐项 PASS/NOT_RUN/BLOCKED 与下一步限制。
