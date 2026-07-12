# Sample output

硬件验收时可在本目录临时生成：

- `snapshot.jpg`：`capture_sample` 的单帧输出。
- sink 日志：包含计数、消息类型、编码、尺寸、字节数和接收时间。

2026-07-12 使用 `HD USB Camera` 完成脱敏日志验证：

```text
message=Image encoding=rgb8 width=640 height=480 bytes=921600
message=CompressedImage encoding=jpeg width=640 height=480 bytes≈58500
message=CompressedImage encoding=png width=640 height=480 bytes=922218
```

PNG 使用吞吐优先的快速、无滤波编码，因此该场景下体积接近 raw；JPEG 更适合实时链路。

两次 snapshot 均识别为 640×480、8-bit、3 通道 JPEG。实际图片仅用于本地验证并由
`.gitignore` 排除；不要提交真实场景图片、大型视频或 MCAP。
