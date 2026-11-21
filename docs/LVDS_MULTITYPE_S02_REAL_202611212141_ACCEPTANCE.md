# LVDS-MULTITYPE-S02-REAL-02：真实多模式抓包验收报告

日期：2026-11-21。项目：D:\RUST\langmuier。软件接收、解码、自动化绘图、保存和历史链路验收通过；原生 Windows GUI 操作和真实硬件联调为 **NOT_RUN**。本轮只完成 S02 范围，没有进入 S03。

## 1. 输入与保护

正式输入：[202611212141.pcapng](../202611212141.pcapng)，来源为工作区原件；Downloads 下同名副本同时保留。两份均为 1,416,044 字节，SHA256：

`9a5d9a73aa46fb2fb24962b2e2b7d4081646477ae5d1f62ee2ee46c375b53907`

两份文件结束时大小、哈希、mtime_ns 均与起始基线一致。三个旧版交付 EXE 哈希不变，原有进程 43960、44796 仍运行于各自旧交付路径。旧报告、协议表、冻结参考及 RS422 模拟器源码保留；没有提交、推送、重置工作区、连接硬件或发送控制命令。

证据：[baseline.json](qa/lvds-multitype-s02-real-02/baseline.json)、[保护与源码差异](qa/lvds-multitype-s02-real-02/protection-and-source-delta.json)。工作区原有未提交修改保留，git-status-before/after 与 git-diff-stat-before/after 均记录于本轮 QA 目录。

## 2. 独立原始审计

独立 Python 读取 PCAPNG、Ethernet、IPv4、UDP、24B FPGA 外层与 269B LVDS 内层，没有调用 Rust 被测业务解码器。接口 if_tsresol=9，按纳秒解释捕获时间，跨度 174.3921073 秒。

|项目|实际结果|
|---|---:|
|全部 UDP|3873|
|FPGA UDP|3839|
|其它 UDP|34|
|AA / AB / AC|3489 / 175 / 175|
|FPGA Payload / 内层|293B / 269B|
|外层 u32|9403–13241|
|相邻序号步长为1|3838次|
|外层缺口、结构异常|0|
|离线参考 CRC 匹配|3839 / 3839|

FPGA 来源为 192.168.1.128:1534，目的为 192.168.1.100:1534。其它 34 条均为 8B 广播，目的 192.168.1.255:1534；来源 192.168.1.100 的端口 1534、59325、54740 分别为 12、11、11 条。不能将其当作 FPGA 错误帧。

参考 CRC16-CCITT-FALSE 覆盖 DATA260；生产仍为 **CRC_NOT_CHECKED**，没有新增 CRC 门禁。协议工作簿 40138B，SHA256 为 `916a0f154ad71e2325a5484cb1345de0fbe18319133282c83c7b73ea3368c11f`。

证据：[独立审计 JSON](qa/lvds-multitype-s02-real-02/independent-pcap-audit.json)、[文本审计](qa/lvds-multitype-s02-real-02/independent-pcap-audit.txt)、[完整逐点 manifest](qa/lvds-multitype-s02-real-02/independent-manifest.json)、[工作簿依据](qa/lvds-multitype-s02-real-02/independent-workbook-evidence.json)。冻结 manifest SHA256 为 `b7bd25bdc1cfa875ee4defc5ba257ccc555ab698e216ee927d58d90fa4d3ff89`。

## 3. AA 科学数据

AA 3489 帧，每通道 348900 点，采样间隔 500μs。仍采用原 10bit 解码、ADC 重建及 V=code10×0.01−5.12。

|通道|code10 范围|电压范围|码值分布|
|---|---|---|---|
|CH1|510–514|−0.02至0.02V|513×244094、512×104804、510×1、514×1|
|CH2|509–513|−0.03至0.01V|512×346771、511×1363、513×765、509×1|

AA seq8 全程连续，不受 AB/AC 插入影响。设备帧起点差值主要为 50000μs（2637次），另有 49999/50001μs（424/423次），49998、50002、50005、49995μs 各1次。这些是原包字段的实际微小抖动，没有改写为理想时刻。

科学 CSV 全部 11 列和 LPD v3 全部记录均经独立读回，双通道 697800 个码值、ADC 重建、seq8、采样时间和顺序一致。自动化图表检查同样核对实际 V 值及双通道数据，不将 AB/AC 计入科学采样。

## 4. AB 十一个真实阶段

下表由独立读取每个 AB 的工作模式、设定频率及电压后连续分组生成。索引从0开始，阶段数量和频率没有硬编码进正式 UI。

|阶段|AB 索引|模式与设定|帧数|实际 Hz|间隔 μs|原始 wave 范围 mV|
|---|---|---|---:|---:|---:|---|
|1|0–20|三角波 0Hz|21|0.000|0|0|
|2|21–33|三角波 1Hz|13|0.931|10800|1655–1715|
|3|34–46|三角波 50Hz|13|49.918|200|1655–1716|
|4|47–59|三角波 100Hz|13|99.930|200|1655–1716|
|5|60–69|正弦波 100Hz|10|99.930|200|1654–1716|
|6|70–80|正弦波 50Hz|11|49.918|200|1654–1716|
|7|81–113|正弦波 1Hz|33|0.931|10800|1655–1716|
|8|114–131|DC 1.000V 设定|18|0.000|0|0|
|9|132–148|DC 3.000V 设定|17|0.000|0|0|
|10|149–164|DC 4.500V 设定|16|0.000|0|0|
|11|165–174|DC 0.000V 设定|10|0.000|0|0|

93 帧具有逐点时间，9300 点均按 latest−(99−i)×interval_us 倒推，按 wave_mV/1000 换算 V；wave[99] 对应最新时间。实际频率没有被设定值覆盖。时间窗跨度为 1069200 或 19800μs，存在45组相邻窗口重叠，因此逐帧查看，不无条件拼接。

另外82帧为21个三角0Hz和61个DC状态，全部 interval_us=0、f_act_mHz=0、points=100、wave全零。软件接纳合法公共帧，保留全部配置、原始状态、PA0和100个零槽；不计算100个相同时间点，不将占位零值或DC设定画成实测线，也不计为丢包。

PA0从183至101，在 AB 索引72→73发生正常255→0回绕。175个 sci_frame_seq 全部等于随后 AA 的 seq8，均不等于前一个 AA。该稳定行为与协议“最近一帧”的文字有差异，列为厂家待确认；不拒收、不减1、不改字段。

证据：[11阶段完整表](qa/lvds-multitype-s02-real-02/independent-AB-stages.csv)、[175行 AB/前后AA关联表](qa/lvds-multitype-s02-real-02/independent-AB-AA-association.csv)。

## 5. 确认缺陷、最小修复及 CSV 兼容

旧 EXE 对本轮原件解码 AA3489、AB175、AC175，AB错误为0。合法零状态帧原本没有被拒收；确切缺陷是旧 sample_time_us 在 interval=0 时返回 latest−0。首个真实状态 AB（raw ID5、外层9407、PA0183）的100个槽位均得到367141944μs，CSV重复写出同一伪采样时间。

核心修复增加 has_sample_timing 语义：interval>0 才有逐点时间；interval=0 时所有 sample_time_us 返回 None。原有字段校验、正间隔窗口下溢拒收和 CRC 策略保持不变。UI不生成无时间曲线或游标点值，明确显示“当前无有效激励波形”“无可用逐点采样时间”；DC电压按三位小数V显示为设定值。

CSV v2保留原22列名称、顺序及有时间帧数值语义，末尾追加 interval_us、points、waveform_status，共25列。无时间帧仍写100行，sample_time_us为空，状态为NO_SAMPLE_TIME；wave_mV=0与wave_V=0.000仅保留原始数值及单位换算，不表示实测0V。每份文件只有一个表头，全部175帧保存17500行。

**固定22列或强制将采样时间解析为整数的旧读取器需要适配。** 旧v1文件保留，v2黄金400行的前22列与旧v1完全一致。完整表头与读取规则见 [AB_EXCITATION_CSV_V2.md](AB_EXCITATION_CSV_V2.md)。

修复证据：

- 核心/CSV真实失败：core-csv-isolated-red.log、csv-isolated-red.log；修复后 core-csv-green-final.log 为180 passed、0 failed、1 ignored，核心/存储Clippy退出0。
- core-interval-zero-red.log、csv-interval-zero-red.log 曾因共享构建排队实际运行于修复之后，内容为GREEN，不能作为失败证据。
- UI真实失败：ui-zero-red-j1.log。核心修复后旧界面缺少明确状态提示；修复后 ui-zero-green.log 通过。旧伪时间问题由核心失败用例证明。
- 第一次核心GREEN有一处新增断言误放到正间隔测试，已修正测试并保留失败日志。
- UI全模式首轮错误假设60秒边界包含完整帧；修正测试按record_id/样本时间裁剪后全量通过，未据此改变生产逻辑。
- 新增测试三处空数组断言触发Clippy，已改成等价assert_eq并复验。

汇总见 [core-csv-fix-evidence.json](qa/lvds-multitype-s02-real-02/core-csv-fix-evidence.json)、[独立黄金检查](qa/lvds-multitype-s02-real-02/independent-synthetic-v2-golden-check.log)。部分 PowerShell 日志为UTF-16，请按BOM读取。

## 6. AC 公共接收

175帧AC均保留并单独统计，260B DATA全零。独立审计 gnss_valid、att_valid及两个更新计数均为0。AC不进入AA/AB曲线，不生成虚构位置、姿态或轨道。RAW完整保存这些报文；本轮没有实现或宣称完成AC业务decoder。

## 7. 正常速率生产保存与全量独立读回

非Mock专项通过生产 PcapReplay/Controller，在首帧前完成 AA LPD+CSV、AB CSV与RawArchive初始化。捕获跨度174.3921073秒，实际正常节奏回放至所有writer关闭为174.5007969秒；含后续全历史检查总用时401.485秒。PID35268及二进制哈希见 [运行身份](qa/lvds-multitype-s02-real-02/backend-run-01-identity.json)。

实际观察全部UDP3873条，来源过滤后接纳FPGA3839条、拒绝其它来源34条。RawArchive在过滤前保存全部3873条，没有修改过滤规则。AA入队/写入3489，AB入队/写入175，RAW收到/入队/写入3873，业务丢失和写入错误均0，全部closed。另实际调用Controller::start_raw_recording完整导出PCAP，同样3873条。

测试故意不消费显示事件，显示队列保留8项、淘汰3656项，业务保存仍全部完整。这是展示队列丢弃，不能当作UDP丢包。约1秒一次的进程采样覆盖正常回放174次，CPU累计约0.578秒，峰值工作集90,464,256B、私有内存73,527,296B；仅代表测试进程，不能作为原生GUI性能结论。

|生成文件|记录/数据行|大小B|独立检查|
|---|---:|---:|---|
|capture.lvdsraw|3873|1,512,619|全部Payload、源/目的、纳秒时间、ID、来源标记、头、END与精确EOF|
|ui-entry-full.lvdsraw|3873|1,512,619|正式Controller录制入口，完整同范围读回|
|science.csv|348900|21,205,769|全部11列、双通道码值/ADC、seq8、索引/时间、顺序、CRC_NOT_CHECKED|
|science.lpd|3489|1,427,041|v3头40B、记录409B，697800个通道值、序号/时间及EOF|
|excitation.csv|17500|3,104,158|全部25列，9300有时间/8200空时间，完整EOF|
|history-pcap-ab.csv|17500|3,524,158|PCAP历史正式导出，全部25列一致|
|history-raw-ab.csv|17500|3,226,658|RAW历史正式导出，全部25列一致|

三个AB CSV长度不同源于session字符串不同。独立期望取自生产会话元数据、PCAP规范路径或RAW文件头，没有从待测CSV自取期望。

独立Python重新从原PCAP计算并核对冻结manifest，再完整比较所有文件，结论不只依据大小或Rust侧数量断言。证据：[生产完整结果](../target/qa/lvds-multitype-s02-real-02/backend-run-01/production-summary.json)、[运行日志](qa/lvds-multitype-s02-real-02/backend-run-01.log)、[独立全量读回](qa/lvds-multitype-s02-real-02/independent-full-storage-readback-real02-run01.json)、[读回日志](qa/lvds-multitype-s02-real-02/independent-full-storage-readback-real02-run01.log)、[全部输出大小/哈希](qa/lvds-multitype-s02-real-02/independent-final-integrity-readback.json)、[性能采样](qa/lvds-multitype-s02-real-02/performance-summary.json)。

## 8. 历史、图表与GUI证据边界

PCAPNG、生成的RAW和LPD均通过正式历史工作线程重新打开。每种输入按四个每段≤1000帧的窗口核对全部348900点/通道，避免超过60秒缓存；PCAP与RAW各检查全部175个AB，包括指定帧和前后导航。LPD仅含AA，不宣称包含AB。

真实PCAP→生产HistoryReader→egui专项2 passed、0 failed。AA按三个60秒窗口核对全部697800个双通道电压值；AB逐175帧核对17500原始槽、全部字段及11阶段标签，93帧9300个有时间绘图点，82帧无曲线及无时间提示。验证指定帧、egui指针点击上一/下一帧、空↔有时间状态切换、冻结与恢复。默认UI回归102 passed、0 failed、3 ignored，包括既有布局、图表交互和固定面板。

“打开历史”“科学波形AA”“激励波形AB”“AB CSV”入口保持直接可见，版本更新为S02-REAL-02。证据：[UI专项日志](qa/lvds-multitype-s02-real-02/ui-multimode-final.log)、[逐帧及阶段索引/标签](../target/qa/lvds-multitype-s02-real-02/ui-multimode-20261121-230933-545.json)。

**原生Windows操作与截图没有执行。** 当前会话没有可调用的node_repl/@oai/sky Windows操作工具；依据 [computer-use/SKILL.md](C:/Users/Administrator/.codex/plugins/cache/openai-bundled/computer-use/26.928.31416/skills/computer-use/SKILL.md) 的“Import it directly by package name from the JavaScript session.”初始化要求，没有以自建控制程序代替。未用绘图图片或egui测试冒充原生截图。11阶段JSON内native_gui_screenshot均为NOT_RUN。

因此原生窗口中的鼠标平移、滚轮缩放、冻结/跟随、文件选择及保存按钮实际操作仍为NOT_RUN；以下AA/AB绘图和模式切换的PASS仅限已执行的自动化软件范围。

## 9. 旧真实输入与最终回归

本轮重新执行2026112101正常速率非Mock生产专项1通过，旧真实egui专项1通过，覆盖1136 AA与57 AB。新RAW1262、AA CSV113600、LPD1136、AB CSV v2 5700行均独立全字段读回通过。旧专项没有产生历史AB CSV，明确不计入旧输入导出验收；新输入两份历史导出已完整验证。

旧专项第一次启动误指向工作区不存在的2026112101，在回放前报NotFound；日志保留。第二次使用冻结manifest中的Downloads原路径成功，没有生成替代抓包。旧UI测试只增加可选输出目录环境变量，将新证据写到本轮目录。

证据：[旧输入独立读回](qa/lvds-multitype-s02-real-02/independent-previous-real-csv-v2-readback-run02.json)、[旧生产重跑](qa/lvds-multitype-s02-real-02/previous-real-backend-02.log)、[旧UI重跑](qa/lvds-multitype-s02-real-02/previous-real-ui.log)。

另重跑2026102401和lvds.txt的5项既有真实专项，5 passed、0 failed。实际使用工作区172292B的2026102401原件，SHA256为 `1bfe2ff956ad4e372bb15c2da04910c3d6a469b85d772ccbf83e4da4e48cca07`，与旧基线一致。检查467条UDP、237原生AA、229候选、1拒绝，原始归档467条/183750B及EOF；TXT20帧/4000个code10继续通过。全部新输出限定本轮目录，输入和独立参考身份不变，见 [旧专项结果](qa/lvds-multitype-s02-real-02/legacy-aa-regression-results.json)。

|最终检查|结果|本轮日志|
|---|---|---|
|cargo fmt --all -- --check|PASS|final-fmt.log|
|cargo check --workspace --locked|PASS|final-check.log|
|cargo test --workspace --features langmuir-desktop/mock --locked -j1|474 passed / 0 failed / 12 ignored|final-test.log|
|cargo clippy --workspace --all-targets --locked -- -D warnings|PASS|final-clippy.log|
|新真实多模式专项|后端1 + egui2，0失败|backend-run-01.log / ui-multimode-final.log|
|旧2026112101专项|后端1 + egui1，0失败|previous-real-backend-02.log / previous-real-ui.log|
|2026102401与TXT专项|5通过，0失败|legacy-aa-regression-results.json|
|最终Release复核|交付EXE与最终构建字节相同|final-release-confirm.log|

本轮开始实际默认基线为470/0/9，任务文字469/0/7是更早版本。本轮新增4个默认回归和3个默认ignored专项，最终474/0/12。10项真实输入ignored测试已单独执行通过，不将重复次数加到默认独立测试数。原生GUI派生文件专项和CPU benchmark未执行。

完整命令与耗时见 [final-verification-commands.json](qa/lvds-multitype-s02-real-02/final-verification-commands.json)。

## 10. 单EXE交付与启动身份

交付：[LangmuierProbeGroundControl.exe](../target/delivery/lvds-multitype-s02-real-02-20261121/LangmuierProbeGroundControl.exe)

版本：S02-REAL-02 (20261121)。大小：10,778,112B。SHA256：

`c0f0cbdf7dd515de182ec96f0cd7b3f9a43cf2932ca005f48f49a2ba114cb38c`

构建使用 RUSTFLAGS=-C target-feature=+crt-static，命令为：

`cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked -j1`

PE32+ AMD64验证通过，无动态CRT导入。使用真实UDP/RS422生产后端，未启用mock。仅交付EXE，没有创建ZIP。

实际新EXE只读 --audit-pcap 启动PID27904，CWD为工作区，退出0。FPGA3839解码AA3489/AB175/AC175，业务错误0，CRC disabled。CLI预筛来源，因此source_rejected=0不同于正常生产回放的34；type_events_dropped=222来自350个辅助展示事件超过128容量，不能当作丢包。CLI明确file_frames=NOT_RUN，保存完整性由实际生产回放另外证明。

构建、PE、路径、哈希和PID证据：[release-build-command.json](qa/lvds-multitype-s02-real-02/release-build-command.json)、[release-identity.json](qa/lvds-multitype-s02-real-02/release-identity.json)、[EXE真实抓包审计](qa/lvds-multitype-s02-real-02/release-pcap-audit.log)。

离线入口调用链复核没有自动连接、RS422发送或开启广播，见 [代码复核](qa/lvds-multitype-s02-real-02/final-code-review.md)。原生窗口交互认证未执行；Release PASS仅涵盖构建、PE、实际CLI启动及生产后端验证。

## 11. 本轮源码差异

生产改动限于AB核心无采样时间语义、CSV状态列、AB无波形提示/DC设定格式及版本。app.rs/backend.rs仅增加新测试模块；新增多模式后端/egui测试、原包零状态fixture、v2黄金CSV和独立QA工具；旧UI测试只增加输出目录选项。

没有修改AA算法、24B外层、269B公共帧、RAW/LPDv3格式、CRC策略、RS422模拟器或AC业务decoder。与开始171项基线相比，160项SHA相同，11项必要修改见 [本轮前后身份](qa/lvds-multitype-s02-real-02/protection-and-source-delta.json)。

cumulative-worktree-tracked.patch是相对HEAD的累计已跟踪差异，包含此前未提交工作，不是本轮独立补丁；此前未跟踪AB文件的本轮变化通过起止SHA、源代码及失败/通过回归记录区分。没有自动Git提交。

## 12. 独立裁决

|项目|状态|已执行范围及限制|
|---|---|---|
|LVDS_PUBLIC_FRAME|PASS|3839 FPGA正确分流，34其它UDP独立统计，结构及u32缺口为0|
|AA_SCIENCE_DECODE|PASS|3489帧，双通道全部码值及时间|
|AA_SCIENCE_PLOT|PASS|生产历史→egui全量自动化；原生手势操作NOT_RUN|
|AB_EXCITATION_DECODE|PASS|175帧全部字段及槽位，含82无时间状态|
|AB_MODE_SWITCH|PASS|11阶段自动化逐帧、导航与状态切换；原生操作NOT_RUN|
|AB_ZERO_STATE_HANDLING|PASS|无伪时间/曲线，保留槽位及DC设定|
|AB_EXCITATION_PLOT|PASS|9300有时间点及82空状态自动化；原生窗口NOT_RUN|
|AC_PUBLIC_RECEIVE|PASS|175公共帧与原始字节；没有S03业务decoder|
|RAW_ARCHIVE|PASS|两次3873记录归档，全Payload/元数据/END/EOF独立读回|
|AA_CSV_LPD_STORAGE|PASS|348900 CSV行、3489 LPD记录全量读回|
|AB_CSV_STORAGE|PASS|17500行及两种历史导出，25字段完整核对|
|HISTORY_REPLAY|PASS|PCAP/RAW/LPD正式线程，全AA分窗及175AB检查|
|WINDOWS_NATIVE_GUI|NOT_RUN|无可调用Windows操作工具，无原生操作或截图|
|WINDOWS_NONMOCK_RELEASE|PASS|x64静态CRT非Mock构建、PE及实际CLI启动，不含原生交互|
|REAL_HARDWARE_LIVE|NOT_RUN|未连接真实硬件、未发送控制或广播|

机器可读结果见 [verification-summary.json](qa/lvds-multitype-s02-real-02/verification-summary.json)。厂家仍需确认AB sci_frame_seq关联后续AA的协议文字。原生Windows操作及11阶段截图、真实硬件在线联调仍未验收，不能以软件PASS或旧报告替代。本轮在S02范围停止。
