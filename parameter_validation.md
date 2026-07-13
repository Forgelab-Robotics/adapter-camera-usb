# 参数验证

## 无硬件可执行

- `config/sensor.example.yaml` 可被配置模型解析。
- `image_jpeg_quality` 仅允许 `1..=100`。
- `width`、`height` 指定时必须大于零；`fps` 必须为有限值且至少为 1。
- `output_id` 必须与 Dora dataflow 的 output 和 sink input 路径一致。
- `raw` 的消息类型为 `forge_msgs.Image`；`jpeg`/`png` 为 `forge_msgs.CompressedImage`。

以上配置解析、Image/CompressedImage 消息封装 round-trip 和 dataflow 静态路径由
单元测试及 `tests/delivery_paths.rs` 覆盖；真实 JPEG/PNG 解码与 Dora 运行行为由
下方真机验收覆盖。

## 真机验证（2026-07-12）

- 默认示例配置可打开 `/dev/video0`：通过
- `list-devices` 在 Orbbec Gemini 多视频节点场景下仅返回其 RGB
  `/dev/video6`，排除 `/dev/video2`、`/dev/video4` 深度/红外节点：通过
- 请求 640×480 后，JPEG 实际协商 MJPG，raw/PNG 实际协商 YUYV：通过
- raw/PNG 协商 stride 1280、sRGB/default quantization；按 V4L2 默认
  limited-range BT.601 转换并正确跳过行 padding：通过
- 请求 30 FPS；本机完整 Dora sensor→sink 链路实测 JPEG、raw/rgb8 和快速
  PNG 均约 30 FPS
- 快速 PNG 单帧约 922 KB；该模式以吞吐优先，不代表压缩体积优于 JPEG
- `image_format` 为 raw/jpeg/png 时，sink 分别解码为 `Image(rgb8)`、
  `CompressedImage(jpeg)`、`CompressedImage(png)`：通过
- 连续两次 snapshot 均输出 640×480 JPEG，证明正常退出后可立即重新打开：通过
- 同场景 snapshot 中 JPEG quality 90 为 47,860 bytes，quality 50 为 35,385
  bytes，文件大小变化可观察；视觉质量不提交真实场景图
- 上述设备参数均需重启采集会话后生效：已确认
- 运行中动态调参：当前未实现；修改 YAML 后需重启节点
- 拔插恢复、数分钟以上稳定性和丢帧统计：待人工验收
