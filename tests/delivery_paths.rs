use std::fs;
use std::path::{Path, PathBuf};

use arrow_array::{RecordBatch, StructArray};
use bytes::Bytes;
use forge_msgs::{CompressedImage, Image};
use serde_yaml::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_yaml(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_yaml::from_str(&text)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

#[test]
fn delivery_config_and_docs_are_at_stable_paths() {
    let root = root();
    for relative in [
        "LICENSE",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "RELEASING.md",
        ".github/workflows/ci.yml",
        "config/sensor.example.yaml",
        "examples/device_discovery/README.md",
        "examples/capture_sample/README.md",
        "examples/dora_sensor_stream/dataflow.yaml",
        "examples/dora_sensor_stream/sensor_node.yaml",
        "examples/dora_sensor_stream/test_sink.yaml",
        "assets/README.md",
        "sample_output/README.md",
        "scripts/install_permissions.sh",
    ] {
        assert!(root.join(relative).is_file(), "missing {relative}");
    }
    assert!(!root.join("usb_camera.example.yaml").exists());
}

#[test]
fn dora_dataflow_connects_sensor_to_sink_with_relative_paths() {
    let root = root();
    let example_dir = root.join("examples/dora_sensor_stream");
    let dataflow = read_yaml(&example_dir.join("dataflow.yaml"));
    let nodes = dataflow["nodes"]
        .as_sequence()
        .expect("nodes must be a list");
    assert_eq!(nodes.len(), 2);

    let sensor = nodes
        .iter()
        .find(|node| node["id"] == "sensor")
        .expect("sensor node");
    let sink = nodes
        .iter()
        .find(|node| node["id"] == "sink")
        .expect("sink node");

    let sensor_path = sensor["path"].as_str().expect("sensor path");
    let sink_path = sink["path"].as_str().expect("sink path");
    assert!(!Path::new(sensor_path).is_absolute());
    assert!(!Path::new(sink_path).is_absolute());
    assert_eq!(sensor["outputs"][0], "image");
    assert_eq!(sink["inputs"]["image"], "sensor/image");
    assert!(
        sensor["args"]
            .as_str()
            .unwrap()
            .contains("sensor_node.yaml")
    );
    assert!(sink["args"].as_str().unwrap().contains("test_sink.yaml"));

    let sensor_config = read_yaml(&example_dir.join("sensor_node.yaml"));
    let sink_config = read_yaml(&example_dir.join("test_sink.yaml"));
    assert_eq!(sensor_config["output_id"], "image");
    assert_eq!(sink_config["input_id"], "image");
}

#[test]
fn forge_image_message_paths_round_trip() {
    let raw = Image::new(1, 2, "rgb8", 6, Bytes::from_static(&[1, 2, 3, 4, 5, 6]))
        .expect("valid raw image");
    let raw_batch = raw.to_record_batch().expect("raw batch");
    let raw_struct: StructArray = raw_batch.into();
    let decoded_raw =
        Image::from_record_batch(&RecordBatch::from(raw_struct)).expect("decode raw image");
    assert_eq!(decoded_raw, raw);

    for (format, payload) in [
        ("jpeg", Bytes::from_static(&[0xff, 0xd8, 0xff, 0xd9])),
        ("png", Bytes::from_static(b"\x89PNG\r\n\x1a\n")),
    ] {
        let compressed = CompressedImage::new(format, payload).expect("valid compressed envelope");
        let compressed_batch = compressed.to_record_batch().expect("compressed batch");
        let compressed_struct: StructArray = compressed_batch.into();
        let decoded_compressed =
            CompressedImage::from_record_batch(&RecordBatch::from(compressed_struct))
                .expect("decode compressed image");
        assert_eq!(decoded_compressed, compressed);
    }
}
