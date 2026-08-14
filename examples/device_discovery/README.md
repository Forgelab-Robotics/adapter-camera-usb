# 设备发现

从仓库目录运行：

```bash
cargo run --bin usb_camera -- list-devices
cargo run --bin usb_camera -- list-devices --json
```

该命令会查询 V4L2 设备能力，可能访问视频设备。若只需在不打开硬件的前提下检查系统与设备节点权限，请运行：

```bash
cargo run --bin usb_camera -- check-environment
```

设备型号、序列号和可用格式需在目标硬件上验证，并保存脱敏后的验收记录。
