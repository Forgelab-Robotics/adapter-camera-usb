# Capture sample

使用统一示例配置采集一帧并保存为 JPEG：

```bash
cargo run --bin usb_camera -- snapshot \
  --config config/sensor.example.yaml \
  --output sample_output/snapshot.jpg
```

命令会打开配置中的 V4L2 设备。执行前先复制并调整配置，确保分辨率和帧率在设备能力范围内。生成图片仅用于本地验收，不应提交大型样本或包含敏感画面的文件。
