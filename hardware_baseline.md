# 硬件基线

验证日期：2026-07-12。

- 设备：`HD USB Camera: HD USB Camera`，USB VID:PID `32e4:9230`
- 唯一标识：设备未暴露硬件序列号；稳定部署应优先使用 `/dev/v4l/by-path/`
- USB revision：`0100`；连接路径报告为 USB 2
- 系统：Ubuntu 24.04.3 LTS，Linux 6.17.0-35-generic，x86_64
- 驱动：`uvcvideo` 6.17.13
- RGB 节点：`/dev/video0`；`/dev/video1` 为非 RGB/辅助节点，设备发现会过滤
- 权限：`/dev/video0`、`/dev/video1` 均为 `root:video 0660`
- 官方工具：`v4l2-ctl --all --list-formats-ext -d /dev/video0` 验证通过

## V4L2 能力摘要

- MJPG：1920×1080@30、1280×720@60、1024×768@30、640×480@120.101、
  800×600@60、1280×1024@30、320×240@120.101
- YUYV：1920×1080@6、1280×720@9、1024×768@6、640×480@30、
  800×600@20、1280×1024@6、320×240@30
- 示例 JPEG 配置 640×480、请求 30 FPS，实际协商 MJPG，snapshot 输出 640×480 JPEG
- raw/PNG 配置实际协商 YUYV、stride 1280、sRGB colorspace、default
  quantization；转换按 V4L2 默认规则使用 limited-range BT.601

## 已完成验证

- 连续两次打开、抓图和释放设备成功
- Dora JPEG 链路连续运行 60 秒，正常 Ctrl+C/STOP 后可立即重新打开
- Dora raw/rgb8、JPEG、PNG 均由 test sink 成功解码
- 与 Orbbec Gemini 深度相机同时连接时，设备发现会排除其 `/dev/video2`、
  `/dev/video4` 深度/红外节点，仅保留 RGB 节点 `/dev/video6`
- 修正 mmap `bytesused` 和 MJPEG 直通后，640×480 JPEG 在完整
  sensor→Dora→sink 解码链路稳定约 30 FPS
- raw/PNG 改为 YUYV 采集、紧凑双像素 RGB 转换后，640×480 raw/rgb8
  在完整链路约 30 FPS
- PNG 使用快速压缩、无滤波后约 30 FPS，但单帧约 922 KB，几乎不节省带宽；
  需要较小文件时应使用 JPEG，PNG 更适合明确要求无损的场景
- 节点启动日志可记录最终协商的 FourCC、尺寸、stride、colorspace 和 quantization

## 仍需人工验收

- 拔插过程中的断流检测与重新枚举
- 更长时间（数分钟以上）的连续采集、丢帧率和温升
- 目标部署机上的 USB 拓扑、CPU 性能和最终帧率
- 多进程占用冲突行为

不得提交隐私图像、大型视频或 MCAP。
