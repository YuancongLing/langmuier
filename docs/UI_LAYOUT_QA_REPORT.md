# 桌面 UI 布局与响应式验收报告

日期：2026-10-16

本轮范围是桌面 UI 的布局、遮挡、滚动、窗口尺寸和 UI 缩放。工作区中已有的 RS422 协议修复、LVDS 数据解码、UDP 重组和数据存储改动均予以保留；本轮没有连接真实设备，也没有发送真实控制指令。

## 1. 原始问题与根因

| 模块 | 复现条件 | 根因 | 修复 |
| --- | --- | --- | --- |
| 主窗口 | 拖动窗口或使用小屏幕 | `main.rs` 将窗口固定为 1360×860 且禁止调整大小 | 允许调整大小，最小逻辑尺寸 800×600，移除最大尺寸锁定 |
| 左侧指令区 | 422 指令区内容变多或窗口变窄 | 根布局和侧栏使用固定尺寸；指令编辑器固定为 132px，高度不足时父容器裁切底部按钮 | 侧栏采用 260–420px 可调整范围；保留垂直滚动；移除指令编辑器固定高度 |
| 指令操作按钮 | 左侧栏最小宽度、原始编辑模式 | 重置、重算 SUM、发送指令在单行布局中超过可用宽度 | 改为 `horizontal_wrapped`，窄栏自动换行 |
| 遥测轮询设置 | 左侧栏窄宽 | 输入框与预设按钮单行排列导致预设被裁切 | 轮询输入和预设改为可换行布局 |
| 中央操作区 | 小窗口或高 UI 缩放 | 中央区域只有垂直滚动；极窄时 LVDS 工具栏可能需要横向空间 | 中央内容区使用双向滚动，保证极端组合下控件仍可达 |
| 右侧遥测区 | 长十六进制字段或较窄右栏 | Grid 最小列宽和长文本会撑宽或挤压相邻数据 | 列宽按可用宽度计算；值和状态文本截断，悬停显示完整内容；右栏可调整并垂直滚动 |
| 顶部与工具栏 | 窄窗口、字体放大 | 单行布局和固定高度容易造成标题、状态和按钮裁切 | 使用换行布局；顶部高度允许 60–180px 范围内自适应 |
| 图表与日志 | 窗口尺寸变化、日志增长 | 图表和底部区域固定尺寸，日志可能挤压主内容 | 图表高度按可用高度计算；日志面板可在 88–220px 范围调整，内容保持有限显示 |

## 2. 修复记录

### `apps/desktop/src/main.rs`

- `ViewportBuilder` 改为可调整窗口。
- 初始窗口仍为 1360×860，最小逻辑尺寸改为 800×600。
- 移除固定最大尺寸，避免宽屏空间被锁死。

### `apps/desktop/src/app.rs`

- 删除根布局必须等于 1360×860 的断言。
- 上一阶段左、右侧栏使用 260–420px 和 240–380px 的可调整范围并保留垂直滚动；固定停靠专项在第 11 节改为 360/320 logical points，并为小窗口提供 300/280 紧凑尺寸。
- 底部日志面板使用 88–220px 的可调整高度。
- 中央内容区使用双向滚动；工具栏、报警行、串口状态和图表控制栏支持换行。
- 删除指令编辑区固定 132px 高度，确保表单和发送按钮能随内容增长。
- 右侧遥测 Grid 使用自适应列宽；长值和状态文本提供截断与悬停全文。
- 增加 80%、90%、100%、110%、125%、150% 的 UI 缩放选择，使用 egui 的 `Context::zoom_factor`，保留操作系统 Retina/高 DPI 的原生像素比例。
- 增加缩放范围和响应式尺寸单元测试。
- 保留原始字节编辑和 SUM 重算能力；类型化指令预览继续直接使用实际编码结果。

本轮没有修改主题、GUI 技术栈、LVDS 科学数据解码、UDP 重组或存储逻辑。工作区中 `bridge.rs`、`mock.rs`、`langmuir-core`、CLI 和协议文档的未提交变更属于前一轮 RS422 修复，未在本轮回退。

## 3. 实际 GUI 启动

已执行：

```text
cargo run -p langmuir-desktop --features mock -- --mock
```

程序成功编译并进入 `target/debug/langmuir-desktop --mock`。运行过程中只出现 macOS 系统服务的 `Connection invalid` 日志，没有 Rust panic；随后为结束验证而手动停止进程。Mock 模式没有连接真实串口或真实 LVDS 设备。

当前执行环境不能进行可视化验收：

- 没有可用的 `DISPLAY` 或 `WAYLAND_DISPLAY`；
- 环境没有 `xvfb-run`；
- `screencapture` 返回 `could not create image from display`；
- `osascript` 返回 `not allowed assistive access`。

因此不能把代码审查或启动日志当作截图验收证据。

## 4. 分辨率与 UI 缩放矩阵

下表记录了要求的场景。由于当前环境没有可用 GUI 显示器，所有需要观察控件位置、点击和截图的场景均为 `BLOCKED`，没有虚构截图或把未执行的场景标为 PASS。

| 窗口逻辑尺寸 | UI 缩放 | 平台/证据 | 状态 |
| --- | ---: | --- | --- |
| 800×600 | 100% | macOS Mock 启动可编译；无显示器截图 | BLOCKED |
| 1024×768 | 100% | 无显示器截图与交互 | BLOCKED |
| 1280×720 | 100% | 无显示器截图与交互 | BLOCKED |
| 1440×900 | 100% | 无显示器截图与交互 | BLOCKED |
| 1920×1080 | 100% | 无显示器截图与交互 | BLOCKED |
| 2560×1440 | 100% | 无显示器截图与交互 | BLOCKED |
| 3840×2160 | 100% | 无显示器截图与交互 | BLOCKED |
| 1280×720 | 125% | 无显示器截图与交互 | BLOCKED |
| 1920×1080 | 150% | 无显示器截图与交互 | BLOCKED |
| 800×600 | 150% | 极端窄窗口布局由双向滚动承接；无显示器截图 | BLOCKED |

### 动态操作验收

最大化、恢复、拖动到最小尺寸、连续改变宽高、拖动侧栏分隔线、切换波形/DC/LVDS/遥测、增长日志、显示异常和改变 UI 缩放均需要真实窗口交互。当前均为 `BLOCKED`，应在有桌面会话的 QA 机器上完成，并将截图保存到 `target/ui-qa/`。当前该目录没有可交付截图文件。

## 5. 功能与回归验收

| 编号 | 验收项 | 状态 | 依据 |
| --- | --- | --- | --- |
| UI-01 | 按钮遮挡 | BLOCKED | 已移除固定编辑区并改为换行；缺少 GUI 截图/点击证据 |
| UI-02 | 模块重叠 | BLOCKED | 面板边界已改为 egui 面板布局；缺少 GUI 观察证据 |
| UI-03 | 文字完整可读 | BLOCKED | 长文本已截断并支持悬停；缺少 GUI 观察证据 |
| UI-04 | 窗口缩小后核心功能可访问 | BLOCKED | 最小尺寸和滚动策略已实现；缺少 GUI 交互证据 |
| UI-05 | 窗口放大后空间利用 | BLOCKED | 侧栏范围和中央弹性区域已实现；缺少 GUI 观察证据 |
| UI-06 | 面板滚动 | BLOCKED | 左右和中央滚动代码已编译；缺少 GUI 交互证据 |
| UI-07 | UI 缩放 | BLOCKED | 缩放选项和边界测试代码已加入；缺少 GUI 交互证据 |
| UI-08 | 高 DPI | BLOCKED | 使用 egui 缩放并保留 native pixel ratio；当前环境无法提供 Retina/Windows 实测 |
| UI-09 | 关键按钮可操作 | BLOCKED | Mock 已启动，但无辅助功能权限进行点击自动化 |
| UI-10 | 大量日志/遥测不破坏布局 | BLOCKED | 有界日志、滚动和文本截断已实现；缺少 GUI 数据注入截图 |
| UI-11 | RS422 协议回归 | PASS | `langmuir-core` 31 个测试通过，包含六条黄金帧；CLI 4 个测试通过 |
| UI-12 | Mock 隔离 | PASS | 使用 `--features mock -- --mock` 启动，未连接真实硬件 |
| UI-13 | GUI 实测与截图 | BLOCKED | 启动成功，但显示、截图和自动化访问均被环境阻断 |
| UI-14 | 编译与测试 | BLOCKED | 格式、workspace check、核心/CLI 测试通过；桌面/全 workspace 测试受依赖缓存阻断 |

## 6. 上一阶段质量验证记录

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check -p langmuir-desktop --features mock --offline` | PASS |
| `cargo check -p langmuir-desktop --locked --offline` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked --offline` | PASS，31 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `errno v0.3.14` |
| `cargo test -p langmuir-desktop --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `getrandom 0.4.3` |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：无法解析 `static.crates.io`，缺少 `rustix 1.1.4` |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy -p langmuir-core -p langmuir-protocol-cli --lib --locked --offline -- -D warnings` | PASS |

没有修改 `Cargo.lock`，也没有为了绕过依赖下载问题删除测试或调整依赖版本。桌面测试的原始阻断为：

```text
error: failed to download from `https://static.crates.io/crates/tempfile/3.27.0/download`
Caused by: [6] Couldn't resolve host name (Could not resolve host: static.crates.io)
```

离线重跑时对应错误为：

```text
error: failed to download `errno v0.3.14`
Caused by: attempting to make an HTTP request, but --offline was specified
```

## 7. 待补验证与剩余风险

- 需要在有图形桌面会话的 macOS/Windows QA 机器上，按第 4 节矩阵完成真实拖拽、点击、滚动和截图。
- 需要在 Retina 以及 Windows 125%、150%、200% 系统 DPI 下检查系统 DPI 与应用 UI 缩放是否符合预期；当前实现没有叠加修改 `native_pixels_per_point`。
- 800×600 + 150% 是极端组合，中央区已提供横向滚动，顶部换行上限已放宽到 180px；仍应由真实 GUI 检查是否需要进一步折叠。
- `cargo test -p langmuir-desktop` 和完整 workspace test/clippy 需要在可下载 `errno v0.3.14` 的环境中补跑。

## 8. 上一阶段 Git 变更

上一阶段结束时执行的工作区检查（第三阶段最新状态见第 10 节）：

```text
git status --short
 M apps/desktop/src/app.rs
 M apps/desktop/src/bridge.rs
 M apps/desktop/src/main.rs
 M apps/desktop/src/mock.rs
 M crates/langmuir-core/src/command.rs
 M crates/langmuir-core/src/lib.rs
 M docs/PROTOCOL.md
 M docs/RS422_PROTOCOL_20260724.md
 M tools/protocol-cli/src/main.rs
?? docs/UI_LAYOUT_QA_REPORT.md
```

`git diff --stat`（Git 默认不计入尚未添加的本报告文件）：

```text
 apps/desktop/src/app.rs             | 587 +++++++++++++++++++++---------------
 apps/desktop/src/bridge.rs          |   2 +
 apps/desktop/src/main.rs            |   5 +-
 apps/desktop/src/mock.rs            |  63 ++--
 crates/langmuir-core/src/command.rs | 227 ++++++++++-----
 crates/langmuir-core/src/lib.rs     |   4 +-
 docs/PROTOCOL.md                    |  28 +-
 docs/RS422_PROTOCOL_20260724.md     |  20 +-
 tools/protocol-cli/src/main.rs      |  64 +++-
 9 files changed, 632 insertions(+), 368 deletions(-)

上一阶段报告版本长度为 174 行，尚未计入上面的 Git 默认统计。
```

没有执行 Git 提交、重置或清理用户已有改动。

## 9. 第三阶段：指令构造器与 UI 精细化修复

本阶段依据用户描述的真实截图缺陷进行针对性修复：右侧遥测信息在无数据时不可辨认、左侧指令区按钮被内容遮挡、不同指令类型仍显示无关参数、滑条不易操作，以及轮询预设高亮与实际周期不一致。当前工作区和附件目录没有可供再次读取的 PNG/JPG 截图文件，因此截图缺陷记录以用户提供的文字和现有代码复现为依据；没有把代码检查结果冒充视觉验收结果。

### 指令构造器

- `TYPE=0x11` 遥测请求只显示固定请求说明、12B 预览、SUM 状态、复制和发送操作。
- `TYPE=0x21` 工作模式使用三角波、正弦波、DC 三个明确选项。三角波和正弦波只显示 `0..=100` 扫描频率滑条及整数输入；DC 只显示原始 `u16` 电压设定值，并明确协议没有定义物理单位。
- `TYPE=0x31` LVDS 使用“发送开启/发送关闭”分段选择，只显示目标状态与帧预览；实际设备状态仍以遥测为准。
- 预览直接来自 `langmuir-core` 编码器，显示完整 12B、`F0 → 55`、TYPE、当前 SUM、预期 SUM 和校验状态，并支持复制完整帧。
- 原始模式保留 `HEADER | SRC | DST | LEN | TYPE | DATA | SUM` 编辑器。校验、重算和发送保护均调用核心 API；“重算 SUM”只改第 12 字节，错误结构或错误 SUM 不会通过发送按钮下发。发送按钮在原始帧无效时置灰，校验错误仍显示具体原因。
- 右侧“上一条指令”改为“上一条指令数据域（5B）”，并说明遥测字段只包含 TYPE + DATA1..DATA4，避免把 5B 数据域误称为完整 12B 发送帧。

### 滑条、轮询和状态布局

- 频率、电压和轮询滑条使用高对比度蓝色选中区、轨道边界和活动态，宽度按可用面板空间伸展；数字输入与滑条双向同步。
- 轮询周期输入、滑条和 `100/1000/5000 ms` 预设在同一分组。预设只有在实际值相等时高亮，手动改为 `1100 ms` 时不会错误高亮 `1000 ms`；顶部运行状态同时显示当前周期。
- 温度卡片拆成标题/实时值、进度条和阈值说明三行，减少窄右栏中的挤压。
- 右侧遥测字段逐项换行显示，时间显示完整日期时间；无遥测时显示等待数据、连接/采集状态和 Mock 来源。设备状态、报警空态和长文本保持可读。
- 中央无数据图表显示“暂无 LVDS 数据”，同时说明 LVDS 连接、采集是否启动、冻结状态和等待样本；隐藏全部通道时明确提示重新选择通道。日志使用有界滚动列表，支持跟随最新消息和手动查看历史。
- 顶部状态栏分别展示采集、LVDS、RS422、遥测轮询和 MOCK 状态；软件急停文案明确只停止上位机采集/循环遥测并发送禁用 LVDS 指令，不替代硬件急停回路。

### 第三阶段验证矩阵

| 验收项 | 结果 | 证据或限制 |
| --- | --- | --- |
| 遥测/三角波/正弦波/DC/LVDS 动态控件 | 代码通过 | `typed_command_controls` 按 `CommandFamily` 和工作模式分支渲染；未有桌面会话进行点击截图 |
| 原始错误 SUM 检测、重算和发送保护 | PASS | 核心校验 API 及桌面原始校验测试；错误帧发送按钮置灰 |
| 频率边界 `0x00`、`0x64` 与越界 | PASS | 核心 `ScanFrequency` 测试；`0x65`/`0xFF` 返回错误 |
| 电压大端序 | PASS | 核心 DC 1000 黄金帧和 `0x1234` 字节序测试 |
| 六条黄金帧 | PASS | `langmuir-core` 31 tests；包含遥测、三角波 50、正弦波 50、DC 1000、LVDS 开启/关闭 |
| 轮询周期与预设同步 | PASS | 1000 ms 高亮、1100 ms 不高亮的纯函数测试；未改变轮询通信语义 |
| 无数据图表、日志、右侧和顶部状态 | 代码通过 | 需真实 GUI 交互验证；当前无 `DISPLAY`/`WAYLAND_DISPLAY` |
| 800×600、1024×768、1280×720、1440×900、1920×1080 及 100/125/150% | BLOCKED | 当前环境无显示器，无法拖拽、点击或截图 |
| Mock 启动隔离 | PASS | `cargo run -p langmuir-desktop --features mock -- --mock` 成功启动，未连接真实设备 |

截图目录 `target/ui-qa/` 当前没有可交付截图。需要在有桌面会话的 QA 机器上补拍遥测、三角波、DC、LVDS、错误 SUM、无数据图表、1280×720 和 150% UI 缩放场景；这些场景仍标记为 BLOCKED。

## 10. 第三阶段最新 Git 与验证记录

本阶段没有执行提交、重置、清理或硬件操作。当前 `git status --short` 为：

```text
 M apps/desktop/src/app.rs
 M apps/desktop/src/bridge.rs
 M apps/desktop/src/main.rs
 M apps/desktop/src/mock.rs
 M apps/desktop/src/ui/theme.rs
 M crates/langmuir-core/src/command.rs
 M crates/langmuir-core/src/lib.rs
 M docs/PROTOCOL.md
 M docs/RS422_PROTOCOL_20260724.md
 M tools/protocol-cli/src/main.rs
?? docs/UI_LAYOUT_QA_REPORT.md
```

最新 `git diff --stat`（未跟踪的报告不计入 Git 默认统计）为：

```text
 apps/desktop/src/app.rs             | 1210 +++++++++++++++++++++++------------
 apps/desktop/src/bridge.rs          |    2 +
 apps/desktop/src/main.rs            |    5 +-
 apps/desktop/src/mock.rs            |   63 +-
 apps/desktop/src/ui/theme.rs       |   13 -
 crates/langmuir-core/src/command.rs |  489 ++++++++++++--
 crates/langmuir-core/src/lib.rs     |    5 +-
 docs/PROTOCOL.md                    |   28 +-
 docs/RS422_PROTOCOL_20260724.md     |   20 +-
 tools/protocol-cli/src/main.rs      |   64 +-
 10 files changed, 1343 insertions(+), 556 deletions(-)
```

本阶段命令结果：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked --offline` | PASS，31 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo clippy -p langmuir-core -p langmuir-protocol-cli --lib --locked --offline -- -D warnings` | PASS |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `tempfile v3.27.0` |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `getrandom v0.4.3` |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：无法解析 `static.crates.io`，缺少 `getrandom v0.4.3` |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

桌面/全 workspace 测试和 clippy 未被伪造为通过；它们需要在可访问 crates.io 且缓存完整的环境中补跑。LVDS 科学数据解码、UDP 重组、数据存储及真实设备通信本阶段未修改、未连接、未验证。

本阶段结束时 `git status --short` 仍为：

```text
 M apps/desktop/src/app.rs
 M apps/desktop/src/bridge.rs
 M apps/desktop/src/main.rs
 M apps/desktop/src/mock.rs
 M apps/desktop/src/ui/theme.rs
 M crates/langmuir-core/src/command.rs
 M crates/langmuir-core/src/lib.rs
 M docs/PROTOCOL.md
 M docs/RS422_PROTOCOL_20260724.md
 M tools/protocol-cli/src/main.rs
?? docs/UI_LAYOUT_QA_REPORT.md
```

最新 `git diff --stat`（未跟踪报告不计入）为：

```text
 apps/desktop/src/app.rs             | 1437 ++++++++++++++++++++++++-----------
 apps/desktop/src/bridge.rs          |    2 +
 apps/desktop/src/main.rs            |    5 +-
 apps/desktop/src/mock.rs            |   63 +-
 apps/desktop/src/ui/theme.rs       |   13 -
 crates/langmuir-core/src/command.rs |  489 ++++++++++--
 crates/langmuir-core/src/lib.rs     |    5 +-
 docs/PROTOCOL.md                    |   28 +-
 docs/RS422_PROTOCOL_20260724.md     |   20 +-
 tools/protocol-cli/src/main.rs      |   64 +-
 10 files changed, 1541 insertions(+), 585 deletions(-)
```

## 11. 固定停靠式 UI 与闪烁专项修复

本阶段针对最新截图中右侧卡片缩窄、三栏边界不稳定、动态数据引起的跳动和状态灯闪烁进行修复。协议编码、遥测解析、LVDS 解码、UDP 重组、存储和硬件控制逻辑均未修改。

### 缺陷、根因与改动

| 缺陷 | 根因 | 改动 |
| --- | --- | --- |
| 右侧卡片只占半列，右边出现黑色空白 | `Frame::show` 按子内容最小宽度计算外框，短文本使卡片收缩；滚动区内容宽度也未明确锁定 | `full_width_card`/`full_width_subtle_card` 在子 UI 中调用 `take_available_width()`；左右滚动内容调用 `set_min_width(available_width)`，右栏时间、温度、设备状态、报警卡片共享完整父列宽度 |
| 三栏像自由浮动卡片 | 左右面板允许持久化/拖拽尺寸变化，内容变化会影响可用空间 | 左栏固定 360 logical points，右栏固定 320 logical points，均使用稳定 ID、`resizable(false)` 和 `exact_size`；中央区填充剩余空间 |
| 小窗口三栏中央区域过窄 | 宽屏固定列尺寸直接用于 800×600 | 当可用逻辑宽度低于 1120 时切换到明确的紧凑停靠尺寸：左 300、右 280；该判断只依赖窗口逻辑宽度，不依赖遥测数据 |
| 时间、计数和状态更新时内容上下跳动 | 监控字段采用纵向自适应布局，数值长度变化会改变行高 | 监控字段使用固定标签宽度、固定 32pt 行高和等宽数字；值区占用剩余宽度并提供悬停全文 |
| 正常状态灯和空遥测 spinner 闪烁 | `heartbeat_indicator` 使用时间相位改变颜色/半径；无遥测状态使用动画 spinner；UI 因此持续高频重绘 | 状态灯改为稳定颜色和半径；空遥测改为静态标记；仅采集/科学图表活动时保持 50ms 重绘，遥测空闲时 250ms 重绘 |
| 顶部状态变化导致控件左右跳动 | 冻结、MOCK、字体警告和轮询文字按内容自然占宽 | 状态项使用固定逻辑宽度容器；轮询周期变化不会重新分配其他按钮位置 |

### Rect 调试与稳定性证据

新增开发调试开关：

```text
LANGMUIR_UI_DEBUG_RECTS=1 cargo run -p langmuir-desktop --features mock -- --mock
```

启用后，`app.rs` 会记录顶部状态栏、左栏、中央区、右栏、日志、左右控制卡、中央操作/图表卡以及右侧四张监控卡的 `x/y/w/h`。同一块区域只有在几何变化超过 1 个 logical point 时再次输出，适合观察 60 秒 Mock 运行期间是否发生布局跳动。当前无 `DISPLAY`、`WAYLAND_DISPLAY` 或可用截图桌面，因此本环境只能完成代码接入和启动编译，不能将 Rect 日志或启动日志当作真实视觉验收。

### 固定布局验收矩阵

| 场景 | 结果 | 证据/限制 |
| --- | --- | --- |
| 1360×860、1600×900、1920×1080 宽屏三栏 | 代码通过 | 固定左/右停靠尺寸，中央填充剩余空间；缺少真实桌面截图 |
| 1280×720、1024×768 | 代码通过 | 使用固定三栏；无 GUI 交互证据 |
| 800×600 | 代码通过 | 进入 300/280 紧凑停靠尺寸，中央保留约 220 logical points；无 GUI 交互证据 |
| 100%、125%、150% UI 缩放 | 代码通过 | 尺寸按 egui logical points 计算；缺少 Retina/Windows 实测 |
| 右侧四张卡片边界对齐 | 代码通过 | `take_available_width` + ScrollArea 内容宽度锁定；缺少截图/Rect 实际采样 |
| Mock 连续运行 60 秒 | BLOCKED | 无桌面会话，无法保持窗口观察或取得 Rect 日志 |
| 状态灯/空遥测无动画闪烁 | 代码通过 | 移除 heartbeat 和 spinner 时间动画；告警未确认态仍保留告警语义的颜色强调 |
| RS422 六条黄金帧 | PASS | 核心 31 项测试和 CLI 4 项测试继续通过 |

本阶段未生成截图。需要用户在有桌面会话的机器上按 1360×860、1600×900、1280×720、800×600、125% 和 150% 场景补拍主窗口、右栏满宽、60 秒前后、小窗口和缩放截图，并同时保存 `LANGMUIR_UI_DEBUG_RECTS=1` 的日志。

### 本阶段最新验证

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked --offline` | PASS，31 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `tempfile v3.27.0` |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `rustix v1.1.4` |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：无法解析 `static.crates.io`，缺少 `fastrand v2.5.0` |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

当前工作区仍保持未提交状态，没有执行 Git 提交、重置、清理或真实设备操作。

## 12. 收尾复核（2026-10-16）

在不改变代码的情况下再次执行了格式、离线编译、核心/CLI 回归和差异检查：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked --offline` | PASS，31 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo clippy -p langmuir-core -p langmuir-protocol-cli --lib --locked --offline -- -D warnings` | PASS |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |

联网版桌面测试、workspace 测试和 workspace clippy 也已尝试，均因当前环境无法解析 `static.crates.io` 而无法下载缺失依赖；没有将其记为通过。当前 `git diff --stat`（未跟踪的本报告不计入）为 10 个已跟踪文件、1541 insertions、585 deletions。本报告本身仍为未跟踪文件，所有代码和文档修改均未提交。

## 13. 顶部 Header 与遥测 Slider 专项修复（2026-10-16）

本轮只修改顶部状态工具栏、自动遥测轮询控件及其直接状态展示，没有重构三栏主体，也没有修改 RS422、LVDS、UDP、存储或硬件通信逻辑。

### 根因与修改

| 问题 | 实际根因 | 修改 |
| --- | --- | --- |
| 顶部状态和按钮错行 | Header 使用 `horizontal_wrapped`，标题、状态、轮询、缩放和急停按内容自然换行；固定宽度容器也没有统一的右侧操作区 | `header_content` 改为固定的左侧标题/设备状态组与右侧控制组；宽屏使用单行 `Layout::left_to_right` + 右对齐控制区，窄宽度只切换到固定两行；删除 Header 的随机换行路径 |
| Header 高度跳变 | 顶部面板允许从 60 到 180 logical points 自适应 | 宽屏 Header 固定 60，紧凑 Header 固定 104；紧凑断点只由可用逻辑宽度决定，不由遥测数据决定 |
| Slider 只有小灰色滑块 | `add_sized` 只扩大了外层 Ui，egui `Slider` 内部仍使用 `Spacing::slider_width` 默认值约 100，轨道因此没有填满父列 | 在局部样式 scope 中将 `slider_width` 设置为父列可用宽度（最小 180 logical points），`slider_rail_height` 设置为 6pt，并启用 trailing fill；轨道、选中区、悬停、拖动和滑块边框使用高对比度主题色 |
| 周期显示不一致 | 编辑控件直接修改后端确认状态字段，异步回执尚未到达时顶部值与正在编辑的值可能不同 | 新增 `telemetry_polling_edit_millis`。顶部只显示后端确认的“当前执行周期”，左栏同时显示编辑值和“已同步/待应用”；后端 `TelemetryPollingChanged` 或状态快照确认后再同步两者 |

### 代码位置

- [`apps/desktop/src/app.rs`](/Users/lingyuancong/Documents/Rust%20APP/rust/apps/desktop/src/app.rs:1121)：轮询分组改为“当前执行值、编辑值、整行 Slider、范围提示、输入/预设、启停”固定顺序；Slider Rect 记录为 `left.telemetry_slider`。
- [`apps/desktop/src/app.rs`](/Users/lingyuancong/Documents/Rust%20APP/rust/apps/desktop/src/app.rs:1902)：新增 Header 标题、设备状态组、右侧控制组和紧凑 Header 布局；顶部 Rect 记录为 `top_status`、`header.title`、`header.device_group`、`header.controls`。
- [`apps/desktop/src/ui/theme.rs`](/Users/lingyuancong/Documents/Rust%20APP/rust/apps/desktop/src/ui/theme.rs:15)：增加 `SLIDER_TRACK` 语义颜色。

### 几何与状态验证

- Slider 使用 `polling_slider_width(available_width)`，左栏正常内容宽度约 284pt 时实际宽度为 284pt；即使父列低于 180pt，仍保留 180pt 的可操作最小轨道。新增单元测试覆盖 120→180 和 284→284 两个边界。
- 轮询范围仍为 100–60000ms，未改变后端定时器语义；预设为 100、1000、5000、60000ms，只有编辑值精确相等时高亮。
- Header 宽屏单行断点为 1280 logical points；低于该值进入固定两行布局。所有状态项和右侧操作项均使用稳定容器宽度，周期从 100ms 变为 60000ms 不会改变其他按钮的分配位置。

### 本轮验证结果

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo check -p langmuir-desktop --locked --offline` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked --offline` | PASS，31 passed，六条黄金帧保持不变 |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：无法解析 `static.crates.io`，缺少依赖下载 |
| `cargo test --workspace --locked` | BLOCKED：无法解析 `static.crates.io`，缺少依赖下载 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：无法解析 `static.crates.io`，缺少依赖下载 |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |

当前环境仍没有 `DISPLAY`/`WAYLAND_DISPLAY`，因此没有虚构截图或 60 秒视觉验收结果。用户本机可执行：

```text
cargo run -p langmuir-desktop --features mock -- --mock
LANGMUIR_UI_DEBUG_RECTS=1 cargo run -p langmuir-desktop --features mock -- --mock
```

重点观察 1360×860、1600×900、1920×1080 单行 Header，1280×720/125% 和 1024×768/150% 紧凑 Header，以及轮询周期 100、1000、60000ms 时 `header.controls` 和 `left.telemetry_slider` 的 Rect 是否稳定。所有改动仍未提交。

本轮收尾 `git diff --stat`（未跟踪报告不计入）为 10 个已跟踪文件、1730 insertions、623 deletions；现有未提交改动均予以保留。

## 14. RS422 最终协议与校验和验收（2026-10-16）

本轮以根目录原始《星务与等离子体密度探针单机 RS422 串行通信协议-20260724（公开）.doc》的表 2-2、2-3、2-4、2-5 为唯一依据，只补充 RS422 回归测试，没有修改 UI 布局、LVDS/UDP/存储逻辑或真实设备配置。

### 六条标准指令

独立按 `SUM = (TYPE + DATA1 + DATA2 + DATA3 + DATA4) & 0xFF` 重算，结果与原始表格、`langmuir-core` 编码器、CLI 输出完全一致：

| 指令 | 12 字节帧 | 独立 SUM | 结果 |
| --- | --- | ---: | --- |
| 遥测请求 | `EB 90 F0 55 00 06 11 9B 9B 9B 9B 7D` | `7D` | PASS |
| 三角波 50 | `EB 90 F0 55 00 06 21 55 32 9B 9B DE` | `DE` | PASS |
| 正弦波 50 | `EB 90 F0 55 00 06 21 AA 32 9B 9B 33` | `33` | PASS |
| DC 1000 | `EB 90 F0 55 00 06 21 BB 03 E8 9B 62` | `62` | PASS |
| LVDS 开启 | `EB 90 F0 55 00 06 31 55 9B 9B 9B 57` | `57` | PASS |
| LVDS 关闭 | `EB 90 F0 55 00 06 31 AA 9B 9B 9B AC` | `AC` | PASS |

三角波/正弦波频率继续限制为 `0x00..=0x64`，DC 电压按协议使用大端 `u16`，未引入物理单位或未经表 2-4 规定的下发范围。新增测试覆盖修改 DATA 保留旧 SUM 的错误，以及显式重算后恢复合法帧；遥测应答也新增同类测试。

### 编码、发送与接收链路

| 项目 | 结果 | 依据 |
| --- | --- | --- |
| 类型化构造、HEX 预览、复制完整帧、普通发送同源 | PASS | `typed_command_bytes()` 始终调用核心 `Command::encode()`；预览、复制和 `UiRequest::SendCommand([u8; 12])` 使用同一数组 |
| 原始字节校验与发送保护 | PASS | 核心 `validate_command_frame` 检查 `EB 90`、`F0 -> 55`、`0x0006`、TYPE/DATA 合法性和 SUM；无效帧的普通发送按钮禁用，重算只改第 12 字节 |
| 遥测应答解析 | PASS | `EB 90 55 F0 00 1C 1A DATA[26] SUM` 固定 34B；解析器验证头、地址、长度、TYPE、SUM，并按大端解析时间和电压字段 |
| 控制指令应答语义 | PASS | 控制指令无设备应答；`CommandSent` 只在本地写入 ticket 完成后产生，UI 日志明确写“设备无控制应答” |
| Mock 发送路径 | PASS（代码）/ BLOCKED（单元测试执行） | 新增测试连接 MOCK-RS422 后断言 `CommandSent` 与提交的 12B 逐字节相同；本环境因缺少 `errno v0.3.14` 未能运行桌面 Mock 测试 |
| 真实设备执行 | BLOCKED | 本轮未连接真实串口，Mock 结果不作为硬件执行证据 |

### 串口参数审计

`langmuir-transport::SerialConfig::new` 设置 8 数据位、Odd 奇校验、1 停止位、无硬件流控；桌面默认波特率为 115200，以上固定格式 PASS。当前连接面板仍允许选择其他波特率或自定义值，后端把调用方传入值交给串口驱动，因此“始终强制 RS422=115200”本轮标记为 FAIL（既有可配置接口未在本轮擅自修改）。使用真实 RS422 前必须将端口配置固定为协议要求的 115200。

### 本轮回归命令

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked` | PASS，33 passed |
| `cargo test -p langmuir-core --locked --offline` | PASS，33 passed |
| `cargo test -p langmuir-protocol-cli --locked` | PASS，4 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：DNS 无法解析 `static.crates.io`，缺少 `fastrand v2.5.0` 等依赖 |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test -p langmuir-desktop --features mock --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14`，Mock 单元测试未能在本环境执行 |
| `cargo test --workspace --locked` | BLOCKED：DNS 无法解析 `static.crates.io`，缺少 `getrandom v0.4.3` 等依赖 |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：DNS 无法解析 `static.crates.io`，缺少 `rustix v1.1.4` 等依赖 |

当前没有图形桌面或真实设备，未生成新的 GUI/硬件截图；既有未提交改动全部保留，未执行提交、重置或清理。

本轮结束时 `git diff --stat`（未跟踪的本报告不计入）为 11 个已跟踪文件、1785 insertions、624 deletions；`git status --short` 中的既有 UI、协议、Mock 和文档改动均未被覆盖。

## 15. 第四阶段：中央工作区固定布局与无滚动重构（2026-10-16）

本阶段只处理中央 LVDS/UDP、实时图表和系统日志区域，顶部 Header、左侧 RS422 指令区、右侧遥测区以及 RS422、LVDS、UDP、存储业务逻辑均未改变。

### 根因

中央原先由 `ScrollArea::both()` 包住全部内容。LVDS 控件使用自然高度和 `horizontal_wrapped()`，图表用 `available_height().max(MIN_PLOT_HEIGHT)` 强行保留最小高度，底部日志又作为独立 `Panel::bottom` 与中央图表争抢空间。三者叠加后，控件换行或日志增长会使中央内容溢出并出现整体滚动条；空数据图表的大片灰色 Frame 也被误认为滑动条。

### 实现结果

`apps/desktop/src/app.rs` 的 `central_layout_rects()` 现在从中央可用 Rect 明确分配四个矩形：

```text
A  LVDS / UDP 操作区       136 logical pt
   间隔                     6 logical pt
B1 图表工具栏              68 logical pt
   间隔                     6 logical pt
B2 图表主体                 弹性剩余高度
   间隔                     6 logical pt
C  系统日志                 160 logical pt
```

只有 B2 随窗口高度变化。A、B1、C 的位置和高度与样本数量、日志数量、监听状态无关；极小高度时固定区按同一比例收缩，所有矩形仍保持在中央 Rect 内。中央根布局不再创建 `ScrollArea`，只有 C 区的日志条目使用 `ScrollArea::vertical()`。

`operations_panel()` 改为固定四行。地址输入、端口、连接和开始/停止采集保持在固定行内；低频刷新、CSV 导出和 Mock 异常注入在紧凑宽度收入“更多操作”菜单；协议说明、监听状态和统计使用受约束的截断文本。采集按钮所在行按 44pt 实际高度分配，避免按钮内容从固定行溢出。

`plot_toolbar()` 改为固定两行。常规宽度显示通道、叠加/分屏、独立归一化、冻结、清空和缓存点数；中央宽度低于 520pt 时，第二行使用固定“图表操作”菜单，缓存点数占用剩余宽度并截断，因此不会把工具栏撑出分配矩形。分屏图表高度按主体剩余高度计算，不再使用会挤出日志的固定最小高度。

`plot_panel()` 的无数据状态改为图表主体内部的居中图标、状态文字和等待样本说明，去掉类似 ProgressBar 的灰色横条。接收到样本后仍使用原有通道、单位和 `egui_plot` 交互；没有修改 LVDS 解码或数据缓存。

`log_panel()` 固定在中央底部，标题和计数占用稳定行高，日志条目在自己的垂直 ScrollArea 内滚动并保留自动跟随最新消息。窄宽度下隐藏辅助说明文字，但保留标题和计数，避免日志标题栏撑宽中央区域。

### 几何验证

新增 `central_layout_rects_partition_normal_workspace_without_overlap`、`central_layout_rects_remain_inside_tiny_workspace`、`central_layout_rects_geometry_is_content_independent` 和 `central_layout_rects_cover_requested_viewport_matrix` 单元测试。它们验证矩形在中央 Rect 内、宽度一致、按顺序无重叠、日志贴合底部、内容变化不会改变几何结果。

这些桌面单元测试已随应用通过 workspace `cargo check` 编译；由于桌面测试所需的 `errno v0.3.14` 不在本地缓存，测试二进制本轮未能执行，故几何 PASS 表示确定性布局函数的静态矩阵结果，不能替代真实 GUI 观察。

按固定停靠尺寸计算的中央工作区静态矩形如下；这是几何函数的可重复验证数据，不是桌面截图证据：

| 窗口逻辑尺寸 | 中央可用宽×高（估算） | A 高度 | B1 高度 | B2 高度 | C 高度 | 几何结果 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1360×860 | 680×800 | 136 | 68 | 418 | 160 | PASS |
| 1600×900 | 920×840 | 136 | 68 | 458 | 160 | PASS |
| 1920×1080 | 1240×1020 | 136 | 68 | 638 | 160 | PASS |
| 1280×720 | 600×660 | 136 | 68 | 278 | 160 | PASS |
| 1024×768（紧凑 Header/侧栏） | 444×664 | 136 | 68 | 282 | 160 | PASS |

表中的尺寸是传给 egui 布局的 logical points 假设，不能直接当作物理像素；例如系统 DPI 150% 时物理像素尺寸会另有换算。当前没有桌面会话，无法取得实际 `available_rect_before_wrap()`，因此 1024×768/150% 的系统 DPI 换算和视觉结果仍为 BLOCKED。

调试环境变量 `LANGMUIR_UI_DEBUG_RECTS=1` 会记录 `central_data`、`central.operations`、`central.plot_toolbar`、`central.plot_body`、`central.logs`，并只在几何变化超过 1 logical point 时再次输出。图表内部 Rect 使用独立标签，避免把卡片内边距误判为中央主体发生跳动。

### 实际 GUI 与 Mock 稳定性

本环境无 `DISPLAY`、`WAYLAND_DISPLAY` 或可用虚拟显示器。已启动 Mock 应用并确认程序编译、进入运行态；启动日志只包含 macOS 系统服务的 `Connection invalid`，没有 Rust panic。由于没有桌面会话，没有采集到 `UI_RECT`、截图或真实 60 秒窗口观察，因此以下项目仍为 `BLOCKED`：

- 1360×860、1600×900、1920×1080、1280×720/125%、1024×768/150% 的视觉布局；
- 中央无整体滚动条、图表空态视觉、日志内部滚动和按钮点击；
- Mock 连续运行 60 秒期间的实际 Rect 采样和闪烁观察。

本机可在有桌面会话的 QA 机器上执行：

```text
cargo run -p langmuir-desktop --features mock -- --mock
LANGMUIR_UI_DEBUG_RECTS=1 cargo run -p langmuir-desktop --features mock -- --mock
```

验收时依次检查上述五组窗口/缩放组合，确认中央没有根滚动条、A/B1/C 边界不变、B2 优先吸收高度变化、日志只在内部滚动，并在 60 秒前后保存 Rect 日志与截图到 `target/ui-qa/`。

### 第四阶段回归命令

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked` | PASS，33 passed |
| `cargo test -p langmuir-core --locked --offline` | PASS，33 passed |
| `cargo test -p langmuir-protocol-cli --locked` | PASS，4 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：当前环境无法解析 `static.crates.io`，缺少 `tempfile v3.27.0` 等依赖 |
| `cargo test -p langmuir-desktop --features mock --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked` | BLOCKED：无法下载 `getrandom v0.4.3` 等依赖 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：无法下载 `fastrand v2.5.0` 等依赖 |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

当前仍未执行 Git 提交、重置、清理或真实设备操作。RS422、SUM、遥测解析、LVDS 科学数据、UDP 重组、存储和 Mock/实机隔离均未因本阶段布局调整而改变。

当前工作区变更清单仍包含既有的 `apps/desktop/src/bridge.rs`、`apps/desktop/src/main.rs`、`apps/desktop/src/mock.rs`、`apps/desktop/src/ui/theme.rs`、`crates/langmuir-core/src/command.rs`、`crates/langmuir-core/src/lib.rs`、`crates/langmuir-core/src/rs422.rs`、`docs/PROTOCOL.md`、`docs/RS422_PROTOCOL_20260724.md` 和 `tools/protocol-cli/src/main.rs`，以及本阶段继续修改的 [`apps/desktop/src/app.rs`](/Users/lingyuancong/Documents/Rust%20APP/rust/apps/desktop/src/app.rs)；[`docs/UI_LAYOUT_QA_REPORT.md`](/Users/lingyuancong/Documents/Rust%20APP/rust/docs/UI_LAYOUT_QA_REPORT.md) 仍为未跟踪报告文件。当前 Git 默认统计为 11 个已跟踪文件、2177 insertions、742 deletions，未执行自动提交。

## 16. 扫描频率 Slider 宽度塌缩专项修复（2026-10-16）

本轮只修改 RS422 指令构造器中三角波/正弦波扫描频率编辑器及其局部诊断和测试，没有修改自动遥测轮询定时、协议编码、SUM、三栏布局、LVDS/UDP 或存储逻辑。

### 实际根因

原 `frequency_parameter()` 只用 `ui.add_sized([width, height], Slider)` 扩大外层 Widget Rect，没有同步设置 egui 的 `ui.spacing_mut().slider_width`。当前 egui 版本的 Slider 内部轨道仍按 `spacing.slider_width` 计算，因此外层看似有宽度，实际绘制轨道仍可能塌缩成很窄的区域，截图中只剩孤立的竖直滑块。

频率 Slider 下方原本还把最小值、DragValue、当前/最大文本和预设放在同一个 `horizontal_wrapped()` 中。内容自然换行或抢占宽度后，数值区域互相挤压，进一步掩盖了 Slider 的问题。该 Slider 没有被禁用；问题是内部宽度配置与父布局结构不一致。

### 修复结果

`frequency_parameter()` 现在使用固定四行结构：

1. `扫描频率设定值` 与右对齐的 `当前值 N`；
2. 独占整行的真实 `egui::Slider`；
3. 左右对齐的 `最小 0` 和 `最大 100`；
4. `快速设置` 的 `1/50/100` 按钮、分隔线和精确 DragValue 输入。

Slider 使用父卡片剩余宽度，并在局部样式中同时设置：

- `spacing.slider_width = slider_width`；
- `spacing.slider_rail_height = 6.0`；
- `trailing_fill(true)`；
- 灰蓝未选中轨道、亮蓝选中轨道、白色滑块边界，以及悬停/拖动状态。

`frequency_slider_width()` 只使用父容器实际宽度，窄于 180pt 时允许合理收缩，不强行制造横向溢出；父容器达到 180pt 或更宽时，轨道会尽量填满可用宽度。三角波和正弦波仍共用这一编辑器，分别由 `Command::configure_triangle()` 和 `Command::configure_sine()` 生成原有协议帧。

在 `LANGMUIR_UI_DEBUG_RECTS=1` 下新增诊断：

```text
UI_RECT left.scan_frequency_parameter: ...
UI_RECT left.scan_frequency_slider: ...
UI_SLIDER left.scan_frequency_slider: parent_w=... configured_w=... response_w=... response_h=... rail_h=6.0 enabled=true
```

这分别核对父容器宽度、Slider 配置宽度、Response Rect 和启用状态；egui 轨道宽度由同一局部 `slider_width` 设置控制。新增 `frequency_slider_uses_the_parent_width_without_forcing_overflow` 测试覆盖 284pt、180pt、120pt 和零宽度边界。

### 协议和回归验证

频率值仍限制为 `0..=100`，输入、拖动和快捷按钮都会直接修改同一个 `u8` 参数；下一次绘制会通过核心 `Command::configure_triangle/sine` 重新生成预览帧和 SUM。核心六条黄金帧、正弦波 50 的 `EB 90 F0 55 00 06 21 AA 32 9B 9B 33` 均未修改。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked` | PASS，33 passed |
| `cargo test -p langmuir-protocol-cli --locked` | PASS，4 passed |
| `cargo test -p langmuir-desktop --features mock --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14`，桌面频率单元测试未执行 |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：无法解析 `static.crates.io`，缺少 `fastrand v2.5.0` 等依赖 |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

最终 Mock 版本成功编译并进入运行态，随后手动停止；当前环境没有 `DISPLAY`/`WAYLAND_DISPLAY`，没有生成截图或实际 Slider Rect 日志。因此正弦波 0/50/100、三角波 50、连续拖动、数字输入同步、1360×860、1600×900 和 1280×720/125% 的视觉验收仍为 `BLOCKED`，不能把代码检查当作截图通过证据。

有桌面会话时可执行：

```text
cargo run -p langmuir-desktop --features mock -- --mock
LANGMUIR_UI_DEBUG_RECTS=1 cargo run -p langmuir-desktop --features mock -- --mock
```

复测时确认 `left.scan_frequency_slider` 的 Response Rect 与父卡片宽度接近，轨道从 0 到 100 均清晰可见，数字输入和 1/50/100 按钮不再与轨道共行，且预览帧和 SUM 实时同步。

## 17. 中央工作区布局回归修复（2026-10-16）

本轮只修复中央 LVDS/UDP 控制区、实时图表区和系统日志区。顶部 Header、左侧 RS422 指令区、右侧遥测区以及 RS422、SUM、遥测解析、LVDS 解码、UDP、存储和真实设备通信逻辑均未修改。

### 截图问题的代码根因

审计中央旧布局后确认，八个现象分别来自同一组尺寸传播问题：

1. LVDS 控件父容器使用自然高度并允许 `horizontal_wrapped()`，IP、端口和操作按钮在仍有剩余宽度时被分裂到多行。
2. 辅助操作与监听状态继续参与自然宽度计算，导致左侧内容先换行、右侧留下空白。
3. 监听地址、协议说明和状态文本没有各自的固定行矩形，文本变化会影响父卡片的最小尺寸。
4. 图表标题、通道、模式和操作按钮共用可换行布局，缓存计数因剩余宽度不足而单独落行。
5. 空数据状态原先通过全高灰色 Frame/占位控件表达，既产生横条观感，也可能显示缺失字体字符。
6. 图表主体使用内容高度和最小高度共同参与布局，数据状态变化会改变外部高度。
7. 日志作为独立底部区域与中央图表争抢同一份可用高度，日志面板因此被挤出视野。
8. 各卡片由内容最小矩形决定外框，缺少统一的中央根矩形和固定间距，造成边界间距不一致。

### 实现与空间预算

`apps/desktop/src/app.rs` 的 `central_layout_rects()` 从 `CentralPanel` 的 `available_rect_before_wrap()` 生成四个不重叠矩形，并由 `scope_builder(UiBuilder::max_rect(...))` 直接承载内容：

```text
中央根区域 = A + 6 + B1 + 6 + B2 + 6 + C
A  LVDS / UDP 控制区   136 logical pt
B1 图表工具栏          68 logical pt
B2 图表主体             剩余高度（唯一弹性区域）
C  系统日志            160 logical pt
```

正常窗口下三段固定高度不随数据量、日志量、监听状态或按钮文本变化。极短窗口将 A、B1、C 按同一比例缩小，所有矩形仍限制在中央根区域内；中央根布局不再使用 `ScrollArea`，只有日志内容使用独立 `ScrollArea::vertical()`。

`operations_panel()` 采用固定行：第一行放 LVDS 状态、IP、端口、连接和开始/停止采集；第二行放刷新串口、CSV、Mock 异常和监听状态；第三行是截断的协议摘要。宽屏第一行控件不会随机换行；窄宽度使用明确的紧凑模式，把连接、采集和低频操作收进第二行/“更多操作”菜单。`plot_toolbar()` 在宽屏按通道、显示模式、图表操作/缓存三组单行排列，窄宽度固定为两行或菜单。`plot_panel()` 的无数据状态使用图表主体内的居中文字，不再绘制类似进度条的灰色横条。`log_panel()` 的标题、计数和底部矩形固定，日志条目只在内部滚动。

### 几何验证结果

已增加并编译几何测试：`central_layout_rects_partition_normal_workspace_without_overlap`、`central_layout_rects_remain_inside_tiny_workspace`、`central_layout_rects_geometry_is_content_independent` 和 `central_layout_rects_cover_requested_viewport_matrix`。它们检查区域在根矩形内、宽度一致、顺序无重叠、日志贴合底部，以及内容变化不影响矩形。

按当前固定侧栏和 Header 规则计算的 logical point 矩形如下：

| 窗口 | 中央根宽×高 | A | B1 | B2 | C | 结果 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1360×860 / 100% | 680×800 | 136 | 68 | 418 | 160 | PASS（几何） |
| 1600×900 / 100% | 920×840 | 136 | 68 | 458 | 160 | PASS（几何） |
| 1920×1080 / 100% | 1240×1020 | 136 | 68 | 638 | 160 | PASS（几何） |
| 1280×720 / 125% | 600×660 | 136 | 68 | 278 | 160 | PASS（几何） |
| 1024×768 / 150% | 444×664 | 136 | 68 | 282 | 160 | PASS（几何） |

调试开关 `LANGMUIR_UI_DEBUG_RECTS=1` 会记录中央根区域、操作区、IP/端口/连接/采集控件、图表工具栏、图表主体、图表空态和日志区域；每个标签仅在几何变化超过 1 logical point 时再次输出。日志标签为 `central.logs`，图表内部标签为 `central.plot_body_inner`，避免把卡片内边距误判成主体跳动。

### GUI、Mock 与回归状态

已执行 `LANGMUIR_UI_DEBUG_RECTS=1 cargo run -p langmuir-desktop --features mock -- --mock`：Mock 版本编译成功并进入运行态，随后手动停止；运行日志没有 Rust panic。当前环境没有 `DISPLAY`、`WAYLAND_DISPLAY`、`xvfb-run` 或可用截图会话，故未取得实际窗口 Rect、截图、按钮点击和 60 秒视觉观察。以下验收必须在有桌面会话的 Mac/Windows QA 机器完成，不能把本节几何 PASS 当作视觉 PASS：

- 1360×860、1600×900、1920×1080、1280×720/125% 和 1024×768/150% 的中央无整体滚动、控件同一行和日志可见性；
- IP 地址完整可读、连接/采集按钮对齐、图表空态无方框/灰色横条、日志内部滚动；
- Mock 连续运行 60 秒时中央五个区域 Rect 稳定，数据刷新和监听状态变化不改变 A/B1/C 高度；
- `target/ui-qa/` 中保存截图与 `LANGMUIR_UI_DEBUG_RECTS=1` 输出。

本轮验证命令结果：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo check --workspace --locked --offline` | PASS |
| `cargo check -p langmuir-desktop --features mock --locked --offline` | PASS |
| `cargo test -p langmuir-core --locked` | PASS，33 passed |
| `cargo test -p langmuir-core --locked --offline` | PASS，33 passed |
| `cargo test -p langmuir-protocol-cli --locked` | PASS，4 passed |
| `cargo test -p langmuir-protocol-cli --locked --offline` | PASS，4 passed |
| `cargo clippy -p langmuir-core -p langmuir-protocol-cli --lib --locked --offline -- -D warnings` | PASS |
| `cargo test -p langmuir-desktop --locked` | BLOCKED：`static.crates.io` DNS 失败，缺少 `rustix/fastrand` 等依赖 |
| `cargo test -p langmuir-desktop --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo test --workspace --locked` | BLOCKED：`static.crates.io` DNS 失败，无法下载缺失依赖 |
| `cargo test --workspace --locked --offline` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | BLOCKED：`static.crates.io` DNS 失败，无法下载缺失依赖 |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | BLOCKED：缓存缺少 `errno v0.3.14` |
| `git -c core.whitespace=cr-at-eol diff --check` | PASS |

本轮没有执行 Git 提交、重置、清理或真实设备操作。当前视觉验收仍为 BLOCKED；完成上述有桌面会话复测前，不应宣称中央 UI 已通过最终截图验收。

复核后又补充了两处直接相关的紧凑布局处理：分屏模式只显示一个通道时，该通道现在占用全部图表弹性高度；最小中央宽度下，图表通道选择改为明确的“通道”菜单，LVDS 地址/端口输入收缩到可用宽度，避免在 800×600 最小窗口中产生隐式第三行或横向裁切。两处均未改变数据解码、通道语义或控制请求。
