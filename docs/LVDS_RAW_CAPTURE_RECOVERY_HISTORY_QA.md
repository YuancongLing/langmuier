# 原始 UDP 保存、受控位恢复与历史查看验收

日期：2026-10-25。状态：**本阶段软件验收 PASS**。跨目标隔离、recovery ON 组合门禁、同时间 Native 替换恢复帧的曲线保留，以及历史外层错误/重组错误异常与审计均已修复。冻结源码五条命令全部退出 0，工作区 **351 通过、7 显式忽略、0 失败**；静态 CRT 非 Mock acceptance 包通过字节/PE/ZIP 核验。同一新包完成三类历史文件原生 GUI、双通道交互、科学 CSV/LPD/质量 CSV/raw 独立读回及 60 秒 SYNTHETIC localhost 20Hz 原始归档。真机、长时及物理磁盘故障未测边界见第 16 节。此前 `-delivery` 与 `RAW_HISTORY_FINAL_*` 保留为验收前阶段；本次最终 GUI 证据使用 `RAW_HISTORY_ACCEPTANCE_*`。

## 1. 范围与当前结论

本阶段保持最终 `55/AA`、269B、两路各 100 个 code10、500μs 采样间隔，以及普通原生科学帧 `CRC_NOT_CHECKED` 的既定规则。新增独立原始归档、明确质量分级的可选位恢复和软件内历史查看。不改 RS422 协议，不自动连接硬件或发送控制指令，不改变 LPD v3/标准科学 CSV 布局。

| 验收项 | 状态 | 已有证据/边界 |
|---|---|---|
| 实际用户 PCAP 独立逐 UDP 审计 | PASS | 467 个 UDP，正常 PCAP EOF，无包解析诊断 |
| 实际位边界和分类 | PASS | 237 NATIVE、229 RECOVERY_CANDIDATE、1 REJECTED、0 已确认恢复 |
| 不拼接跨记录位、不补造 CRC/ADC | PASS | 独立参考实现及单元用例；实际 PCAP 无完整恢复候选 |
| `lvds.txt` 独立原生回归 | PASS | 20 帧、4000 个 code10；269B/55AA/seq8/时间步进正确 |
| 独立 Python 审计器测试 | PASS | 18 个测试通过，`py_compile` 通过 |
| `.lvdsraw` 生产格式规范 | PASS | 版本、端点语义、原始字节边界及完整关闭规则已明确，见正式格式文档 |
| 核心/存储自动化回归 | PASS | 最终工作区核心 99、存储 36 项通过 |
| 后端及 UI 状态自动化 | PASS | 最新工作区开启 Mock 的桌面 185 通过、6 忽略；包含 5 项新曲线/异常追溯用例，不与阶段套件累加 |
| SYNTHETIC Rust Release CPU 计时与边界 | PASS | 生产核心路径专项计时；不代表整机实时采集、I/O 或 GUI 性能 |
| 最终包 SYNTHETIC localhost 20Hz 原始录制压力 | PASS | 60.000894 秒、1200 UDP，收/入队/写均 1200、drop0，全字节及 END/EOF 独立读回；不是设备科学吞吐 |
| Rust 对实际 PCAP 全样本交叉验收 | PASS | 显式真实文件测试 2 项通过；467 UDP，93200 个可得 code10 与独立审计一致 |
| 实际 PCAP 写入 `.lvdsraw` 后全记录读回 | PASS | 467 条、136830B Payload、所有端点/时间/原包索引/字节一致，完整 END/EOF |
| 最终包 Windows GUI 打开三类历史文件及操作 | PASS | PCAP/raw/LPD 原生选择及双通道；叠加/分屏、缩放/平移/冻结、播放/暂停/seek/EOF/退出均有新包实测 |
| GUI 科学 CSV/LPD 及质量 CSV 独立读回 | PASS | 原生 237 帧/23700 行；质量导出 46600 行，237 NATIVE+229 CANDIDATE，无错误升级 |
| 最终完整 fmt/check/全工作区测试 | PASS | 最后两类修复后各命令退出码 0；工作区 351 通过、7 显式忽略、0 失败 |
| 最终要求的 Release 构建/全工作区严格 Clippy | PASS | 修复后两条原样命令退出 0，见第 15 节 |
| 最新 Windows x64 非 Mock 静态 CRT EXE/ZIP | PASS | 最后两类修复后构建退出 0，包字节/PE/静态 CRT 校验通过；最新哈希见第 15 节 |
| 跨目标端点锚点隔离及 recovery ON 组合门禁 | PASS | RED→GREEN、阶段非 Mock 桌面 173 项及最新工作区回归通过，详见第 7、13 节 |
| 同时间 Native 替换恢复帧的实时/历史曲线 | PASS | 原位采用 Native，不误清空此前/后续帧；质量筛选/冻结重建仍保留曲线，真实时钟回退仍可见 |
| 历史外层/重组错误异常与追溯 | PASS | raw/PCAP 的 3 项新用例通过；未知 UDP 序号保持 None，拒绝不消耗合法分片依赖 |
| 最新 EXE 的 GUI 与 20Hz 补验 | PASS | 新包 SHA 与进程记录对应，实际紧凑窗口 1362×632；最终输出/截图及压力证据见第 10、12–15 节 |
| 真机实时 UDP/RS422 | NOT_RUN | 本轮不自动连接硬件；PCAP 回放不等于真机接收验收 |

`0` 个已确认恢复是独立边界计算的结果，不是未达到先前约 465 帧估计的测试失败。不能将旧估计作为通过阈值，也不能借用后续报文或补造缺失位达到该数字。

## 2. 输入文件、来源和完整性

| 输入 | 实际路径 | 字节数 | SHA-256 |
|---|---|---:|---|
| 本轮实际抓包 | `D:\RUST\2026102401.pcapng` | 172292 | `1bfe2ff956ad4e372bb15c2da04910c3d6a469b85d772ccbf83e4da4e48cca07` |
| 用户 20 条 HEX 的本地转存 | `D:\RUST\langmuier\lvds.txt` | 10780 | `b5a2eee986fbe4f26da33da67bfeab17b8f9185f72ed0c6f81f232f3c2f15add` |

两项输入在本轮审计和参考测试后重新核对，哈希未改变。`lvds.txt` 来源为用户在会话中提供的 20 行 HEX，按 ASCII/LF、末尾保留 LF 转存；其哈希仅标识这份本地转存文件，不声称是未知原始磁盘文件的哈希。

工程中的 `lvds.pcapng`（7516B，SHA-256 `e082a83fd079df35484f42b891b59e294849b747874be7410aedc8fee5c42813`）是此前用这 20 条 HEX 加网络封装得到的派生文件，不是本轮新的实际抓包。它不能替代 `2026102401.pcapng` 的实际 UDP 验收。

## 3. 生产调用链与原始保存边界

执行前保留工作区未提交修改、输入和既有报告。受影响链路为：

```text
实际 UDP Socket / 用户主动打开的 PCAP
  → 原始观察：时间、来源、目标语义、原始 UDP Payload
  → 独立有界原始写入队列
  → 来源过滤、24B 外层解析、原有分片重组/TTL
  → 原生 269B 解码 / 有界字节同步 / 可选位恢复
  → 科学门禁与去重
  → 双通道显示、独立恢复审计、原生科学文件
```

`.lvdsraw` 的保存对象是应用实际获得的 UDP Payload。Live UDP 不具有真实 Ethernet/IP 头，不能把配置的 Socket binding 当作接收到的实际目的地址。PCAP 路径可保存抓包中明确存在的目标端点，并保留原包索引。在 PCAP 历史模式下，原始保存操作由后台完整扫描该用户文件进行导出，不把当前可见窗口或播放抽稀点作为原始数据来源。

归档发生在科学解析之前，结构错误、短帧、位错位、错误来源、重复报文及被拒绝的 Payload 都应独立保留。原始记录不按科学帧成功数筛选，不排序时间，不修改帧头、位序、尾部或 CRC。

这一边界只能报告应用收到后的保存情况；即使观察数等于写入数，也不能证明网卡、驱动或操作系统在此前没有丢包。生产实际调用链的最终运行验收仍随本轮集成结果更新。

## 4. `.lvdsraw` v1 与独立 Reader

正式布局见 [LVDSRAW_FORMAT.md](LVDSRAW_FORMAT.md)。主要边界：

| 区域 | 布局 | 作用 |
|---|---|---|
| 文件头 | 128B；`LVDSRAW\0`、v1、长度、字节序、来源、会话、配置端点 | 识别版本和采集来源 |
| DATA | 96B 固定头 + N 字节 UDP Payload + 4B 重复长度 | ID、精确时间、来源、目标语义、原包索引和实际 Payload |
| END | 92B，必须存在且后面恰好 EOF | 观察/入队/落盘/丢失/字节数、正常关闭或不完整状态 |

整数为小端；时间按秒与纳秒分开保存。DATA 记录占 `100+N` 字节，N 最大 65535；原始记录 ID 从 1 开始，在尝试入队前分配。队列丢失会留下 ID 间隙，并永久使完整标志为假。

Rust `RawReader` 与 Python 独立 Reader 分别实现校验。独立 Reader 检查版本、端点、保留位、时间范围、长度前后边界、ID、计数关系和精确 EOF。即使恰好在完整 DATA 之后结束，没有 END 也只是不完整文件。部分头、记录、Payload 或 END 为截断，不能当正常 EOF。

该格式没有内容校验和。边界和 END 能识别结构截断和已知应用丢失，不能保证任意文件内容损坏都能被检测；独立比对另用 SHA-256 和逐字节比较。

## 5. 异步录制、错误和关闭状态

`RawArchive` 使用独立 `BatchWriter`，默认有界队列 128、批次 32、250ms flush 和 64KiB 文件缓冲。最大 Payload 下，队列、活动批次和一条待提交 Payload 为 `(128+32+1)*65535=10551135` 字节，另有记录元数据和文件缓冲；不会无限堆积 Payload。

创建使用 `create_new`，不能覆盖已有文件。原始保存与 LPD/科学 CSV 有独立生命周期和状态。正常停止需要排空和明确 END；Drop、超时、写入/flush 错误、队列溢出均不能显示为完整保存。计数不依赖可能溢出的 GUI 事件队列。普通 flush 不等于断电持久性承诺。历史 PCAP 全文件导出在生产者积压达到 96 时等待后台写入推进；这只约束文件读取，不阻塞真实 UDP 接收回调，也不免除最终丢失/错误状态检查。

独立 Python 用例已验证正常/不完整 footer、记录 ID 间隙、计数矛盾、截断、错误 header/边界的识别。Rust 存储阶段 36 项测试通过，其中 raw 的 11 项覆盖独占创建、完整 Duration/端点/字节顺序、独立字节夹具、所有截断位置、无 END 的记录边界 EOF、版本/flag/长度/ID/计数错误、IPv6 scope、队列溢出、Drop/空文件明确关闭、非法元数据、注入 write/flush/close 故障，以及 flush 阻塞后迟到 close 不能抹除已有失败。

后端真实 localhost Socket 用例验证：错误来源、错误外层、短帧、重复 UDP 都在来源/解析/去重之前进入 raw hook。另有取消待写审计记录、畸形 PCAP 使 raw 与审计均不完整的回归。上述为自动化/故障注入 PASS；最终原生 GUI 的录制错误状态与真实磁盘故障实测仍 NOT_RUN。证据见第 13 节，不把测试夹具当实际硬件输入。

## 6. 正常协议与同步搜索

原生协议仍为：`EB 90 55 AA 01 07 AA`，总长 269B；设备秒和微秒为 BE u32；CH1/CH2 各 125B，分别恢复 100 个 code10；DATA250 为 seq8；DATA251 必须 0；尾部 267–268B 保留。采样时间 `sec*1000000+us+500*k`，`k=0..99`；重建 ADC 为 `(code10<<6)-32768`。

恢复器先使用现有完整科学解码，再在最多 4096B 的明确科学 Payload 内寻找唯一字节同步，最后才在用户开启时进行位搜索。寻找完整 56 位固定头，不能仅凭 `EB90` 接纳随机数据。候选还需通过时间、保留字节、通道解包和真实长度检查；多个合法候选拒绝自动选择。完整原生帧优先返回，避免重新同步产生重复交付。

普通原生帧继续不以 CRC 作为接收门禁。参考测试包含尾部 CRC 改变仍为 NATIVE 的回归，不能因新增恢复功能而重新启用原生 CRC 门禁。

## 7. 位对齐规则和确认条件

位搜索按 MSB-first 寻找固定头，仅处理相位 1、4、7；7 表示另一个方向的 1bit 相位。对于相位 s，实际重对齐字节等价于 `(raw[i]<<s)|(raw[i+1]>>(8-s))`，实际首位偏移由搜索结果给出，不固定猜测位移方向。

每个字节的所有位都必须在同一条实际科学 Payload 内存在。恢复器不改变原始缓冲，不跨 UDP 借位，不补 0、不补 CRC、不插值 ADC。只具有 267B 以上合法实际前缀的结果可以解出诊断样本，但不足 2152 个实际帧位就不能成为完整科学帧。

完整恢复确认需要唯一候选、269B 全部真实位、固定结构/时间/样本合法、完整参考 CRC 匹配，以及前一可信帧的外层 u32 +1、seq8 +1、设备时间 +50000μs 辅助关系。可信锚点 TTL 为 2 秒，重复观察不延长 TTL。调用方按来源、目标及目标语义、会话隔离可信锚点；`RawDestination::Captured(endpoint)` 与 `SocketBinding(endpoint)` 不混同，`Unknown` 目标不能确认恢复。

跨目标端点隔离已修复并通过回归。来源/目标流切换清空残留分片、恢复锚点、依赖及相关恢复流状态；不会清空 Native 去重缓存，以避免旧原生记录重新交付。同一外层序号在不同目标出现不同内容仍保守判冲突。Native 与已确认恢复分别使用 256 项/5 秒去重缓存，已确认恢复不再消耗后到合法 Native 进入普通存储的资格。重复、乱序/缺口、冲突、恰好 TTL 到期以及新 Native 锚点后的恢复均有 recovery ON 联合用例，见第 13 节。实际 PCAP 只有同一目标流，其 237/229/1 分类仍不是跨端点行为的测试替代。

核心模块只有显式输入和上下文，实际无跨记录拼位逻辑。可选恢复默认关闭；开启只使异常帧有机会进入诊断或明确标识的恢复链路，不使不完整候选进入普通 LPD v3。

## 8. 实际 PCAP 的逐帧分类

独立标准库 PCAPNG Reader 读取全部 467 个 EPB/UDP，来源均为 `192.168.1.128:1534`。24B 外层序号为 3584–4050，466 个相邻增量均为 1。科学 Payload 长度为 466 条 269B、1 条 268B，无包解析诊断，正常 PCAP EOF。

| 质量 | 数量 | 实际原因 |
|---|---:|---|
| NATIVE | 237 | 直接具有完整合法 269B 结构 |
| RECOVERED_1BIT | 0 | 没有完整、证据满足的 1bit 候选 |
| RECOVERED_4BIT | 0 | 没有完整、证据满足的 4bit 候选 |
| RECOVERY_CANDIDATE | 229 | 可解码实际前缀，但尾部真实位不足 |
| REJECTED | 1 | 268B 短帧同时 DATA251=2，结构非法 |

以下索引为独立 JSONL 的 UDP 0 起始索引；将其归档时完整文件 raw ID 对应索引+1：

| UDP 索引 | 外层序号 | 处置和位边界 |
|---|---|---|
| 97 | 3681 | 268B，bit0 固定头；缺 8 位，且 DATA251=2；REJECTED |
| 98–325 | 3682–3909 | 228 条相位 4；每条只有 2148/2152 位，缺 4 尾位；CANDIDATE |
| 326–328 | 3910–3912 | 恢复原生正常结构 |
| 329 | 3913 | 相位 1；只有 2151/2152 位，缺 1 尾位；CANDIDATE |
| 330 起 | 3914 起 | 再次为原生正常结构 |

相位 4 或 1 的输入虽有 269 字节，重对齐需要额外尾位；不能把“收到 269 字节”等同于“具有重对齐后的完整 269 字节”。相位 4 的归一化实际整字节只有 268B，另外有 4 个真实尾位；相位 1 有 268B 加 7 个真实尾位。JSONL 明确保留这些位数。

完整逐条证据：`target/qa/raw-history-20261025/2026102401-final-frames.jsonl`；统计：`2026102401-final-summary.json`。初始不带 `-final-` 的草稿把非法短帧列为候选，已由最终文件更正，不作为结论依据。当前 18 测试版本重新计算得到的全部逐条行与最终文件完全一致。

## 9. CRC 参考和实际尾位证据

参考为 CRC-CCITT-FALSE：poly `0x1021`、init `0xFFFF`、不反射、xorout=0，计算范围归一化帧 `[7..267]`。这是当前软件离线解释，厂家最终覆盖范围尚未确认；不作为硬件故障根因证明。

| 对象 | 完整 CRC 参考结果 | 辅助尾位证据 |
|---|---|---|
| 237 个 NATIVE | 离线 MATCH 237 | 生产仍为 CRC_NOT_CHECKED |
| 229 个候选 | NOT_EVALUATED 229 | 已收到的 CRC 前缀位均与计算前缀一致；完整尾位缺失 |
| 1 个非法短帧 | NOT_EVALUATED 1 | 其可得部分 CRC 前缀还存在不匹配 |

候选的前缀匹配不能提升为完整 CRC MATCH。审计明确记录可计算的覆盖数据、计算值、实际收到的 CRC 前缀长度/值以及缺失位；不伪造完整收到的 CRC。实际数据中完整恢复候选为 0，跨记录使用的位为 0。

## 10. 质量追溯和科学保存规则

质量分为 NATIVE、RECOVERED_1BIT、RECOVERED_4BIT、RECOVERY_CANDIDATE、REJECTED；CRC 参考状态单独记录。恢复审计 JSONL 的设计包括 raw ID、原始字节和哈希、来源/采集来源、外层完整序号、位偏移及方向、实际依赖记录、归一化实际字节/完整帧哈希、结构/时间/连续性/CRC 检查和具体原因。独立队列容量 128，采用 `create_new`；HEADER、RAW_INPUT、SCIENCE_ALIGNMENT 和 END 区分输入和对齐证据。有界异步审计队列溢出或写入失败必须单独显示，不宣称完整追溯。

LPD v3 继续 40B 头、409B/帧，保存原设备时间、seq8、两路各 100 个原始 code10。本轮默认只写 NATIVE；不把位恢复或不完整候选无标识混入 v3。普通 11 列科学 CSV 保持兼容，直接使用完整科学样本，不来自显示抽稀。

包含恢复或候选的 CSV 应使用单独版本标识与文件名，并含 `quality/raw_record_id/recovery_shift/recovery_validation/source`。原始归档和审计保留可重新计算的依据；不修改历史 CSV 数值，不覆盖原始文件。

自动化已通过混合 NATIVE/已确认恢复/NATIVE 的显示顺序与原生存储隔离、LPD 历史完整样本导出、CSV 不受显示背压影响、已有 11 列原生格式回归，以及可见范围在两个边缘按 500μs 样本裁剪。CSV 使用独立 `lvds-history-csv-export` worker 和有界结果/取消通道，暂停不等待整个导出结束；不能把“后台扫描”误写成在 GUI 线程同步导出。

最新 acceptance EXE 已实际保存以下科学输出，并使用独立真实 PCAP 参考逐字段读回，不生成替代科学数据：

| 最终 GUI 输出 | 结果 | 字节数 / SHA-256 |
|---|---|---|
| `data/scientific-20261025-023840.901.csv` | PASS：237 Native 帧、23700 行、11 列，260700 字段及 47400 个 code10 全核对 | 1636896B；`33ea097d21848a631618bb29b02b2de5685184ea47d818135234ad0da20ff97e` |
| `data/scientific-20261025-023840.901.lpd` | PASS：v3、237 帧、40+237×409B，47400 个 code10/原设备时间/seq8 一致、严格 EOF；同一新包已重新打开绘图 | 96973B；`befe6135c42c27f20bb492d5bb60e2ecf6f86a8bb965f19bbe3a4c6636714d10` |
| `data/history-20261025-024800-763-quality-v1.csv` | PASS：v1、16 列、466 帧/46600 行，745600 字段/93200 code10；237 Native+229 Candidate，0 错误升级 | 11479703B；`0b4875e432869bb224ea6a6054fe93df68b67d56ed07d5f4b597cbdec8104273` |

当前输出证据为 `gui-acceptance-lpd-csv-readback.json` 与 `gui-acceptance-quality-readback.json`。质量 CSV 与此前验收前质量 CSV 内容相同，哈希一致，但本行实际文件来自最后两类修复后的新 EXE。来源、逐样本时间、code10、重建 ADC、质量和验证字段均独立核对。

下列旧输出保留为验收前阶段证据，不能代替上表最终构建的实际保存：

| 实际 GUI 输出 | 结果 | 字节数 / SHA-256 |
|---|---|---|
| `data/history-20261025-020554-575.csv` | 验收前 EXE PASS：237 NATIVE 帧，23700 行、11 列，260700 字段、47400 code10 | 1636896B；`33ea097d21848a631618bb29b02b2de5685184ea47d818135234ad0da20ff97e` |
| `data/history-20261025-020625-741-quality-v1.csv` | 验收前 EXE PASS：v1、16 列、466 帧/46600 行，237 NATIVE+229 CANDIDATE；745600 字段、93200 code10 | 11479703B；`0b4875e432869bb224ea6a6054fe93df68b67d56ed07d5f4b597cbdec8104273` |
| `data/scientific-20261025-014333.796.csv` | 前阶段 GUI PASS：正式原生保存，23700 行，全部 11 列/47400 code10 与真实 PCAP 一致 | 1636896B；`33ea097d21848a631618bb29b02b2de5685184ea47d818135234ad0da20ff97e` |
| `data/scientific-20261025-014333.796.lpd` | 前阶段 GUI PASS：v3、237 帧，40+237×409B，47400 code10/原设备时间/seq8 一致、精确 EOF | 96973B；`befe6135c42c27f20bb492d5bb60e2ecf6f86a8bb965f19bbe3a4c6636714d10` |

质量 CSV 的 raw ID 映射为独立 0 起始 UDP 索引+1；source 关联实际 `D:\RUST\2026102401.pcapng`，逐行验证结构、唯一性、完整位数、外层/内层序号及设备时间检查字段。229 个候选始终保持 CANDIDATE，未写为已确认恢复或混入原生科学保存。code10、重建 ADC 和 500μs 时间均来自独立实际 PCAP 前缀。验收前 EXE 导出证据为 `gui-delivery-pcap-csv-readback.json`；更早正式保存证据为 `gui-final-lpd-csv-readback.json`，均位于本轮 QA 目录。两组文件分别说明实际执行的构建与操作范围。

## 11. `lvds.txt` 正常回归

当前独立审计器对本地转存的 20 条用户 HEX 逐帧校验通过：全部 269B/55AA、NATIVE、DATA251=0、两路各 100 点、code10 在 0–1023；seq8 为 0–19，帧设备时间步长均 50000μs，帧内步长 500μs，总计 4000 个 code10。

首样本设备时间 `1700000000000000μs`，末样本 `1700000000999500μs`。离线参考 CRC 为 20 MATCH，普通原生接纳不使用该结果作为门禁。证据：`target/qa/raw-history-20261025/lvds-txt-current-native-regression.json`。

这里的 PASS 是独立解码回归，不自动等于当前 Rust Release 或 GUI 已通过；实际 Rust 对照和新构建实测另行列出。

## 12. 历史查看和 Windows 操作验收

三个入口复用已有双通道图表：`.lpd` 使用 `BinaryV3Reader`；`.lvdsraw` 使用 `RawReader`；`.pcapng` 使用现有 `PcapNgReader`，后两者进入共享 UDP/重组/科学同步/可选恢复链路。历史模式不得发真实 RS422 指令。

后台历史读取使用各 64 项的有界事件/请求队列和最多 4096 个稀疏元数据索引项，索引随增长降采样；60 秒可见窗口每通道最多 120100 点，异常列表最多 128 项。长原始文件不全量收集样本到 GUI。该索引上限只证明元数据有界，当前并没有用它实现 seek 加速。原始/PCAP seek 仍在后台从头重扫以正确恢复重组/去重/位恢复上下文，耗时随文件长度增长；循环每条记录检查取消与 selection epoch，新 seek 可撤销旧窗口发布，不能声称随机读取加速或常数时间定位。

历史 raw/PCAP 审计目录已改为 `%LOCALAPPDATA%\Langmuier\audits`，环境变量缺失时使用系统临时目录下同名子目录；不再要求输入目录可写。实际目录创建/写入失败仍会单独报告，Windows 只读输入目录操作尚待实测。

时间导航仅由可信 NATIVE/已确认恢复帧驱动，未验证候选的异常时钟不会清空/老化原生缓存或改写导航范围。真实原生设备时间回退时保留文件到达顺序并显示错误，主导航限制在首个连续时间段，不将后续重启时钟强行接到首段；当前没有跨时钟 epoch 的单独导航界面。

收尾新增曲线回归：同一采样跨度先收到已确认恢复帧、再收到合法 Native 时，实时缓存原位采用 Native，保留前后科学帧；不会因重新追加同时间样本而误当时钟回退。历史重建对重叠时间点采用 Native，并保留真实文件时钟回退顺序；筛选、冻结及重建不丢此前曲线。此显示选择不改科学存储资格，普通 LPD/CSV 仍只接纳 Native。

外层解析错误、短于 24B、实际/声明长度不符、超过 4096B 搜索上限及重组冲突现在进入历史拒绝统计和有界异常列表，同时写出 `UDP_REJECTED` 审计行，保留原始字节/哈希、来源、目标、raw ID、可知外层序号及具体原因。无法读取的外层序号为 None，不伪造 0。坏外层记录不会删除同序号正在重组的合法依赖；回归验证其后正确完成帧的依赖仍为真实参与的记录 ID。

不完整 END、缺 END 或记录尾部损坏可以保留此前有效前缀供历史窗口诊断，并显示源不完整；不是成功完整文件。导出扫描遇到该警告必须失败，可能留下明确未完成的输出，不能报导出成功。后台逐记录取消、CSV 独立 worker、首段导航和前缀可读行为已有自动化证据，Windows 实际操作另列如下。

最终 GUI 使用交付 EXE `LangmuierProbeGroundControl.exe`，SHA-256 为 `e8b923dd9818ebc294cc8fdc8bea48235961fd055eb88965dd1c9169fdc6fd1f`。`gui-acceptance-process.json` 记录进程 PID 44564、启动时间 02:36:41 与该包哈希。测试后停止录制和监听，将界面地址恢复为原 `192.168.1.100`，正常关闭软件并确认进程退出。未使用 Mock、未自动连接硬件或发送 RS422 指令。

| 最终 acceptance EXE 的 Windows GUI 操作 | 状态 | 新包截图/证据 |
|---|---|---|
| 原生启动，恢复默认关闭 | PASS | [启动](qa/RAW_HISTORY_ACCEPTANCE_STARTUP.jpg)、[恢复默认关闭](qa/RAW_HISTORY_ACCEPTANCE_RECOVERY_DEFAULT_OFF.jpg) |
| PCAP 恢复关闭时原生显示、开启后额外诊断候选 | PASS | [PCAP 原生](qa/RAW_HISTORY_ACCEPTANCE_PCAP_NATIVE.jpg)：每通道 23700 点；[诊断候选](qa/RAW_HISTORY_ACCEPTANCE_PCAP_CANDIDATE.jpg)：每通道 46600 点，237 Native+229 Candidate、0 已确认恢复 |
| LPD v3 重新打开及双通道分屏 | PASS | [LPD 文件信息](qa/RAW_HISTORY_ACCEPTANCE_LPD_OPEN.jpg)、[LPD 分屏](qa/RAW_HISTORY_ACCEPTANCE_LPD_SPLIT.jpg)，来自最终 GUI 正式保存的 237 帧文件 |
| PCAP raw 完整录制关闭并重新打开 | PASS | [raw 已关闭](qa/RAW_HISTORY_ACCEPTANCE_RAW_CLOSED.jpg)、[raw 重新打开](qa/RAW_HISTORY_ACCEPTANCE_RAW_OPEN.jpg)、[raw 双通道分屏](qa/RAW_HISTORY_ACCEPTANCE_RAW_SPLIT.jpg)；467 条全字段/字节核验见第 13 节 |
| 播放、暂停、seek、EOF 与退出历史 | PASS | [暂停 37.5%](qa/RAW_HISTORY_ACCEPTANCE_PAUSE.jpg)、[seek 87%](qa/RAW_HISTORY_ACCEPTANCE_SEEK.jpg)、[EOF 100%](qa/RAW_HISTORY_ACCEPTANCE_EOF.jpg)、[退出历史](qa/RAW_HISTORY_ACCEPTANCE_EXIT_HISTORY.jpg)；播放及切文件已原生操作；起点按钮手测见下表前阶段证据 |
| 双通道缩放与左键平移，ManualView | PASS | [缩放](qa/RAW_HISTORY_ACCEPTANCE_ZOOM.jpg)、[平移前](qa/RAW_HISTORY_ACCEPTANCE_MANUAL_BEFORE.jpg)、[平移后](qa/RAW_HISTORY_ACCEPTANCE_PAN_AFTER.jpg)；约 8.2 秒时间窗，刻度约 364–371 秒移动到 362–369 秒，两通道同步 |
| 原生 LPD+CSV 保存及带质量 CSV 导出 | PASS | [科学保存已关闭](qa/RAW_HISTORY_ACCEPTANCE_SCIENCE_CLOSED.jpg)、[质量 CSV 导出](qa/RAW_HISTORY_ACCEPTANCE_QUALITY_EXPORT.jpg)，第 10 节独立逐字段读回 |
| 紧凑实际窗口布局 | PASS | [1362×632 紧凑窗口](qa/RAW_HISTORY_ACCEPTANCE_COMPACT.jpg)，三栏、图表和底部日志可见；未将该原生截图写作 1280×720 实测 |
| SYNTHETIC 60 秒原始录制，期间冻结/恢复响应 | PASS | [压力运行](qa/RAW_HISTORY_ACCEPTANCE_SYNTHETIC_RUNNING.jpg)、[冻结](qa/RAW_HISTORY_ACCEPTANCE_SYNTHETIC_FREEZE.jpg)、[完整关闭](qa/RAW_HISTORY_ACCEPTANCE_SYNTHETIC_CLOSED.jpg)，1200 写入、0 丢失，独立读回见第 14 节 |
| 手动停止录制、停止监听及正常退出软件 | PASS | [停止监听](qa/RAW_HISTORY_ACCEPTANCE_SYNTHETIC_STOPPED.jpg)，原始文件已完整关闭；地址恢复后正常关闭，PID 44564 已退出。录制过程中直接关闭软件的故障场景未另行手测 |

下表是 `-delivery` 验收前 EXE 已完成的原生操作。截图虽然命名 `RAW_HISTORY_FINAL_*`，但均早于最后两类收尾修复；它们保留为阶段证据，最终新包操作以上表 `RAW_HISTORY_ACCEPTANCE_*` 为准：

| 验收前 EXE 的 Windows GUI 操作 | 状态 | 本轮截图/证据 |
|---|---|---|
| 启动后恢复开关默认关闭 | PASS | [默认关闭](qa/RAW_HISTORY_FINAL_RECOVERY_DEFAULT_OFF.jpg) |
| 原生选择实际 PCAP、打开恢复与诊断候选、叠加/分屏及 EOF | PASS | [验收前叠加](qa/RAW_HISTORY_FINAL_PCAP_OVERLAY.jpg)、[验收前分屏 EOF 100%](qa/RAW_HISTORY_FINAL_PCAP_SPLIT_EOF.jpg)；237 NATIVE、229 CANDIDATE、1 REJECTED、0 已确认恢复 |
| 验收前 EXE 导出普通/质量 CSV | PASS | 第 10 节输出及 `gui-delivery-pcap-csv-readback.json`，全部字段独立核验 |
| 验收前 EXE 的 raw 录制按钮及完整关闭 | PASS | [验收前 raw 已关闭](qa/RAW_HISTORY_FINAL_RAW_CLOSED.jpg)，467/467/467、drop0；`gui-delivery-raw-summary.json` 全记录读回 |
| 与最终 acceptance EXE 的关系 | 阶段记录 | 本表仅记录旧阶段，最终三类历史、保存、布局和新 60 秒压力已由上表独立验收 |

此前阶段构建的原生操作证据仍保留如下，说明实际操作范围：

| 前阶段 Windows GUI 的操作 | 状态 | 本轮截图/证据 |
|---|---|---|
| 原生选择并打开实际 PCAPNG，显示两路波形/质量 | PASS | [PCAP 候选叠加](qa/RAW_HISTORY_WINDOWS_PCAP_DIAGNOSTIC_OVERLAY.jpg)，467 条、237/0/229/1 |
| 原生选择并打开 GUI 生成的 `.lvdsraw` | PASS | [raw 文件信息](qa/RAW_HISTORY_WINDOWS_RAW_OPEN_DETAILS.jpg)、[raw 双通道分屏](qa/RAW_HISTORY_WINDOWS_RAW_SPLIT.jpg) |
| 原生 picker 打开最新 LPD v3，加载两路波形 | PASS | [LPD 文件信息](qa/RAW_HISTORY_WINDOWS_LPD_OPEN_DETAILS.jpg)、[LPD 237 帧分屏](qa/RAW_HISTORY_WINDOWS_LPD_SPLIT.jpg)；legacy/截断的 GUI 手动打开另未测 |
| 双通道叠加/分屏、左键拖动、滚轮缩放 | PASS | [PCAP 平移缩放分屏](qa/RAW_HISTORY_WINDOWS_PCAP_PAN_ZOOM_SPLIT.jpg)，root 已原生操作 PCAP/raw |
| 播放、暂停、进度/seek、回到起点、EOF、切文件、退出历史 | PASS | [暂停 68.9%](qa/RAW_HISTORY_WINDOWS_PAUSED.jpg)，root 已原生操作其余动作；静态截图不单独证明全部动作 |
| 冻结与恢复显示 | PASS | [冻结](qa/RAW_HISTORY_WINDOWS_FREEZE.jpg)，后台状态保持；原生恢复已操作 |
| 恢复开关和候选独立质量提示/显示 | PASS | 候选叠加截图、raw 详情黄色“恢复证据不完整”提示；导出质量分级独立核验 |
| PCAP 原始录制开始/完整关闭 | PASS | [raw 已关闭，467/467/467、丢失 0](qa/RAW_HISTORY_WINDOWS_RAW_CLOSED.jpg)；独立全字节读回通过 |
| 历史原生/质量 CSV 导出及原生 LPD+CSV 保存 | PASS | [科学保存已关闭](qa/RAW_HISTORY_WINDOWS_SCIENCE_CLOSED.jpg)及第 10 节真实文件读回 |
| 前阶段正常窗口布局 | PASS | 约 1360×900 三栏截图，无新增中央滚动或图表遮挡；最新原生紧凑实测为上表 1362×632 |
| 真实错误磁盘/满盘、只读输入、legacy/截断输入的原生提示 | NOT_RUN | 故障注入已有自动化，不替代这些 GUI 手动场景 |
| 最终 EXE 未覆盖的边界 GUI 场景 | NOT_RUN | 只读/磁盘错误/legacy/截断输入等按实际未测记录，不从阶段正常操作推定通过 |

上述截图均为本任务原生 Windows 操作，最终 `RAW_HISTORY_ACCEPTANCE_*`、验收前 `RAW_HISTORY_FINAL_*` 与更早 `RAW_HISTORY_WINDOWS_*` 分列，未使用旧 `LVDS_55AA_*` 截图替代。`RAW_HISTORY_WINDOWS_OUTPUT_ERROR.jpg` 是历史审计输出问题修复之前的错误证据，**不计入成功截图**；阶段和最终结果各自对应实际关闭/导出截图与独立读回。

## 13. 自动化与真实读回

独立参考工具：`tools/qa/lvds_raw_recovery_reference.py`，测试：`test_lvds_raw_recovery_reference.py`。标准库实现不调用生产 Rust 解码器，覆盖已知 CRC 向量、普通坏 CRC 原生接纳、真实位不足、1/4/7 相位、4096B 搜索上限、唯一候选、固定字段/时间、前可信帧、禁止相邻 UDP 补位、大小端 PCAP/时间/端点、显式截断及不支持包块、raw 边界/footer/ID 丢失和元数据/字节变化。

已执行并通过：

```powershell
& 'C:/Users/Administrator/miniconda3/python.exe' -m unittest discover -s tools/qa -p test_lvds_raw_recovery_reference.py -v
& 'C:/Users/Administrator/miniconda3/python.exe' -m py_compile tools/qa/lvds_raw_recovery_reference.py tools/qa/test_lvds_raw_recovery_reference.py
```

18 测试 PASS；最终日志 `target/qa/raw-history-20261025/reference-tests-18-green.log`。测试证据及脚本/输入哈希见 `reference-test-evidence-18.json`；该文件替代先前 17 测试证据。RED 阶段保留为本轮实际执行的摘要，未把摘要冒充完整原始失败日志。

集成阶段已有下列执行日志，均位于同一 QA 目录；这里按实际套件记录，不将重跑或过滤套件累加为独立测试总数，也不替代最终全工作区命令：

| 日志 | 实际结果 | 对本阶段的证据 |
|---|---|---|
| `core-storage-final-tests.log` | PASS：核心 99、存储 36，0 失败 | 原生 CRC 不门禁；字节同步/1、4、7 相位/缺尾位/连续性与资源边界；raw 格式与故障注入 |
| `core-storage-clippy-followup.log` | PASS：窄范围严格 Clippy 完成 | 核心/存储阶段检查，不代表最终 workspace |
| `backend-final-integrity-green.log` | PASS：74 通过、6 显式忽略 | localhost 原始 hook、混合质量隔离、原生 LPD/CSV、取消、范围导出、时钟首段、不完整源 |
| `desktop-final-integrity-green.log` | PASS：162 通过、6 显式忽略 | 当时桌面完整套件；历史模式/冻结/质量/旧 generation、显示缓存及后端回归 |
| `ui-history-candidate-merge-green.log` | PASS：14 通过 | 后续 UI 状态回归：候选填补诊断空段不抹除可信点、原生重启仍可见、恢复刷新显示节奏 |
| `backend-final-clippy.log` / `ui-history-candidate-merge-clippy.log` | PASS：窄范围桌面严格 Clippy 完成 | 不是最终新源码全工作区 Clippy 结论 |
| `reference-final-green.log` | PASS：18 | 独立 raw/bit/PCAP Reader 最新重跑 |
| `txt-reference-final-green.log` | PASS：28 | 原生 HEX 独立 Reader、CSV/LPD 验证和转换工具回归 |
| `recovery-destination-red.log` | RED：0 通过、2 失败 | 修复前 PCAP 目标隔离与 raw 目标语义隔离的实际失败证据 |
| `recovery-joint-gates-red.log` | RED：6 通过、5 失败、1 忽略 | recovery ON 时重复/TTL/冲突、目标隔离与后到 Native 正式存储资格的失败证据 |
| `recovery-final-desktop-green.log` | PASS：173 通过、0 失败、6 忽略 | 修复后非 Mock 桌面完整回归，含目标/分片隔离、来源/会话、序号/时间、TTL、恢复和 Native 独立去重 |
| `test-workspace-delivery.log` | 阶段 PASS：346 通过、0 失败、7 忽略 | 验收前工作区；桌面 180 项含 7 项 Mock 测试，不作为最后两类修复后的最终总数 |
| `overlap-red.log` / `overlap-native-red.log` | RED：前者 2 失败；后者 1 通过、1 失败 | 同时刻恢复→Native 曾使实时/历史曲线重置；加强用例另发现实时筛选重建后的显示路径丢失 |
| `overlap-ui-green.log` / `overlap-native-green.log` | PASS：UI 16 项；最终加强的 overlap 2 项 | Native 原位替换同跨度恢复样本，保留前后帧及真实时钟回退；冻结/质量重建后仍可见 |
| `history-envelope-red.log` / `history-envelope-provenance-red.log` | RED：前者 2 失败；后者 1 失败 | raw/PCAP 历史外层错误漏异常；早期修复会误删待重组合法分片依赖 |
| `history-envelope-green.log` | PASS：3 通过、0 失败 | raw/PCAP 历史 5 类外层/重组错误入拒绝统计/异常/审计；未知序号 None，合法帧依赖不丢失，Native 去重保持 |
| `overlap-native-clippy.log` / `history-envelope-clippy.log` | PASS：窄范围严格 Clippy 完成 | 两类修复独立阶段检查；最新完整 workspace 严格命令见第 15 节 |
| `test-workspace-acceptance.log` | PASS：351 通过、0 失败、7 忽略 | 最新冻结源码；核心 99，桌面 185（含 7 Mock），CLI 6，存储 36，transport 25，不累加过滤/阶段重跑 |

桌面六项忽略包含显式真实输入/端口/性能验收；核心另有一项忽略，工作区共七项。忽略不等于通过。真实 PCAP 两项已在显式环境下单独执行通过（如下）；本报告不把套件内的 ignored 本身算作真实数据 PASS。部分早期日志有警告或失败并已被后续修复重跑覆盖；保留其 RED 证据，不使用旧失败/旧 PASS 代替最终结果。PowerShell 的 `NativeCommandError` 包装有时仅表示 Cargo 向 stderr 输出构建进度；最终命令仍须结合 Cargo 完成行与实际退出码判定。

显式真实文件 Rust 测试在 `apps/desktop/src/backend_raw_history_real_tests.rs`。最终冻结源码使用实际输入再次单独执行，**2 通过、0 失败、0 忽略**（`real-capture-rust-acceptance.log`，0.26 秒测试体），467 UDP、93200 个可得 code10、237/229/1 分类与独立参考一致。此前 `real-capture-rust-delivery.log` 的 0.31 秒记录保留为验收前阶段；最新结论使用 acceptance 日志。工作区 ignored 不能替代该专项，缺少输入环境变量时不能算通过，也不会生成替代输入：

- `real_capture_recovery_matches_independent_bit_boundary_audit`：逐 UDP 与独立 JSONL 对照质量、位数、时间/seq8及全部可得诊断 code10；实际检查 93200 个值，分类 237/229/1 一致，无跨记录使用的位。
- `real_capture_raw_archive_preserves_every_received_udp_record`：真实 PCAP 全 UDP 写入新归档，检查全部记录时间/端点/原包索引/字节/ID/计数和精确 END/EOF。

实际归档后的独立全记录比对：PASS。生产 `RawArchive` 接收从本轮实际 PCAP 逐条读取的 467 个 UDP，写入新文件 `target/qa/raw-history-20261025/2026102401-actual-archive.lvdsraw`，随后 Rust Reader 和独立 Python `--raw` 均重新打开并校验。文件 183750B，SHA-256 `d3253e1619598e2424df2a995f3930e68e8c3fddb5a37c94cb8db1aac4852cbe`。

独立 Reader 对全部 467 条记录的 Payload 长度/顺序/全部字节、来源、实际抓包目标、纳秒时间、原包索引、raw ID、source_matches 校验通过，Payload 总计 136830B。END 的 observed/enqueued/written 均 467、dropped=0、last observed/written ID 均 467，状态 complete、explicit stop，后面精确 EOF。错误短帧和位异常未经筛除、修正或补字节。完整证据 `2026102401-actual-readback-summary.json` 与 `real-archive-independent-final.log`。

最终源码的真实文件测试新写出 `target/qa/raw-history-20261025/2026102401-acceptance-archive.lvdsraw`，183750B、467 条、136830B Payload；Rust 重新打开并对全部元数据/Payload、记录顺序及完整 END/EOF 校验通过，见 `real-capture-rust-acceptance.log`。验收前 `2026102401-delivery-archive.lvdsraw` 同为 183750B/467 条/136830B，见旧 delivery 日志；前述 `-actual-` 文件及独立 Python 读回也作为阶段双 Reader 证据保留。

最终 Windows EXE 的 PCAP 原始录制按钮生成 `data/raw-20261025-024836-232.lvdsraw`，183750B、SHA-256 `57abd6087247ed4307f087ec7d4eb10f0a6b17c54131ecae54ee3cc910f0b73b`。`gui-acceptance-raw-summary.json` 对全部 467 条的 Payload 长度/顺序/字节、来源、实际捕获目标、纳秒时间、原包索引、记录 ID 和 source_matches 独立比较 PASS；Payload 136830B，END observed/enqueued/written 均 467、dropped=0、完整 explicit stop，精确 EOF。header origin=PCAP_REPLAY、binding=`0.0.0.0:0`，各记录保留实际捕获目标；短帧和异常没有筛除或补位。同一新 EXE 重新打开该文件、显示双通道并完成播放/暂停/seek/EOF/退出，第 12 节给出原生截图。

验收前 Windows EXE 的原始录制按钮实际生成 `data/raw-20261025-020722-570.lvdsraw`：183750B，SHA-256 `a14e50efc9c8a252260772f7165bd181c93905e362008bd2f5749276b603d51a`。`gui-delivery-raw-summary.json` 证明独立 Reader 对该 GUI 文件的 467 条记录、136830B Payload 及全部时间/来源/抓包目标/原包索引/字节比较 PASS，END 467/467/467、dropped=0、精确 EOF。origin 为 PCAP_REPLAY，header binding 为 `0.0.0.0:0`；实际抓包目标保存在各 DATA 记录中，未拿 binding 冒充捕获目标。验收前 GUI 截图显示已关闭及丢失 0；该路径包含实际点击按钮。此段保留旧阶段证据，最终包以新 acceptance 文件为准；Windows 现场实时硬件接收和真实磁盘故障仍分别未测。

最终补验矩阵明确列出易漏的行为，不将某一个大工作区测试的成功自动当作所有 GUI 或磁盘场景通过：

| 必须覆盖的细项 | 当前状态 | 需要的证据 |
|---|---|---|
| 真实 467 UDP 归档，包括 268B 短帧及全部位异常 | PASS | 实际新归档 + Rust/独立 Reader 全字段/字节读回，Payload 136830B，容器 183750B，正常 END/EOF |
| 最终包完整文件 NATIVE 科学保存 | PASS | GUI `023840.901` 保存 237 帧，LPD 96973B、CSV 23700 样本行，独立全字段读回 |
| 最终包带质量导出候选，和普通 CSV/LPD 隔离 | PASS | `024800-763-quality-v1.csv` 46600 行，237 NATIVE+229 CANDIDATE，0 误标恢复；普通格式仍只含 237 原生帧 |
| 最终包标准 CSV 的 11 列、code10/重建 ADC/时间全字段 | PASS | 最终 GUI 实际写出文件 260700 字段逐项核对，不从图表抽稀取预期 |
| 字节同步前导噪声、多个合法头、已有原生不重复交付 | PASS | 核心/后端自动化；`core-storage-final-tests.log` 与混合质量回归，Windows 观察另验 |
| 跨目标端点恢复锚点隔离 | PASS | PCAP/raw 实际入口用例 RED→GREEN；目标地址或 Captured/SocketBinding 语义变化隔离，Unknown 不确认恢复；分片不跨目标拼接 |
| 跨来源/会话、外层/seq8 断点、设备时间重启的完整入口组合 | PASS | 原阶段非 Mock 桌面 173 项及最新工作区回归，跨流清锚点/依赖，保留 Native 去重；序号回绕及新锚点恢复通过 |
| 重复帧/乱序/TTL/重组冲突与恢复同时开启 | PASS | `recovery_joint_gates_preserve_duplicates_and_expire_the_anchor_at_exact_ttl`、`recovery_joint_gates_break_on_conflicts_gaps_expiry_and_late_native_then_recover`；重复不续锚点，TTL 2 秒 |
| 恢复不抑制后到 Native 正式保存 | PASS | `confirmed_recovery_cannot_consume_a_later_native_frame_storage_eligibility`；Native/恢复各 256 项、5 秒去重，普通 LPD/CSV 仍仅 Native |
| 同时间恢复帧与后到 Native 的曲线保留 | PASS | 两项加强 overlap 回归；实时/历史、Native/Confirmed/Diagnostic、冻结/重建均验证前后帧及 Native 优先，不排序真实时钟回退 |
| raw/PCAP 历史外层解析/重组错误追溯 | PASS | 三项 `history_envelope_*` 回归；每种输入均保留字节/ID/来源/原因，未知序号 None，合法分片依赖不被坏包清除 |
| 恢复开关/候选显示、历史换文件与再打开 | PASS | 最终包默认关闭、PCAP OFF 23700/ON 诊断 46600 点，LPD/raw 再打开及双通道成功；新截图见第 12 节 |
| 新 seek 取消旧窗口、拒绝旧 generation、CSV 导出时可暂停 | PASS | `a_new_seek_cancels_the_old_window_before_it_is_published`、旧窗口 UI 状态及独立 export worker 回归 |
| 最终包播放/seek/暂停/EOF/切文件/退出历史 | PASS | 新包暂停 37.5%、seek 87%、EOF 100%、退出等原生操作，截图见第 12 节；起点按钮与旧阶段暂停 68.9%另见前阶段证据；显式取消长时加载/导出仍仅有自动化与有界设计 |
| 不完整 END、无 END 和截断 raw 的分层处理 | PASS | raw Reader 截断/边界用例；历史有效前缀及源损坏时 export 必须失败回归；GUI提示另验 |
| 同名文件拒绝、注入 write/flush/close 失败与队列溢出 | PASS | 自动化故障注入和独占创建回归；原始/科学/审计独立状态，晚到 close 不能抹除失败 |
| 真实磁盘满/物理 write/flush/close 故障、只读输入/错误路径的 GUI 提示 | NOT_RUN | 不能把故障注入、已修复前 OUTPUT_ERROR 截图或正常关闭当作这些场景实测 |
| 手动停止原始录制与停止监听 | PASS | 最终 SYNTHETIC 1200 条归档明确关闭、0 丢失、精确 END/EOF 后停止监听；录制中直接切输入/关软件未额外做 GUI 故障场景，关闭/Drop/取消边界以自动化证据为限 |
| 取消待写审计记录、畸形 PCAP 与不完整 END | PASS | `cancelled_pending_audit_record_marks_the_footer_incomplete`、`malformed_pcap_marks_both_raw_and_audit_incomplete`；真实磁盘故障/GUI另验 |
| LPD v2/错误版本/记录截断，raw/PCAP 非法长度的 Reader 拒绝 | PASS | 核心/存储/历史 Reader 自动化；原生 GUI 反应另验 |

这些预期数来自本次独立分类和既有文件布局，不是通过阈值。任何生产结果差异需要说明来源过滤、去重或真实失败原因，不能通过删记录、补值或改变质量标签凑数。

## 14. 性能与有界性

已明确的源码边界：单条 raw Payload 最大 65535B；原始队列/批次有界；恢复科学 Payload 搜索最多 4096B；不跨记录拼位；历史队列、索引、异常列表和可见窗口有界；导出从独立流式 Reader 读取完整样本，不从绘图缓存取样。

本轮另外执行 **SYNTHETIC 独立 Python 参考工具计时**，不创建抓包、不访问网络/硬件，不运行 Cargo 或生产 EXE。复现脚本和机器结果为 `target/qa/raw-history-20261025/SYNTHETIC-reference-benchmark.py` / `.json`。Windows Python 3.13、QueryPerformanceCounter，七批取每次耗时中位数，10 次预热；每批调用数和全部计时样本在 JSON 中。每种输入先后检查预期分类和边界。

| SYNTHETIC Python 路径 | 每批调用 | 中位耗时/次 | 100 次调用期间 tracemalloc 峰值 |
|---|---:|---:|---:|
| 先前独立严格原生 decoder | 2000 | 111.383μs | 13410B |
| 当前独立原生 body 校验/解包，不含 CRC/搜索 | 2000 | 108.158μs | 16342B |
| 完整离线原生审计，包含参考 CRC 和搜索 | 500 | 1.063ms | 30819B |
| 完整同记录 1bit 候选离线审计 | 500 | 0.939ms | 30820B |
| 缺 4 尾位候选离线审计 | 500 | 0.914ms | 30030B |
| 4096B 全零噪声边界 | 200 | 2.104ms | 267222B |
| 4096B 固定头密集输入；最多保留 32 候选 | 20 | 27.510ms | 617958B |
| 65535B 超限输入立即拒绝 | 5000 | 1.251μs | 770B |

所有 SYNTHETIC 分类和候选上限断言通过；超限路径没有搜索整个输入。这里比较的是两个不同的独立 Python decoder，属于描述性计时，不能作为生产正常路径的受控前后回归。离线审计故意对 NATIVE 做参考 CRC 和位搜索，生产原生快路径不做这些工作；不能把离线审计时间当作生产每帧时间。JSON 中相对 20Hz/50ms 预算的比例只是算术，不是实际按节奏接收；tracemalloc 不是进程 RSS、Rust 队列内存或长时浸泡测试。

另已运行 **SYNTHETIC 生产 Rust 核心 Release CPU 专项**，使用当前直接 decoder 作同一构建基线，原生 direct/off/on 顺序每批轮换，10 次预热、七批，计时包括结果销毁与生产分配，不包括断言/输出：

```powershell
cargo test -p langmuir-core --release --locked synthetic_production_recovery_cpu_measurements -- --ignored --nocapture --test-threads=1
```

最终测试 1 通过、0 失败，3.03 秒；窄范围 `cargo clippy -p langmuir-core --all-targets --locked -- -D warnings` 退出 0。使用最终日志 `SYNTHETIC-rust-core-release-perf-final.log` / `SYNTHETIC-rust-core-perf-clippy-green.log`，完整九路径逐批结果、环境和源码哈希在 `SYNTHETIC-rust-core-release-perf-final.json`；不引用已修正的中间计时或中间 Clippy 失败作为最终结果。

| SYNTHETIC Rust Release 路径 | 中位耗时/次 |
|---|---:|
| 当前直接原生 269B decoder | 393.132ns |
| `inspect` 原生，恢复关闭 | 619.448ns |
| `inspect` 原生，恢复开启 | 623.516ns |
| 同记录完整 1bit 已确认恢复 | 6.819250μs |
| 同记录完整 4bit 已确认恢复 | 6.752750μs |
| 缺 4 尾位的 269B 候选 | 7.184200μs |
| 4096B 全零噪声 | 76.754800μs |
| 4096B 错相固定头密集输入 | 570.138800μs |
| 65535B 超限且含合法帧前缀，立即拒绝 | 110.890ns |

当前 `inspect` 相对同一当前 decoder 的中位比值为关闭 1.575674、开启 1.586022；这描述包装和结果构造成本，基线不是旧 EXE/旧 checkout，不能声称历史版本性能回归已经测量。原生 code10/时间保持不变、参考 CRC 不执行；SYNTHETIC 完整位帧满足确认，缺尾位仅候选；最大输出 269B/2152 位、最多保留 2 个候选，超限即使有合法前缀也无候选/输出，断言均通过。上述为公开结果/源码边界，没有内部搜索步数或 allocator 仪器测量。

最终冻结源码的 acceptance EXE 又独立执行 **SYNTHETIC localhost 20Hz、60 秒原始录制**，与第 12 节同一 PID 44564 和交付哈希对应。发送内容为实际 PCAP Payload 的人为节奏重复，**不是原始采集时序、真机接收或科学解码吞吐测试**。最终结果如下，旧阶段压力另保留在后文：

| 最终 SYNTHETIC 短时压力项目 | 实际结果 |
|---|---|
| 发送与节奏 | 1200 条、351597B；总耗时 60.0008938 秒，首末 59.9487423 秒，实测 20.0004196Hz；间隔中位 50.0017ms、均值 49.998951ms、范围 13.7686–86.4506ms，最大迟到 36.6133ms |
| 实际原始归档 | `data/raw-20261025-025331-925.lvdsraw`，471817B，SHA-256 `0f1831f5d131f1832ec2d0a76a1c681feacbc69490d2dffb59a8508fb4d6498f`；observed/enqueued/written 均 1200、dropped=0、Payload 351597B |
| 独立读回 | 所有 Payload 字节及顺序与发送 manifest 一致；原始记录来源/目标语义、`source_matches=false` 均核对；完整 explicit stop END、精确 EOF，PASS |
| 来源与边界 | sender=`127.0.0.1:58451`，binding=`127.0.0.1:1534`，期望来源仍为 `192.168.1.128:1534`；origin=LIVE_UDP，每条目标语义为 SocketBinding。故意来源不匹配，验证来源过滤之前的 raw hook，科学接收为 0 属预期 |
| 进程短时观察 | 12 个样本、采集约 56.132 秒；WorkingSet 133435392–133648384B，PrivateBytes 157773824–157917184B，全部 `Responding=true` |
| 原生交互和关闭 | 运行中冻结/恢复响应，随后手动停止录制与监听、正常退出软件；第 12 节给出 RUNNING/FREEZE/CLOSED/STOPPED 截图 |

最终证据为 `SYNTHETIC-acceptance-localhost-20hz-raw-readback.json`、`SYNTHETIC-acceptance-localhost-20hz-memory.json` 与 `SYNTHETIC-acceptance-localhost-20hz-manifest.jsonl`。manifest 为 1279033B，SHA-256 `1a1a959bfda4a218e9669c7ca3cf43e456db4f7e7e2020889430badc6bf228b3`。1200 条是接收归档独立比较后的结果，不仅是发送器成功计数；12 次进程采样和交互响应只证明本次短时观察，不能证明小时级无泄漏或 UI 延迟上限。

此前阶段原生 EXE 已执行过 **SYNTHETIC localhost 20Hz 原始录制压力**，下面保留原始记录，不作为最终 EXE 的替代证据：重复实际 PCAP Payload，并以人为 50ms 节奏发送，不声称保留原始采集时序。发送器 60.0006392 秒成功 `sendto` 1200 条、351597B，首末报文 59.9487197 秒，实测 20.000427Hz；间隔中位 49.9959ms，范围 43.6058–55.6281ms。独立接收归档比较才是保存证据，不能仅凭成功发送计数判定接收。

实际 GUI 原始录制收/入队/写入均 1200、drop0，文件 `data/raw-20261025-015133-323.lvdsraw` 为 471817B、SHA-256 `ffe9a8feec5344c041fc7e77f351ddd1097fd8b7cb6ecbe565463e7e0d124c77`。独立 Reader 对 manifest 中所有 Payload 字节、顺序、端点和 `source_matches=false` 比较 PASS，END 完整且精确 EOF。来源故意为 `127.0.0.1:55195`，接收 binding `127.0.0.1:1534`，与期望 `192.168.1.128:1534` 不匹配，验证来源过滤之前的 raw hook；科学接收为 0 属预期，因此这不是科学解码吞吐测试。

该旧阶段压力期间约 55.365 秒取得 12 个进程样本：WorkingSet 168087552–168210432B、PrivateBytes 163094528–163213312B，所有采样 `Responding=true`，冻结按钮原生操作可响应；[旧阶段 20Hz 活动状态截图](qa/RAW_HISTORY_WINDOWS_SYNTHETIC_20HZ_ACTIVE.jpg)。这些是短时观察，不证明更长运行无泄漏或 UI 延迟上限。证据 `SYNTHETIC-localhost-20hz-raw-readback.json`、同 stem manifest/log，以及 `SYNTHETIC-localhost-20hz-memory.json`。该压力使用目的端点隔离修复前的阶段 EXE；最终修复后的重跑已由上表 acceptance 证据单独记录。

小时级浸泡、真实设备持续 20Hz 科学接收、旧/新 Rust Release 前后对照、长历史文件峰值/定位取消延迟以及真实 GUI 原始队列溢出提示：NOT_RUN。上述短时 loopback/raw 保存和核心 CPU 微基准均不能替代这些场景或真机验收，且没有按硬件耗时阈值硬判通过。

## 15. 工作区检查和 Windows 正式交付

下表为全部已发现收尾修复冻结后的最新执行记录，包含同时间 Native 曲线保留及历史外层错误/重组错误追溯。耗时和退出码来自 `acceptance-check-results.json`；此前 `final-check-results.json` 与 `delivery-check-results.json` 均仅为阶段历史，不作为本表依据。

| 命令/交付步骤 | 状态 | 结果 |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | 退出 0，1.572 秒，`fmt-acceptance.log` |
| `cargo check --workspace --locked` | PASS | 退出 0，2.666 秒，`check-acceptance.log` |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS | 退出 0，28.863 秒；核心 99+1 ignored，桌面 185+6 ignored，CLI 6，存储 36，transport 25；合计 351 通过、7 忽略、0 失败 |
| `cargo build --release -p langmuir-desktop --locked` | PASS | 退出 0，26.694 秒，`build-release-acceptance.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | 退出 0，6.907 秒，`clippy-workspace-acceptance.log`；完整严格命令 |
| 最新 Windows MSVC x64 静态 CRT 非 Mock 构建 | PASS | `build-windows-static-acceptance.json`：02:35:51–02:36:21、30.118 秒、exit 0，`-C target-feature=+crt-static`，Mock=false、HDF5=false |
| 最新 EXE、ZIP、README，归档字节与哈希核验 | PASS | `windows-package-acceptance-verification.json`，PE32+ x64、无动态 MSVC CRT imports、ZIP 内容/CRC 与 EXE/README 一致 |
| 验收前 EXE PCAP、CSV/质量 CSV、raw 关闭与独立读回 | 阶段 PASS | 第 10、12、13 节的 `gui-delivery-*` 及 `RAW_HISTORY_FINAL_*`，明确早于最后两类修复 |
| 最新 acceptance EXE 的三类历史、保存、紧凑布局及新 20Hz 压力 | PASS | 第 10、12–14 节的新包实际操作与独立读回；1362×632 紧凑截图、1200 条短时压力，0 丢失；停止录制/监听并正常关闭软件 |

最新 acceptance 包标识如下；验收前 `-delivery` 包已保留于 `target/delivery/previous-pre-acceptance-20261025/`，更早阶段包在 `previous-20261025-012919/`。本表当前路径和哈希对应最后两类修复后的新构建：

| 最终交付物 | 路径 | 字节数 | SHA-256 |
|---|---|---:|---|
| 源 EXE / 交付 EXE（字节一致） | `target/x86_64-pc-windows-msvc/release/langmuier.exe` / `target/delivery/windows-x64/LangmuierProbeGroundControl.exe` | 10174464 | `e8b923dd9818ebc294cc8fdc8bea48235961fd055eb88965dd1c9169fdc6fd1f` |
| ZIP | `target/delivery/LangmuierProbeGroundControl-windows-x64.zip` | 4209507 | `b97cf5848c701f24c527e557fd1cd2b7bc705cf5a468ba82ab9d1b0f68cab6e3` |
| README / ZIP 中 README（字节一致） | `docs/WINDOWS_README.md` / `target/delivery/windows-x64/README.md` | 3269 | `1737d27a4fb154aabca539f57971d56c91e782c9d1b494fbfa79ddd92ecd2d4b` |

最终构建命令为 `cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked`，RUSTFLAGS=`-C target-feature=+crt-static`；Mock/HDF5 均未启用。ZIP 仅含 `LangmuierProbeGroundControl.exe` 和 `README.md`。包检查验证包字节和 PE 信息；原生 UI 操作按第 12 节的构建阶段分别记录，不把打包通过当作 GUI 操作通过。

已完成命令的退出码和耗时来自 `target/qa/raw-history-20261025/acceptance-check-results.json`，冻结源码指纹见 `acceptance-source-manifest.json`。最终独立复核 `final-source-log-audit.json` 确认 70 个冻结源码哈希不变、五条命令均 exit 0、工作区日志为 351 通过/0 失败/7 忽略，包的 PE/ZIP/字节验证再次 PASS。工作区 ignored 有显式输入/性能前提；真实 PCAP 两项及 Rust Release CPU 专项的已有显式执行按各节阶段记录，其余忽略项目不自动计为 PASS。最终 GUI/压力按同一冻结源码和当前包执行；若之后修改源码，需要相应重新验证及构建。

## 16. 尚未验收的硬件与明确限制

- 真机网卡→操作系统→真实 UDP Socket 的持续采集、真实 RS422 串口和整机连续运行：NOT_RUN。本轮无自动硬件连接或控制指令发送。
- 实际文件中 229 条候选缺少真实 CRC 尾位，没有完整恢复；不提供“465 条已成功修复”的结论。
- 参考 CRC 覆盖范围未获厂家最终确认；CRC 匹配不构成硬件故障根因结论。
- 原始容器无内容校验和；异常退出可能只有不完整前缀，可用于诊断，不能显示完整保存。
- 原始/PCAP 历史定位目前为后台重扫；本次小文件的 GUI 播放/seek/交互已测，长文件耗时、峰值内存与定位取消延迟尚未实测。
- 本阶段软件验收已完成：全部已发现收尾修复、最新五命令、静态 CRT acceptance 包、新 EXE 三类历史/保存/1362×632 紧凑布局与 60 秒 SYNTHETIC 20Hz 短压均有实际证据。小时级浸泡、真机和物理磁盘故障等明确未测项目仍为 NOT_RUN，不据此宣称整机硬件验收完成。

## 17. Git 修改和证据索引

本阶段不自动提交或推送 Git，不删除用户文件。已有未提交修改被保留。以下列出本阶段涉及的文件范围，不将本地 `target/` 输出视为已跟踪源码：

| 类别 | 文件 |
|---|---|
| 工作区依赖/文档入口 | `.gitignore`、`Cargo.toml`、`Cargo.lock`、`README.md`、`apps/desktop/Cargo.toml`、`apps/desktop/README.md`、`crates/langmuir-core/Cargo.toml` |
| 核心正常协议/恢复 | `crates/langmuir-core/src/lib.rs`、`lvds.rs`、新增 `lvds_recovery.rs`/`lvds_recovery_tests.rs`/`lvds_recovery_perf_tests.rs` |
| 原始存储 | `crates/langmuir-storage/src/lib.rs`、新增 `raw.rs`/`raw_tests.rs` |
| 桌面集成 | `apps/desktop/src/app.rs`、`app_layout_tests.rs`、`backend.rs`、`bridge.rs`、`mock.rs`；新增 `app_history.rs`、`backend_history.rs`、`backend_raw_storage.rs`、`backend_recovery_audit.rs`、`backend_raw_history_real_tests.rs`、`backend_capture_history_tests.rs` |
| 规范/计划/独立审计/交付 | `docs/LVDSRAW_FORMAT.md`、本报告、`docs/WINDOWS_BUILD.md`、新增 `docs/WINDOWS_README.md`、`docs/superpowers/plans/2026-10-25-raw-recovery-history.md`、两份 Python 工具/测试；`docs/qa/RAW_HISTORY_ACCEPTANCE_*` 最终截图、`RAW_HISTORY_FINAL_*` 验收前及 `RAW_HISTORY_WINDOWS_*` 更早截图；`docs/qa/raw-history-acceptance-20261025/` 的最终关键证据副本 |

主要本轮机器证据位于 `target/qa/raw-history-20261025/`，该目录是本地构建证据目录，不声称所有内容都被 Git 跟踪：

最终关键 summary/log/script 共 25 份另有逐字节一致副本保存在 [持久证据目录](qa/raw-history-acceptance-20261025/)，[evidence-manifest.json](qa/raw-history-acceptance-20261025/evidence-manifest.json) 列明各文件大小和 SHA-256。该目录不包含完整逐 UDP JSONL、SYNTHETIC 发送 manifest 或科学/原始归档；它们仍在下面明确列出的 `target/qa/raw-history-20261025/` 或 `data/` 本机路径，不声称已随 Git 或交付 ZIP 分发。

- `2026102401-final-frames.jsonl` / `2026102401-final-summary.json`：最终实际抓包逐条分类和汇总。
- `reference-tests-18-green.log` / `reference-test-evidence-18.json`：当前独立测试执行和文件哈希。
- `lvds-txt-current-native-regression.json`：本轮 20 条用户 HEX 的原生独立回归，以及当前审计版本与最终 PCAP 行完全一致的检查。
- `SYNTHETIC-reference-benchmark.py` / `.json`：仅独立 Python 的计时、分配峰值与搜索边界；不作为生产性能通过结论。
- `SYNTHETIC-rust-core-release-perf-final.log` / `.json` 与 `SYNTHETIC-rust-core-perf-clippy-green.log`：当前 Rust 核心 CPU 计时和边界，明确排除端到端采集性能。
- `SYNTHETIC-localhost-20hz-raw-readback.json`、`SYNTHETIC-localhost-20hz-manifest.jsonl`、同 stem log 和 `SYNTHETIC-localhost-20hz-memory.json`：阶段原生 60 秒 loopback 原始归档，非设备科学接收或长时 soak。
- `SYNTHETIC-acceptance-localhost-20hz-raw-readback.json`、`SYNTHETIC-acceptance-localhost-20hz-manifest.jsonl`、`SYNTHETIC-acceptance-localhost-20hz-memory.json`：最终 acceptance EXE 的新 60 秒/1200 条/0 丢失归档，全部 Payload/顺序独立读回及 12 次进程采样；manifest 留在本地 target，raw 为 `data/raw-20261025-025331-925.lvdsraw`。
- `2026102401-actual-archive.lvdsraw`、`2026102401-actual-readback-summary.json`、`real-archive-independent-final.log`：此前实际归档全部 467 条 Rust/独立 Reader 读回 PASS；`2026102401-delivery-archive.lvdsraw`、`real-capture-rust-delivery.log`：验收前源码真实数据两项再次通过。
- `2026102401-acceptance-archive.lvdsraw`、`real-capture-rust-acceptance.log`：最终冻结源码真实数据两项 2 通过/0 失败，全部 467 条元数据/Payload 与 END/EOF 校验；不把 workspace 中的 ignored 算作该专项执行。
- `recovery-destination-red.log`、`recovery-joint-gates-red.log`、`recovery-final-desktop-green.log`、`recovery-joint-gates-clippy.log`：目标隔离及 recovery ON 组合门禁 RED→GREEN、窄范围严格检查。
- `overlap-red.log`、`overlap-native-red.log`、`overlap-ui-green.log`、`overlap-native-green.log`、`overlap-native-clippy.log`：同时间 Native/恢复样本曲线及重建后的显示路径 RED→GREEN。
- `history-envelope-red.log`、`history-envelope-provenance-red.log`、`history-envelope-green.log`、`history-envelope-clippy.log`：外层/重组错误异常列表、审计与待重组依赖 RED→GREEN。
- `gui-delivery-pcap-csv-readback.json`：验收前 EXE 实际 CSV/质量 CSV 全字段核验；`gui-final-lpd-csv-readback.json`：更早 GUI 正式 LPD/CSV 全样本核验。
- `gui-delivery-raw-summary.json`：验收前 EXE 按钮生成 raw 的 467 条全记录、元数据和字节核验。
- `gui-acceptance-process.json`、`gui-acceptance-lpd-csv-readback.json`、`gui-acceptance-quality-readback.json`、`gui-acceptance-raw-summary.json`：最终 EXE 身份、237 帧正式 LPD/CSV、466 帧质量 CSV 及 467 条 PCAP raw 的全字段/字节核验；实际输出路径和哈希见第 10、13 节。
- `acceptance-check-results.json` 与各 `*-acceptance.log`：最新冻结源码五条命令均退出 0、工作区 351 通过；`delivery-check-results.json` 的 346 及 `final-check-results.json` 仅为阶段历史。
- `acceptance-source-manifest.json`、`final-source-log-audit.json`、`build-windows-static-acceptance.json`、`windows-package-acceptance-verification.json`：70 个最终源码指纹、五命令/测试日志复核、静态 CRT 非 Mock 构建、包字节/PE/哈希。
- `docs/qa/RAW_HISTORY_ACCEPTANCE_*`：最终 EXE 原生三类历史/保存/平移缩放/紧凑布局/短压/停止操作，第 12 节逐项链接；`RAW_HISTORY_FINAL_*` 为验收前、`RAW_HISTORY_WINDOWS_*` 为更早阶段。OUTPUT_ERROR 是审计修复前错误，不作成功证明。
