# LVDS-MULTITYPE-S01 三类型统一接收验收

日期：2026-11-18（Asia/Shanghai）。本报告记录本轮源码、自动化测试及新Windows非Mock Release证据；尚未执行的项目标记 NOT_RUN，缺少真实设备输入的项目标记 BLOCKED。

## 1. 范围与协议依据

本轮范围是269B公共帧解析、AA/AB/AC分流、原始UDP归档与AA回归。AB波形解码、AC参数解码及三视图GUI不在本轮范围。RS422编码、发送和星务模拟器保持既有行为。

直接协议源：根目录 `协议 (PH20261115).xlsx`，SHA256 `916a0f154ad71e2325a5484cb1345de0fbe18319133282c83c7b73ea3368c11f`。工作簿只读，未改写。

| 工作表与单元格 | 直接依据 | 状态 |
|---|---|---|
| `LVDS_公共格式!I11:I19`、`F22:F23` | EB90、SRC55、DSTAA、LEN0107、TYPE AA/AB/AC、260B DATA、2B尾字段；总长269B、大端 | PASS |
| `LVDS_公共格式!F24` | CRC16-CCITT-FALSE，poly1021、initFFFF、不反射、无末尾异或，覆盖260B DATA，不含TYPE | PASS |
| `LVDS_公共格式!F30:J32` | AA每50ms，AB与AC约每1s，三类均为260B DATA/269B总长 | PASS |
| `LVDS_公共格式!F36:H42` | 固定1秒边界插辅助包，第20AA后等待10ms发AB，再10ms发AC；busy各最多20ms；科学优先可放弃剩余辅助包 | PASS |
| `LVDS_科学数据包!I17:J24`、`F28:F36` | AA：8B时间、双通道各125B打包数据、seq8、保留0；各100×10bit，500μs间隔 | PASS |
| `LVDS_激励信号数据包!I17:J31`、`F34:F38` | AB：完整260B域；pa0_seq在DATA偏移23，波形区偏移60/200B；无有效值仍发送固定帧 | PASS |
| `LVDS_GNSS数据包!I17:J28`、`F31:F35` | AC：GNSS原始114B在DATA偏移13，姿态59B在偏移140；两更新计数独立，无有效值仍发送固定帧 | PASS |

生产接收延续用户已确定的 `CRC_NOT_CHECKED` 策略：不以CRC拒收，不自动修复尾字段，不新增严格模式，不将未验证显示为通过。原始CRC字节进入原始归档。科学数据未经CRC完整性验证。

## 2. 实施前审计与最小改动路径

已先执行 `git status --short`、`git diff --stat`。工作区有星务模拟器相关修改和未跟踪文件，原协议DOC存在用户删除/替换状态。本轮不回退、不暂存、不提交这些修改，不覆盖原PCAP/LVDSRAW/LPD/CSV或前期报告。

实施前发现：

1. `lvds.rs` 的 `decode_scientific_frame` 同时检查269B公共结构、TYPE=AA、DATA251=0和科学字段。直接让AB/AC通过该接口会错误执行AA约束。
2. `lvds_recovery.rs` 原字节和位同步固定AA科学头。AA位恢复质量门禁应保留；完整AB/AC要先由公共解析识别，不进入AA位恢复。
3. `UdpLvdsIngress::push_observed_at` 原来把所有重组结果交给科学恢复/去重链。AB/AC没有业务解码器会落入科学解析错误。
4. 外层u32序号监测原在Accepted科学帧分支推进，辅助包插入后有伪报UDP缺口风险。
5. `UdpLvdsReceiver::drain_recorded` 已在来源过滤和协议解析前，将Socket实际返回的完整payload交给 `RawRecording::record`。原始保存边界正确，保留现有格式和写入机制。
6. PCAP实时回放、PCAP历史和LVDSRAW历史已共用 `UdpLvdsIngress`；LPD v3是纯AA科学存储，无原始辅助帧可恢复。

实施前链路：`UDP → 原始录制 → 来源过滤 → 24B解析/重组 → AA同步/解码/去重 → 科学显示/保存`。

本轮生产链路：`UDP → 原始录制 → 来源过滤 → 24B解析/重组 → 公共269B结构检查 → TYPE分流`。AA继续原解码/恢复/科学保存，AB与AC保留完整原始公共帧及来源信息、发送受控类型事件，未知TYPE独立计数并保留原始记录。最终自动化状态在下表和第8节记录。

## 3. 生产路由与帧保留

| TYPE | 业务路由及保留边界 | 状态 |
|---|---|---|
| AA | `LvdsPacketType::Science` → 原 `LvdsBitAlignmentRecoverer` / `UdpLvdsScienceGate` → 原科学绘图、CSV/LPD v3；每通道100点、500μs | PASS |
| AB | `Excitation` → `LvdsPacketObservation` / `BackendEvent::LvdsAuxiliaryFrame`；完整269B/260B DATA/原始CRC、外层u32、来源、raw ID、接收时间 | PASS |
| AC | `GnssAttitude` → 同AB保留/事件；不构造科学样本，不解析GNSS/姿态参数 | PASS |
| 未知TYPE | `Unknown(u8)` → 原始记录和完整公共帧保留、独立未知计数；不进入AA decoder | PASS |
| 公共结构异常 | `structure_errors`与审计；可识别为AB/AC的坏公共头不计AA decode失败，不进入AA位恢复 | PASS |

完整帧计数必须在重组后取得，不能把多个UDP分片计为多个内层帧。AB/AC不依赖20AA累计数量、彼此相邻或当前秒内辅助包完整性。

公共模型位于 `crates/langmuir-core/src/lvds_common.rs`。`parse_common_frame(&[u8]) -> Result<LvdsCommonFrame, FrameError>` 只校验公共结构；模型以原始 `[u8;269]` 持有完整帧，`data()`返回原260B、`raw_crc()`返回原尾字段，不执行CRC或业务解码。精确269B可保留未知TYPE；有界 `find_common_frame` 对非精确输入只接受唯一、完整且TYPE为AA/AB/AC的公共候选。

`LvdsPacketObservation`补充外层u32、record ID、分片raw依赖ID、原接收Unix纳秒时间、来源/目标、同步偏移和重复/冲突处置。新事件复用既有后台事件体系，没有新增UDP Socket。辅助包完整结构到达计数包括重复到达，`auxiliary_duplicates/conflicts`另行报告处置，不把它们混入科学解析失败。

统计口径如下：UI总UDP数为 `udp_packets + source_rejected_packets`，其中 `udp_packets`保留原“来源匹配输入”口径；其和才包含本接收入口看到的来源拒绝数据。AA/AB/AC/未知计数来自重组后识别的完整269B原生或字节同步帧；AA `parsed_frames`另含通过既有质量门禁的确认位恢复，可能与公共AA计数不同。`accepted_frames`表示去重/冲突门禁后的科学接纳，不能与UDP数、分片数、类型收到数或文件写入数互换。

辅助待发布队列上限128，淘汰最旧项时递增 `type_events_dropped`和通用 `metrics.events_dropped`。这是上层事件丢失统计；原始归档的observed/enqueued/written/dropped仍由独立真实写入accounting计算，不以事件队列状态推断原始文件丢失。

## 4. 原始UDP保存边界

原始 `.lvdsraw` 仍使用现有 `RawArchive`：Socket收到的payload（含24B FPGA封装）在来源过滤、外层解析、去重、同步或恢复前入队。源不匹配、坏头、短帧、位错、外层错误、重复及未知类型均属于记录范围。原始记录不使用GUI缓存，也不替换为重组或修复后的269B。

`RawRecordingStatus` 的 received/enqueued/written/dropped/closed/complete/error来自真实归档accounting。入队不等于写入；队列过载和写盘失败必须使状态明确不完整。只保证Socket实际收到且录制期间可排队写入的UDP，归档计数不证明网卡/系统层没有丢包。

受控网络测试 `synthetic_localhost_raw_roundtrip_preserves_all_types_and_rejected_payloads` 已通过：真实localhost UDP Socket接收10条SYNTHETIC payload，包含AA/AB/AC、坏头、268B短帧、坏外层、重复、未知类型、错位和来源不匹配。关闭归档后真实重开文件，10条payload及顺序、来源、目标、raw ID、source_matches和时间元数据逐项相等，footer observed/enqueued/written=10、dropped=0、完整关闭并到精确EOF。该测试不是FPGA设备数据验收。

真实PCAP全量重新归档/读回专项PASS：`real_capture_raw_archive_preserves_every_received_udp_record` 将真实原始PCAP的467条UDP写入本轮新文件 `target/qa/lvds-multitype-s01/original-pcap-roundtrip.lvdsraw`，关闭后Rust Reader重开，全部136830B payload和全部来源/目标/ID/时间/原包索引逐字段相等，完整footer/精确EOF通过。归档183750B、SHA256 `d08b5ddd1500719e53430e2ee8905428a61bb2704c8df57b19557a21809ae223`。

Python独立Reader另行重开同一新文件，与原PCAP逐条比较全部字节和元数据也PASS。END observed/enqueued/written均467，dropped=0，payload_bytes=136830，最后record ID=467、正常停止且无尾部多余字节。证据：`qa/lvds-multitype-s01/real-aa-capture-tests.log`、[独立原始文件全字节读回](qa/lvds-multitype-s01/original-pcap-raw-independent-readback.json)。现有归档格式、LPD v3的40B头/409B记录均不改版。

## 5. 序号、同步与AA回归

UDP外层完整u32属于整个接收流；AA内层seq8属于AA科学帧本身。AB的pa0_seq与AC的更新计数是将来独立decoder的字段，本轮不将其当外层序号。真实来源不匹配在原始录制后拒绝；同来源、同接收session内目标语义变化会清理分片重组/依赖和恢复锚点，保留既有外层观察与科学去重，避免制造重复交付。来源/session变更构造新的ingress；目标地址改变本身不伪装成新的独立发送流。

`outer_sequence_gate`与科学接纳门禁独立。公共结构合法的AA、AB、AC及未知TYPE都先监测外层u32；AA随后即使因设备微秒越界或DATA251保留位失败，也不会让后续合法外层序号出现人为缺口。修正业务字段的同序号重发仍可由独立科学gate接纳。确认恢复使用原恢复gate，原生科学使用原生gate和流冲突gate，确认恢复先到不会占用后续完整原生帧的正常保存资格。

公共字节同步必须同时检查完整长度、EB90、SRC/DST、LEN及TYPE，保留有界搜索；不能仅凭EB90接纳。AA已有1bit/4bit恢复及独立质量门禁继续生效，不跨UDP借位、不补缺失位、不重复交付原生帧。AB/AC仅接收公共结构合法的完整原始帧，专用解码和恢复不在本阶段实施。

AA保留：10bit连续解包、每通道100点、500μs时间轴、`adc16=code10*64-32768`、`V=code10*0.01-5.12`、双通道图表、左键平移/滚轮缩放、60秒缓存、跟随/冻结、科学CSV/LPD v3及恢复质量审计。具体回归结果见第8节，未执行项目不援引旧阶段PASS替代。

## 6. 历史兼容和最小UI

PCAP与LVDSRAW通过 `HistoryReader`进入同一个 `UdpLvdsIngress`公共分流器，AB/AC不生成科学Entry、不计科学rejected。`HistoryStatus.receive_stats`保留类型累计统计，`auxiliary_frames`保存最近128个完整辅助/未知帧；全程公共帧字节及来源/处置进入history audit，原输入文件仍保留全部原始记录。最近128项是浏览缓存上限，不是全文件原始保留上限。audit写入错误使扫描显式失败，不把部分审计表示为完整。

LPD v3仍仅表示AA科学记录，不推断UDP数量、外层序号、原始CRC或不存在的AB/AC。历史处理不触发真实RS422发送。

UI仅增加必要计数和“AB/AC已接收，待解码”说明；详细状态放在原接收详情/更多区域，保持主图为AA双通道波形。没有AB频率、AC经纬度等虚构字段。后台历史与图表单元测试PASS；Windows原生GUI交互验证NOT_RUN。

本轮后台混合保存测试真实打开CSV/LPD写入器，并重读文件：20AA对应CSV 2000数据行、LPD 40+20×409=8220B；历史20+1+1输入在PCAP和LVDSRAW均为22记录、20原生AA、每通道2000点、AB/AC各1、科学解析错误0。上述测试在本轮完整workspace日志中PASS，不将后台测试等同Windows鼠标交互验收。

## 7. 真实原始数据独立扫描

本轮重新执行：

```powershell
python tools/qa/lvds_multitype_s01_verify.py capture --pcap D:/RUST/2026102401.pcapng --output docs/qa/lvds-multitype-s01/original-pcap-independent-audit.json
```

结果 PASS（退出码0）。该脚本复用既有Python PCAP读取器，以独立公共固定字段/精确长度判定原生帧，未调用Rust被测公共解析器，未改变输入字节。证据：[独立PCAP扫描](qa/lvds-multitype-s01/original-pcap-independent-audit.json)。

| 项目 | 实测结果 |
|---|---:|
| 原始PCAP文件字节数 | 172292 |
| UDP记录 | 467 |
| UDP payload总字节数 | 136830 |
| 原生精确公共帧AA | 237 |
| 原生精确公共帧AB / AC | 0 / 0 |
| 结构异常内层数据 | 230 |
| 内层长度269B / 268B | 466 / 1 |
| 完整正偏移byte-sync帧 | 0 |
| 既有AA诊断中的恢复候选 | 229 |

PCAP SHA256：`1bfe2ff956ad4e372bb15c2da04910c3d6a469b85d772ccbf83e4da4e48cca07`。既有AA诊断的候选是228个4bit和1个1bit错位，另有短帧；没有可据此声明的原生AB/AC。未验证的offset6字节7A/D5等不是有效TYPE统计。PCAP读取到精确EOF，无包解析诊断。

现存用户20帧HEX `lvds.txt` 为10780B，SHA256 `b5a2eee986fbe4f26da33da67bfeab17b8f9185f72ed0c6f81f232f3c2f15add`；它与真实PCAP仅用于AA回归。独立20+1+1夹具必须标为SYNTHETIC。真实FPGA AB/AC混合数据验收：BLOCKED，现有真实输入不含原生AB/AC。

本轮显式运行ignored真实输入专项：`real_capture_recovery_matches_independent_bit_boundary_audit` 与原独立逐包/逐位参考比较467UDP及93200个可预览code10，通过；质量237 NATIVE、229 RECOVERY_CANDIDATE、1 REJECTED，确认恢复0，未跨记录取位。`final55_original_txt_decoder_and_address_gate_contract` 重新读取原20帧TXT与独立reference CSV，生产decoder/Ingress/Controller的双通道每路2000点、地址正负例和实际科学保存合同通过。前者与真实全字节归档专项为2 passed，后者1 passed；日志分别为 `real-aa-capture-tests.log`、`real-aa-txt-tests.log`。

另外 `e7_user_capture_no_crc_science_roundtrip`、`e8_user_capture_scientific_csv_all_modes_readback`重跑2 passed：实际467UDP、466个完整269B内层、237原生科学帧、重复/冲突0；每通道23700点，LPD v3实际96973B且重读237帧；CSV与LPD+CSV两种模式均23700数据行，逐点独立code10/ADC/时间/顺序读回通过，日志 `real-aa-storage-tests.log`。最初旧oracle强制所有内层均269B，遇真实268B短帧失败；修复仅将实际短帧按长度拒绝纳入独立census，并明确断言生产decoder返回相应长度错误，未补齐或改写真实数据，未改变生产接纳逻辑。最终5项真实AA专项显式执行均PASS；它们仍在默认workspace运行中标ignored，其余派生GUI文件专项与CPU benchmark没有冒充已运行。

### SYNTHETIC 独立混合PCAP

新增 `tools/qa/lvds_multitype_s01_fixture.py`，只向明确含SYNTHETIC的新路径生成测试输入，拒绝覆盖既有文件，也不修改正式软件默认输入。生成文件 `target/qa/lvds-multitype-s01/SYNTHETIC-20AA-1AB-1AC.pcapng` 为8144B，SHA256 `6727eec810e191b1ca93431a433d1194eda13fe27098619719082b8ae562d9e8`；同目录 `.manifest.json`记录逐包类型、序号、payload hash和期望值。

该夹具采用标准Ethernet/IPv4/UDP和24B外层，源 `192.168.1.128:1534`、目标 `192.168.1.100:1534`。22个外层u32连续并跨低16位边界；20AA内层seq8为0..19、设备时间每50ms递增、DATA251=0；随后AB/AC DATA分别填260个B3/C4不透明标记。每帧原始尾字段固定1234，不重算CRC。AA通道分别重复 `[0,1,512,1023]` / `[1023,512,1,0]`，以40bit整数独立构造。

独立公共字段扫描PASS：22 UDP、6446B payload、22×269B、20AA/1AB/1AC、未知0、结构异常0、外层错误0；证据：[SYNTHETIC混合扫描](qa/lvds-multitype-s01/SYNTHETIC-mixed-pcap-independent-audit.json)。该证据内的 `existing_AA_diagnostic_quality_counts`是旧AA专用参考诊断，对2个辅助包给出REJECTED不代表公共分流器拒绝；公共类型计数以 `native_exact_public_frames`为准。

新非Mock Release二进制对同一夹具的 `--audit-pcap`验证PASS：22UDP、22完整帧、20AA/1AB/1AC、未知0、结构异常0、科学parsed/accepted均20、u32观察22且连续21、gap0；bytes_written/write_calls/commands_queued均0。日志 `qa/lvds-multitype-s01/release-SYNTHETIC-mixed-audit.log`。该CLI仅审计输入，不启动CSV/LPD写入，输出 `file_frames=NOT_RUN`保留实际边界。

## 8. 自动化验收矩阵

| 项目 | 状态 | 本轮证据/范围 |
|---|---|---|
| T1 公共结构、三类269B、CRC原字节保留 | PASS | `crates/langmuir-core/tests/lvds_multitype.rs` 10项公共模型/同步/gate测试 |
| T2 AA/AB/AC正确分流、辅助类型不进入科学链 | PASS | `synthetic_auxiliary_types_do_not_enter_science_or_count_as_parse_errors` |
| T3 SYNTHETIC 20AA+1AB+1AC、22原始记录、20科学记录 | PASS | `synthetic_twenty_plus_auxiliary_variants_preserve_aa_and_real_science_files`、`synthetic_mixed_raw_and_pcap_history_keep_auxiliaries_out_of_science_errors` |
| T4 缺AB或缺AC仍保留20AA | PASS | 上述variants覆盖20AA+AB、20AA+AC和辅助顺序互换 |
| T5 辅助包不同到达位置独立分流 | PASS | `synthetic_interleaved_auxiliaries_do_not_create_outer_sequence_gaps`、variants |
| T6 未知TYPE保留/计数、不产生科学点 | PASS | `synthetic_unknown_and_auxiliary_duplicates_conflicts_never_generate_science`、`synthetic_auxiliary_history_retains_duplicates_conflicts_and_unknown_types` |
| T7 真实写出/关闭/重开LVDSRAW，全部payload逐字节一致 | PASS | `synthetic_localhost_raw_roundtrip_preserves_all_types_and_rejected_payloads`：真实localhost Socket/文件，输入为SYNTHETIC |
| T8 外层u32连续性与AA seq8独立，无辅助包伪缺口 | PASS | interleaved、`synthetic_outer_continuity_is_independent_of_aa_business_validation` |
| T9 AA黄金/电压/图表/PCAP/LVDSRAW/CSV/LPD/同步/恢复/RS422/星务软件回归 | PASS | 完整workspace 420 passed；真实输入专项和原生GUI另列 |
| T10 长度/地址/未知/分片/TTL/重复冲突/来源/过载/写盘错误 | PASS | `synthetic_auxiliary_fragments_count_only_complete_frames_and_expire`、`synthetic_malformed_auxiliary_structure_never_counts_as_aa_decode_failure`、`synthetic_auxiliary_event_overload_reports_loss_without_changing_frame_counts`及既有存储边界测试 |
| 真实PCAP重新独立扫描 | PASS | 467 UDP，237原生AA，AB/AC各0；见第7节 |
| 交付工具SYNTHETIC自检 | PASS | 先3 FAIL（缺少工具），实现后3 PASS；涵盖动态CRT、错误架构、ZIP字节不匹配和公共结构 |
| 全部Python QA测试 | PASS | 49 passed，0 failed；`qa/lvds-multitype-s01/python-qa-suite.log` |
| 真实原PCAP归档/恢复参考专项 | PASS | 显式运行2 passed；467UDP、136830B全字节与metadata；93200code10参考比对 |
| 原20帧TXT生产链专项 | PASS | 显式运行1 passed；`real-aa-txt-tests.log` |
| 真实原PCAP额外科学LPD/CSV专项 | PASS | 显式运行2 passed；237科学帧、每通道23700点、96973B LPD与23700行CSV重读通过，旧oracle短帧断言已修正 |
| SYNTHETIC独立20+1+1 PCAP扫描 | PASS | 22UDP/20AA/1AB/1AC，结构/未知/外层错误0；另见第7节 |
| 新非Mock Release混合PCAP CLI | PASS | 22UDP/22完整帧/20AA/1AB/1AC、unknown/structure/gap均0；科学20，未发真实控制或生成科学文件 |
| `cargo fmt --all -- --check` | PASS | 退出码0，`qa/lvds-multitype-s01/cargo-fmt.log` |
| `cargo check --workspace --locked` | PASS | 退出码0，`qa/lvds-multitype-s01/cargo-check.log` |
| `cargo test --workspace --features langmuir-desktop/mock --locked` | PASS | 420 passed、0 failed、7 ignored，`qa/lvds-multitype-s01/cargo-test.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | 退出码0，`qa/lvds-multitype-s01/cargo-clippy.log` |
| Windows x64 MSVC静态CRT非Mock Release | PASS | 最新源码release构建退出码0、41.50s；静态CRT/非Mock另由构建参数与CLI guard证明 |
| 新交付包PE/imports/ZIP/sha校验 | PASS | `windows-package-verification.json`；PE32+ AMD64，无动态CRT导入，ZIP内容/CRC/sha一致 |
| Windows原生GUI交互与真实硬件联调 | NOT_RUN | 自动化后台检查不能代替该项 |
| 派生PCAP与本轮原生GUI实际保存文件专项 | NOT_RUN | 不复用过往GUI文件充当本轮新构建验证 |
| 恢复CPU性能benchmark | NOT_RUN | 默认ignored性能项未作为本轮正确性验收 |
| 真实FPGA AB/AC混合输入验收 | BLOCKED | 缺少含原生AB/AC的真实抓包或设备输入 |

本轮QA工具自检命令：`python -m unittest discover -s tools/qa -p 'test_lvds_multitype_s01_verify.py' -v`，3 passed，0 failed。该SYNTHETIC PE夹具仅测试包核验器，绝非可交付程序。

四项最终Rust检查的命令、日志和退出码均记录于 [verification-summary.json](qa/lvds-multitype-s01/verification-summary.json)，全部为0；最终workspace为420 passed、0 failed、7 ignored。Mock feature仅用于要求的测试命令，发布构建单独使用 `--no-default-features`。

协议变更对应的旧断言已按新语义调整：原先把AB fixture记作AA错误的测试仍保留相同原始fixture，改为要求AB有效且科学解析错误不增加；原“仅Accepted AA推进外层序号”的断言改为公共结构合法即可观察外层u32，科学错误与序号观察分别断言。没有通过改造输入数据、关闭地址/长度/业务检查或放宽恢复质量门禁掩盖失败。

实施过程回归发现确认恢复先到会吞掉后续原生科学帧的实际缺陷，已通过恢复与原生接纳隔离修复；另有旧单类型断言与计数口径差异，已按公共协议更新，最终验证均通过。来源过滤原有 `udp_packets`断言保持来源匹配含义，UI总UDP展示另外加来源拒绝数。最终完整测试较既有391 passed基线增加29项；默认7 ignored未算入420 passed，其中本轮另外显式运行的5项真实AA测试按独立日志记录。

## 9. Windows非Mock构建与包校验

以最新 `docs/WINDOWS_BUILD.md` 为依据，使用：

```powershell
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked
```

本轮最新源码构建PASS，退出码0，日志 `qa/lvds-multitype-s01/windows-release-build.log`，41.50秒。新交付目录 `target/delivery/lvds-multitype-s01-20261118`，保留旧阶段交付包。发布采用真实UDP/RS422后端，不启用mock，不启动自动合成数据、不自动连接/发控制指令。旧交付物和旧阶段报告不作为本轮构建证据。

| 新产物 | 字节数 | SHA256 |
|---|---:|---|
| `LangmuierProbeGroundControl.exe` | 10399232 | `eab542618f88a4a3fa21b34b23a2b89ddfdfef665075aac91b32e540c6566f70` |
| `LangmuierProbeGroundControl-windows-x64.zip` | 4292992 | `c41c2d28060218a5ec5345190a12f46a60e06c2cfa22cd4defd2d0d25b17f8df` |
| 包内README对应 `docs/WINDOWS_README.md` | 6192 | `71bf824f84703c9c64fef73466e208a2db374bceaccaf7293b1e4830c099d440` |

新二进制真实AA `--audit-pcap`运行读取467包并交付237科学帧，未启动科学文件保存；`file_frames=NOT_RUN`按实际功能边界保留，不伪称CLI已写科学文件。`--mock`运行被正式二进制拒绝，日志 `release-nonmock-guard.log`。这些属于Windows二进制CLI验证，不能代替原生GUI交互或真实硬件联调。

新增包验证器使用参数指定最终产物与新的JSON证据路径：

```powershell
python tools/qa/lvds_multitype_s01_verify.py package --exe <新EXE> --package <新ZIP> --readme docs/WINDOWS_README.md --output <新的包校验JSON>
```

验证PE AMD64/PE32+、导入表无动态MSVC CRT、ZIP精确两项及CRC、EXE/README逐字节一致，并记录SHA256。脚本拒绝覆盖已有输出。Cargo feature和原生GUI状态由单独的构建/运行证据证明，包字节扫描不伪称已验证这些行为。

## 10. Git修改清单与边界

新增本轮资料：

- `docs/LVDS_MULTITYPE_S01_ACCEPTANCE.md`
- `docs/qa/lvds-multitype-s01/original-pcap-independent-audit.json`
- `docs/qa/lvds-multitype-s01/python-qa-suite.log`
- `tools/qa/lvds_multitype_s01_verify.py`
- `tools/qa/lvds_multitype_s01_fixture.py`
- `tools/qa/test_lvds_multitype_s01_verify.py`

本轮Rust/UI/测试变更：

| 文件 | 本轮职责 |
|---|---|
| `crates/langmuir-core/src/lvds_common.rs`（新增） | 公共帧、TYPE枚举、固定结构解析及有界唯一公共帧搜索 |
| `crates/langmuir-core/src/udp_lvds_frame_gate.rs`（新增） | 不调用AA decoder的外层完整u32/原始字节重复冲突门禁 |
| `crates/langmuir-core/src/lib.rs` | 导出新公共接口；保留已有星务导出 |
| `crates/langmuir-core/src/lvds_recovery.rs`、`lvds_recovery_tests.rs` | 公共字节同步兼容；阻止辅助包进入AA位恢复；保留原AA恢复门禁 |
| `crates/langmuir-core/src/udp_lvds_sequence.rs` | 外层流观察语义注释与接口说明 |
| `crates/langmuir-core/tests/lvds_multitype.rs`（新增） | 独立SYNTHETIC公共字段/类型/同步/重复冲突/TTL/容量测试 |
| `apps/desktop/src/backend.rs`、`bridge.rs` | 生产分流、类型事件/来源时间、独立外层观察、统计/有界队列；保留原星务改动 |
| `apps/desktop/src/backend_history.rs`、`backend_raw_storage.rs` | 共用分流器、辅助帧累计/最近缓存与全程审计、清理已消费事件 |
| `apps/desktop/src/app.rs`、`app_history.rs` | 原详情区域增加接收统计与待解码说明，历史统计；保留原星务界面 |
| `apps/desktop/src/backend_multitype_tests.rs`（新增） | 三类型/混合序号/实际CSV与LPD/分片/异常与事件过载测试 |
| `apps/desktop/src/backend_multitype_history_tests.rs`（新增） | PCAP/LVDSRAW混合历史与真实localhost原始文件读回 |
| `apps/desktop/src/backend_capture_history_tests.rs`、`backend_e2e_tests.rs`、`backend_scientific_csv_tests.rs` | 按公共类型语义更新原fixture断言，保持AA恢复/保存回归；独立e2e oracle将实际268B短帧按长度拒绝和计数，并明确断言生产长度错误，不改原始输入；e7/e8最终2 PASS |
| `tools/qa/lvds_multitype_s01_fixture.py`、`lvds_multitype_s01_verify.py`、自检 | 独立SYNTHETIC PCAP构造、公共扫描、PE/ZIP交付核验；输入只读、输出拒覆盖 |
| `docs/WINDOWS_README.md` | 三类型状态与原始保存交付说明；保留原星务说明 |
| `docs/superpowers/plans/2026-11-18-lvds-multitype-s01.md`（新增） | 本轮实施计划与验证任务 |

完整工作区diff还包含执行前已有星务模拟器源文件/测试、transport发送修改、星务报告与协议DOC替换；这些不作为本轮新增修改计算。已有用户修改、DOC替换状态、协议工作簿和原始数据保留，不自动Git提交。新构建和包校验资料见第9节，旧阶段发布包保留。

## 11. 尚未完成的真实设备项目

真实FPGA发送AB/AC、真实混合调度、辅助包丢弃策略、设备来源切换、长期真实网络负载均未取得本轮实测输入。软件SYNTHETIC混合流通过也不能将这些项目改为PASS。后续取得混合抓包后，应保持原始字节，重新执行公共分流/统计/原始归档读回，再进行AB独立波形decoder阶段。
