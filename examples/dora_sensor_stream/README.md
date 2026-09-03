# Dora sensor stream

此示例固定为 `sensor/image -> sink/image`。sink 解码 `forge_msgs.Image` 或 `forge_msgs.CompressedImage`，逐帧打印计数、编码、尺寸、字节数和 sink 接收时的 Unix 毫秒时间。

从仓库根目录构建：

```bash
cargo build --bins
```

复制 `sensor_node.yaml` 并按目标设备修改后，使用 Dora CLI `1.0.0` 在本目录运行。Dora 0.x 与 1.x 节点不能互通：

```bash
dora run dataflow.yaml
```

默认 `fps: 30` 对应 33 ms tick。`raw` 产生 `Image`；`jpeg`/`png` 产生 `CompressedImage`。示例路径均相对本目录，不包含个人绝对路径。

当前基线设备的协商分辨率/帧率及 raw/jpeg/png sink 解码已验证。目标部署机仍需自行验收 USB 拓扑、长时间稳定性、丢帧率和拔插恢复。
