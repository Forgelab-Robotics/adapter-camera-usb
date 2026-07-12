use std::env;
use std::fs;
use std::path::Path;

use eyre::{Result, WrapErr, bail};
use serde::Deserialize;

/// USB Camera 节点配置。
#[derive(Debug, Clone, Deserialize)]
pub struct CameraConfig {
    /// 摄像头设备地址，如 `/dev/video0` 或 `"0"`（索引）。
    #[serde(default = "default_device")]
    pub device: String,

    /// 输出到 dataflow 的 id，与 mujoco/simulator 的 image 输出语义一致。
    #[serde(default = "default_output_id")]
    pub output_id: String,

    /// 输出图像格式：raw（rgb8）、jpeg、png。
    #[serde(default = "default_image_format")]
    pub image_format: ImageFormat,

    /// jpeg 编码质量 [1, 100]，仅当 image_format 为 jpeg 时有效。
    #[serde(default = "default_jpeg_quality")]
    pub image_jpeg_quality: u8,

    /// 期望宽度，None 表示使用设备默认。
    pub width: Option<u32>,

    /// 期望高度，None 表示使用设备默认。
    pub height: Option<u32>,

    /// 期望帧率，None 表示不限制（每 tick 一帧）。
    pub fps: Option<f32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Raw,
    Jpeg,
    Png,
}

fn default_device() -> String {
    "/dev/video0".to_string()
}

fn default_output_id() -> String {
    "image".to_string()
}

fn default_image_format() -> ImageFormat {
    ImageFormat::Raw
}

fn default_jpeg_quality() -> u8 {
    90
}

impl CameraConfig {
    /// 从路径加载 YAML 配置。
    pub fn from_yaml_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read config file: {}", path.display()))?;
        let cfg: CameraConfig = serde_yaml::from_str(&content)
            .wrap_err_with(|| format!("failed to parse yaml: {}", path.display()))?;
        cfg.validate()
    }

    fn validate(self) -> Result<Self> {
        if self.device.trim().is_empty() {
            bail!("device must not be empty");
        }
        if self.output_id.trim().is_empty() {
            bail!("output_id must not be empty");
        }
        if self.image_jpeg_quality < 1 || self.image_jpeg_quality > 100 {
            bail!("image_jpeg_quality must be in [1, 100]");
        }
        if matches!(self.width, Some(0)) {
            bail!("width must be positive if specified");
        }
        if matches!(self.height, Some(0)) {
            bail!("height must be positive if specified");
        }
        if let Some(fps) = self.fps
            && (!fps.is_finite() || fps < 1.0)
        {
            bail!("fps must be finite and at least 1 if specified");
        }
        Ok(self)
    }

    /// 加载配置，优先级：
    /// 1. 显式传入的 config_path
    /// 2. 环境变量 USB_CAMERA_NODE_CONFIG
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        if let Some(path) = config_path {
            return Self::from_yaml_path(path);
        }

        if let Ok(env_path) = env::var("USB_CAMERA_NODE_CONFIG")
            && !env_path.is_empty()
        {
            return Self::from_yaml_path(env_path);
        }

        bail!("no camera config found. set USB_CAMERA_NODE_CONFIG or pass --config <path.yaml>");
    }

    /// 用于 --snapshot 的默认配置：仅指定设备，输出 JPEG。
    pub fn for_snapshot(device: String) -> Result<Self> {
        let cfg = Self {
            device,
            output_id: default_output_id(),
            image_format: ImageFormat::Jpeg,
            image_jpeg_quality: default_jpeg_quality(),
            width: Some(1280),
            height: Some(720),
            fps: None,
        };
        cfg.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/sensor.example.yaml");
        let config = CameraConfig::from_yaml_path(path).expect("example config must be valid");
        assert_eq!(config.output_id, "image");
        assert!(matches!(config.image_format, ImageFormat::Jpeg));
    }

    #[test]
    fn invalid_ranges_are_rejected() {
        let invalid_quality = r#"
device: /dev/video0
image_jpeg_quality: 0
"#;
        let config: CameraConfig = serde_yaml::from_str(invalid_quality).unwrap();
        assert!(config.validate().is_err());

        let sub_one_fps = r#"
device: /dev/video0
fps: 0.49
"#;
        let config: CameraConfig = serde_yaml::from_str(sub_one_fps).unwrap();
        assert!(config.validate().is_err());

        let invalid_fps = r#"
device: /dev/video0
fps: 0
"#;
        let config: CameraConfig = serde_yaml::from_str(invalid_fps).unwrap();
        assert!(config.validate().is_err());

        for non_finite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let config = CameraConfig {
                device: "/dev/video0".to_string(),
                output_id: "image".to_string(),
                image_format: ImageFormat::Raw,
                image_jpeg_quality: 90,
                width: None,
                height: None,
                fps: Some(non_finite),
            };
            assert!(config.validate().is_err());
        }
    }
}
