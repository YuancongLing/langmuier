# AB 激励 CSV v2：状态帧与采样时间

适用于 S02-REAL-02。每个接纳的 AB 帧仍保存 100 行原始 wave 槽位；第一行是唯一表头，不添加注释行。所有输出独占创建，不覆盖已有 CSV。

v1 的前 22 列名称、顺序和有采样时间帧的数值含义不变。v2 在末尾追加 `interval_us,points,waveform_status`，共 25 列；完整表头如下：

```text
ab_frame_index,raw_record_id,udp_sequence_u32,pa0_seq,sci_frame_seq,frame_seconds,frame_microseconds,sample_index,sample_time_us,wave_mV,wave_V,f_act_mHz,f_act_Hz,work_mode,scan_freq,scan_voltage_mV,ad9833_status,dac8563_status,integrity,source,session,received_unix_nanos,interval_us,points,waveform_status
```

| 字段 | v2 语义 |
|---|---|
| `sample_index` | 原始槽位索引 0–99，所有状态帧也保留全部槽位 |
| `sample_time_us` | 有采样时间时为设备微秒整数；无采样时间时为空字符串，不写 0、不复制最新时间 |
| `frame_seconds,frame_microseconds` | 始终原样保留设备包时间，不能在空采样时间时当作100个逐点时间 |
| `wave_mV` | 始终保存原始 u16 槽位；全零状态的占位值也保留 |
| `wave_V` | 始终为原始槽位的单位换算 `wave_mV/1000`，不是独立的有效测量判据 |
| `interval_us` | 原始帧内采样间隔，0 表示没有可用逐点时间 |
| `points` | 原始 points 字段，当前合法 AB 固定为100 |
| `waveform_status` | `TIMED_SAMPLES`（interval_us>0）或 `NO_SAMPLE_TIME`（interval_us=0） |
| `integrity` | 始终为 `CRC_NOT_CHECKED`，没有新增生产 CRC 门禁 |

`TIMED_SAMPLES` 表示能计算逐点时间，不额外承诺硬件有效性。其时间仍按 `latest−(99−i)×interval_us` 倒推，wave[99] 对应原始最新点时间。帧按接纳顺序保存，相邻窗口重叠时不去重、不拼接。

`NO_SAMPLE_TIME` 行的 `wave_V=0.000` 只表示原始零槽位的数值换算，**不表示已经测得0V**。读取方应先检查状态及空时间，保留配置/状态记录，并跳过将其绘制成采样曲线。三角波0Hz与DC无有效数据状态均不因此被拒收。DC的 `scan_voltage_mV` 是设定值，不能代替 wave 构造实测水平线。

兼容读取规则：

1. 按表头名称读取列，允许末尾追加字段；固定只接受22列的读取器需要升级。
2. 将 `sample_time_us` 读取为可空整数，不把空值转为0、最新包时间或上一条时间。
3. 对v2优先判断 `waveform_status`。旧v1文件缺少采样间隔与状态，不能仅凭100个相同时间推断它们一定是有效采样。
4. 旧v1文件和验收证据保持原样；新版保存与历史AB导出统一输出v2。应用的历史打开入口仍为PCAPNG、LVDSRAW、LPD，不新增CSV历史导入。

本轮真实多模式抓包的目标读回口径为175帧/17500行，其中9300行有采样时间、8200行采样时间为空。实际结果与文件身份见 `LVDS_MULTITYPE_S02_REAL_202611212141_ACCEPTANCE.md`。
