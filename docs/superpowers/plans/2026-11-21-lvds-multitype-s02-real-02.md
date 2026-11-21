# S02-REAL-02 真实多模式验收与局部修复计划

> **For agentic workers:** 按本计划并行实施独立参考、核心/存储修复与 UI 修复；主代理负责生产整合与最终验证。用户已授权必要修复，不提交 Git。

**Goal:** 以真实 202611212141.pcapng 完成全模式软件和文件验收，保留175个AB含82个无采样时间状态帧，交付新单EXE。

**Architecture:** 保持生产公共分流、解码与保存架构。独立Python按原始字节建立多模式基线；核心明确无采样时间语义，CSV追加必要原始字段/状态且保留原列顺序；UI对无采样时间帧显示状态而不画假波形。

**Tech Stack:** Rust、egui、Python标准库、Windows MSVC静态CRT。

**Spec:** `C:/Users/Administrator/.codex/attachments/353ffc45-7c06-4bf6-888d-2afb46e79778/Pasted text.txt`

## 全局约束

- 只读原PCAP（工作区与Downloads两份均核对身份），保留旧输出、EXE、报告、进程、全部用户改动。
- 不改AA/UDP/LVDS/LPD/RAW/RS422协议，不增加生产CRC门禁，不进入S03，不连接硬件、不提交。
- 输出只写新的 `target/qa/lvds-multitype-s02-real-02/` 与 `docs/qa/lvds-multitype-s02-real-02/`。
- 原生GUI工具当前未提供；重新确认后如仍不可用明确NOT_RUN，自动化egui不充当原生截图。

## Review Focus

- PCAP if_tsresol=9正确解释纳秒；全UDP3873与FPGA3839边界分别核对。
- interval_us=0合法状态不可拒收，也不可计算100个同时间点；100原始槽位必须保存。
- CSV旧22列的名称/顺序/有时间帧含义保持；追加interval_us、points和明确状态，读取兼容说明必须清楚。
- 175帧模式切换及PA0回绕；DC设定值不用于绘制假实测线。
- 抓包超过60秒：历史按窗口覆盖全AA并逐175AB定位，不误将当前显示窗口当全文件保存。

## 任务

### 1. 独立字节审计和文件读回

**Files:** 新建 `tools/qa/lvds_s02_real02_reference.py`，新QA manifest/JSON/文本/阶段表/关联CSV。

- [x] 独立重算容器/纳秒分辨率、全部端点、外层/类型、AA/AB/AC、参考CRC及11个阶段。
- [x] 输出完整manifest，AB无时间槽位用null，保留全部原始数据；不修改上一轮独立证据。
- [x] 对生产RAW/AA CSV/LPD/AB CSV和PCAP/RAW历史导出全量独立比对。

### 2. 核心/存储最小语义修复

**Files:** `crates/langmuir-core/src/excitation.rs`、`crates/langmuir-storage/src/excitation_csv.rs` 及对应测试/独立黄金CSV。

- [x] 用原始状态帧写失败回归，证明旧sample_time_us和CSV制造重复时间。
- [x] 保留合法解码；interval_us=0时sample_time_us返回None，只有正间隔才进行窗口下溢拒收。
- [x] CSV v2保留前22列并追加interval_us、points、waveform_status；NO_SAMPLE_TIME时sample_time_us空，wave_mV与wave_V保留原始数值/单位换算，状态明确其非实测波形。
- [x] 跑核心/存储及有关后端测试，修复前后证据写入新QA目录。

### 3. UI与多模式真实图表

**Files:** `app_excitation.rs`、新 `app_s02_real02_tests.rs`、`app_excitation_tests.rs`、`main.rs` 与测试模块声明。

- [x] 真实零状态复现失败，显示“当前无有效激励波形/无可用逐点采样时间”，不生成线/游标伪点。
- [x] 保留模式/状态/PA0/DC设定，DC显示1.000/3.000/4.500/0.000V设定值；版本改S02-REAL-02。
- [x] 用生产历史进入egui核对175帧和11阶段，93帧9300点有效时间、电压，82帧空曲线及状态；覆盖状态切换、冻结、前后/指定帧。

### 4. 生产全链路与回归交付（主代理）

**Files:** 新 `backend_s02_real02_tests.rs`（可复用已有专项测试辅助但不覆盖旧证据）、新报告 `docs/LVDS_MULTITYPE_S02_REAL_202611212141_ACCEPTANCE.md`、新delivery目录。

- [x] 首帧前预建所有writer，实际生产回放完整输入；RawReader与历史PCAP/RAW/LPD回开，AB历史导出与直接保存独立比对。
- [x] 至少一次正常捕获节奏（约174.392秒）验证，记录后台保存与显示分离、进程身份/性能；保留EOF与零丢失证据。
- [x] 最终fmt/check/workspace tests/clippy，旧真实输入专项和新真实专项分别计数。
- [x] 重新构建Windows x64 MSVC静态CRT非MockRelease，核对PE/哈希/EXE只读审计，原件与旧进程保护复核。
- [x] 报告逐项PASS/FAIL/BLOCKED/NOT_RUN，附阶段/关联表、CSV版本兼容及真实硬件/原生GUI限制。

执行收尾：软件与文件证据已完成；原生GUI工具不可用，明确记录NOT_RUN；真实硬件NOT_RUN，未进入S03。最终证据见本轮验收报告和verification-summary.json。
