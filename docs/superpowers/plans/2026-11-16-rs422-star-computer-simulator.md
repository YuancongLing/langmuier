# RS422 Star Computer Simulator Implementation Plan

> **For agentic workers:** 20260728 反馈版原件已补齐并完整阅读；按任务先添加失败测试，再实现并验证。字段依据见 `docs/qa/star-simulator/protocol-20260728-audit.md`。

**Goal:** 在现有真实 RS422 单 Writer 中增加用户主动启动的时间、姿态轨道和多点 GNSS 合成广播。

**Architecture:** core 层提供纯字节编码与 WGS-84 转换；desktop Controller 统一调度遥测和广播；现有 transport Worker 独占串口并对广播提供有界低优先排队与取消。UI 在现有控制区内增加页签或卡片，不增加并行串口控制器。

**Tech Stack:** Rust 2024、crossbeam-channel、serialport、egui/eframe、独立 Python 标准库参考。

**Spec:** 用户需求附件；根目录原始 RS422 `.doc`。输入差异及验收状态见 `docs/RS422_STAR_COMPUTER_SIMULATOR_QA.md`。

## Global Constraints

- 必须先完整阅读包含三类广播逐字段表的原协议，不能以提示词摘要替代。
- 广播默认关闭；启动、连接和发送均由用户明确操作；重连不自动恢复广播。
- 同一 COM 口仅现有 Worker 写入；有界队列，不无限积压或重试。
- 不修改 LVDS/UDP、55/AA 科学帧、CSV/LPD/LVDSRAW 格式及 CRC 不校验策略。
- 合成参数及点位明确标识 SYNTHETIC；不声称真实姿态、轨道或 GNSS 测量。
- 最终 Windows x64 静态 CRT Release 不启用 Mock，必须使用本轮最新源码重新构建。
- 不自动提交 Git，不开发其他新功能。

## Review Focus

1. 停止请求与 Writer 并发：正在发送的整帧完成，取消尚未开始的周期广播，不能拆半帧。
2. 写入失败或断开：停止全部调度，可靠状态兜底，重连不恢复广播。
3. 拥塞：控制/遥测保留容量，广播跳过过期时刻，不追赶堆积。
4. 时间进位、闰秒、u32 秒数、定点坐标及 24bit 补码边界：显式处理与独立验证。
5. 窄窗口、高缩放及状态事件丢失：操作可访问，状态可靠同步，中央图表尺寸保持。

## Task 1: Complete source protocol audit

**Files:** `docs/qa/star-simulator/protocol-source.txt`、`protocol-audit.md`、主 QA 报告。

- [x] 执行 Git 初始状态检查，完整提取并阅读现有原协议，核对版本、实际表号和第 3–5 节。
- [x] 审计既有 Writer、发送队列、遥测、控制及写入确认。
- [x] 获取含时间、姿态轨道及 GNSS 字段表的正确原文件，全文阅读后记录 SHA-256。
- [x] 固定 GNSS W1–W114 字段/位映射和未定义测试默认值、时间纪元及闰秒策略。未定义运行/本地时间填合成0；W10有矛盾位采用默认0；2018起无新增闰秒，W97独立GPS偏移18，不向累计秒再加18。
- [x] 固定共享接口：core `BroadcastKind::{Time,Attitude,Gnss}`、`StarTime::from_system_time(SystemTime)`、`encode_broadcast(kind, time, point: GnssPoint, sequence: u16) -> Result<Vec<u8>, BroadcastError>`；desktop `StarSimulatorConfig`、`StarSimulatorStatus` 位于 `apps/desktop/src/star_simulator.rs`；bridge 提供 Configure/Start/Stop/SendStarBroadcast 请求和状态事件。

## Task 2: Pure broadcast encoding and independent golden fixtures

**Files:** 新建 `crates/langmuir-core/src/star_broadcast.rs` 与对应测试；修改 `crates/langmuir-core/src/lib.rs`；新增 `tools/qa/rs422_star_reference.py` 和固定测试向量。

- [x] 依据核对后的原表，独立生成三类完整参考帧，测试错误 endian、偏移、量化及两层校验都能检出。
- [x] 先验证 Rust 黄金测试失败，再实现字段级显式大端编码，禁止直接序列化结构体内存。
- [x] 覆盖 14/67/122B 长度、时间进位和固定 UTC、24bit 有符号角度、float 大端、六磁场通道、14bit 序号回绕。
- [x] 同一大地坐标计算 ECEF，覆盖三个合成点、负坐标、量化误差、非有限值和溢出；完整 GNSS 字段与独立参考逐字节比较。
- [x] 运行 core 定向测试确认全部通过。

## Task 3: Single Writer scheduling and controlled transport integration

**Files:** 修改 `crates/langmuir-transport/src/worker.rs`、`apps/desktop/src/backend.rs`、`bridge.rs`；新建 `backend_star_simulator.rs` 及测试。

- [x] 添加失败测试：停后取消排队周期帧、控制优先且不丢失、每帧完整连续、断线停止/重连不恢复、点位切换只影响后续帧。
- [x] 为同一个 Writer 加入低优先有界广播任务及发送前取消检查；每类最多一个待发任务。
- [x] Controller 统一 tick，三周期独立、默认1000ms、检查100–60000ms范围、错过周期跳过不追赶；物理写确认后更新计数，记录实际写入时间与延迟。
- [x] 遥测默认改5s；广播期间保证轮询小于20s，不能因广播无应答掉线，不实现虚构的电源重启。
- [x] 受控 Transport 捕获完整实际 write 字节和时刻，三类各1s同时启用、遥测和控制并行，独立检查帧边界、校验、时序与停止；输入标记测试来源。
- [x] 运行 transport、desktop 定向测试确认全部通过。

## Task 4: Existing control area UI

**Files:** 修改 `apps/desktop/src/app.rs`、`mock.rs`（桥接口兼容）及相关 UI 测试；新建 `app_star_simulator.rs`。

- [x] 添加失败测试：默认关闭、断线禁止启动、单次请求、切换点位、事件恢复及缩放布局。
- [x] 增加三开关/周期、单次发送、启动/停止、点位选择、位置与HEX预览、UTC时间基准、计数、最近写入、错误及队列状态。
- [x] 标明真实串口与测试来源，预设点明确 SYNTHETIC；合成姿态默认值说明。
- [x] 运行 UI 测试，并使用本轮实际原生窗口操作截图验证不同尺寸/缩放，更新 QA。

## Task 5: Full verification and new Windows release

**Files:** 主 QA 报告、`docs/WINDOWS_README.md`、`target/delivery/` 新构建产物。

- [x] `cargo fmt --all -- --check`。
- [x] `cargo check --workspace --locked`。
- [x] `cargo test --workspace --features langmuir-desktop/mock --locked`。
- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`；如有阻断如实记录诊断，不把部分检查标成全部通过。
- [x] 依 `docs/WINDOWS_BUILD.md` 从最新源码重新构建静态 CRT、非 Mock、x64 Release；验证 PE 架构、CRT 依赖和 ZIP 内容。
- [x] QA 填入完整黄金字节、字段表、两层校验、实际字节捕获、周期误差、截图、回归、EXE/ZIP SHA-256、Git 修改清单和未完成真实硬件项。
- [x] 保持未提交状态，完成本阶段后停止。
