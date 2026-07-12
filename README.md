# USB Camera

纯 Rust 的 Linux USB/UVC 彩色相机接入，保留独立的配置模型、采集 backend、CLI 和 Dora 节点。支持 V4L2 `/dev/video*`，不引入 Python。已完成一款设备的硬件基线验收；其他设备、固件和驱动组合仍需分别验证，见 `hardware_baseline.md`。

## 支持范围

- 传感器类型：通用 RGB USB/UVC 相机。
- 连接方式：Linux V4L2 视频设备。
- 构建：`cargo build --bins` 或 `cargo build --release --bins`。
- 官方工具建议：使用发行版提供的 `v4l2-ctl --list-devices` 和 `v4l2-ctl --list-formats-ext -d /dev/videoN` 验证；当前基线结果已记录在 `hardware_baseline.md`。

## 安装与权限

安装支持 Rust 2024 edition 的 toolchain、Linux V4L2 开发依赖和 `v4l-utils`。
Ubuntu/Debian 可执行：

```bash
sudo apt install build-essential clang libclang-dev libv4l-dev v4l-utils
```

其他发行版的软件包名需由部署环境确认。

先执行不会打开设备的环境检查：

```bash
cargo run --bin usb_camera -- check-environment
cargo run --bin usb_camera -- check-environment --json
```

该命令只读取 OS、`/dev/video*` 目录项和权限元数据，不调用设备 `open`/V4L2 ioctl。当前用户不属于 `video` 组时：

```bash
sudo scripts/install_permissions.sh
```

重新登录后再次自检。脚本只把调用用户加入 `video` 组，不写入或覆盖系统 udev
规则，也不会安装厂商 SDK。

## 配置

唯一交付示例为 `config/sensor.example.yaml`：

```bash
cargo run --bin usb_camera -- run --config config/sensor.example.yaml
```

也可设置 `USB_CAMERA_NODE_CONFIG`。字段如下：

- `device`：设备路径或数字索引；示例 `/dev/video0`。
- `output_id`：Dora output id，必须与 dataflow `outputs` 对齐。
- `image_format`：`raw`、`jpeg`、`png`。
- `image_jpeg_quality`：JPEG 质量 `1..=100`。
- `width`/`height`：期望尺寸；驱动可能协商为其他实际值。
- `fps`：期望设备帧率，必须至少为 1；Dora 输出节奏仍由 `tick` 决定。

配置修改后需要重启节点/采集会话；当前不支持运行中动态调参。

## 发现与单帧采集

设备发现会查询 V4L2 能力：

```bash
cargo run --bin usb_camera -- list-devices
cargo run --bin usb_camera -- list-devices --json
```

采集 JPEG 单帧：

```bash
cargo run --bin usb_camera -- snapshot \
  --config config/sensor.example.yaml \
  --output sample_output/snapshot.jpg
```

对应说明见 `examples/device_discovery/` 与 `examples/capture_sample/`。snapshot 会丢弃预热帧、跳过疑似黑帧并对间歇错误做有限重试，因此会实际打开硬件。

## Dora example

`examples/dora_sensor_stream/` 提供完整 `sensor -> sink` 链路。先构建二进制，再进入该目录运行：

```bash
cargo build --bins
cd examples/dora_sensor_stream
dora run dataflow.yaml
```

该示例的 `dataflow.yaml` 固定引用 `target/debug/`，因此必须使用不带 `--release`
的构建命令。若部署 release 产物，需将两个节点路径改为 `target/release/`。

sink 是 Rust 可执行节点，能解码 `forge_msgs.Image` 和 `forge_msgs.CompressedImage`，打印帧计数、编码、尺寸、数据字节数及 sink 接收时的 Unix 毫秒时间。

## 消息与时间戳语义

- `image_format: raw`：输出 `forge_msgs.Image`，当前编码为 `rgb8`，布局为 HWC、行连续。
- `image_format: jpeg`：输出 `forge_msgs.CompressedImage(format="jpeg")`；有效 MJPEG 可校验后直通。
- `image_format: png`：输出 `forge_msgs.CompressedImage(format="png")`；使用吞吐优先的快速、无滤波编码，文件可能接近 raw 大小。
- JPEG 优先向设备请求 MJPG；raw/PNG 优先请求 YUYV，实际格式仍以驱动协商结果为准。
- YUYV 转换使用驱动协商出的行步长、量化范围和色彩空间；启动日志会记录最终协商结果。
- 消息本体当前没有独立采集时间戳字段。
- 节点发送时原样传递触发该帧的 Dora `tick` metadata parameters；其时间含义是触发/编排时间，不应宣称为相机曝光时间。
- test sink 的 `received_at_unix_ms` 是 sink 进程收到并处理消息时的系统墙钟时间，也不是硬件采集时间。

## 验证与交付文档

- 参数验证：`parameter_validation.md`
- 硬件基线：`hardware_baseline.md`
- 样本策略：`sample_output/README.md`
- 资产策略：`assets/README.md`
- 无硬件测试：`cargo test`

## 常见问题与限制

- 仅实现 Linux V4L2 backend；其他系统的环境检查会报告不支持。
- `Permission denied`：确认设备节点 group/mode、用户属于 `video` 组且已重新登录。
- `Device or resource busy`：关闭浏览器、VLC 或其他相机进程。
- 找不到设备：检查 USB、内核日志和 `/dev/video*`；再用官方工具确认。
- 分辨率/帧率不符：配置是期望值，最终由设备驱动协商，必须在硬件基线中记录实际值。
- 同一设备通常不能被多个进程同时采集。
- 当前真机 640×480 下，完整 Dora sink 链路中 JPEG、raw 和快速 PNG 均约
  30 FPS；快速 PNG 单帧约 922 KB，实时传输通常仍应优先使用 JPEG，详见
  `hardware_baseline.md`。
- raw/jpeg/png、60 秒连续运行及异常退出重开已验证；拔插恢复、数分钟以上稳定性和
  丢帧统计仍待人工验收。
