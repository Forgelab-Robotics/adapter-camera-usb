//! Linux V4L2：枚举可用 **RGB 彩色** 采集节点（排除典型红外/深度子设备）。

use eyre::Result;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;

/// 红外、深度或单通道灰度格式。部分深度相机节点会同时声明 MJPG，
/// 因此不能仅凭“存在 MJPG/YUYV”判断该节点是 RGB。
fn is_non_rgb_sensor_format(repr: &[u8; 4]) -> bool {
    matches!(
        repr,
        [b'G', b'R', b'E', b'Y']
            | [b'Y', b'8', b'0', b'0']
            | [b'Y', b'8', b' ', b' ']
            | [b'Y', b'1', b'0', b' ']
            | [b'Y', b'1', b'2', b' ']
            | [b'Y', b'1', b'4', b' ']
            | [b'Y', b'1', b'6', b' ']
            | [b'Z', b'1', b'6', b' ']
            | [b'I', b'N', b'V', b'Z']
            | [b'Z', b'1', b'6', b'C']
            | [b'D', b'1', b'6', b' ']
    )
}

/// 当前 backend 能可靠解码的彩色像素格式。
fn is_supported_color_format(repr: &[u8; 4]) -> bool {
    matches!(
        repr,
        [b'M', b'J', b'P', b'G']
            | [b'Y', b'U', b'Y', b'V']
            | [b'Y', b'U', b'Y', b'2']
            | [b'R', b'G', b'B', b'3']
            | [b'B', b'G', b'R', b'3']
    )
}

#[cfg(target_os = "linux")]
pub fn list_rgb_video_devices() -> Result<Vec<(String, String)>> {
    use v4l::capability::Flags;
    use v4l::device::Device;
    use v4l::video::Capture;

    /// 名称上明显是红外/深度/元数据时排除（多节点共用同一 card 时主要靠像素格式）。
    fn name_suggests_non_rgb_camera(name: &str) -> bool {
        let n = name.to_lowercase();
        if n.contains("infrared")
            || n.contains("infra-red")
            || n.contains(" ir ")
            || n.ends_with(" ir")
            || n.contains("metadata")
            || n.contains("point cloud")
        {
            return true;
        }
        if n.contains("depth")
            && !n.contains("rgb")
            && !n.contains("color")
            && !n.contains("colour")
        {
            return true;
        }
        if n.contains("mono") && !n.contains("rgb") && !n.contains("color") {
            return true;
        }
        false
    }

    let v4l_dir = "/sys/class/video4linux";
    let entries = match fs::read_dir(v4l_dir) {
        Ok(e) => e,
        Err(_) => return Ok(vec![]),
    };
    let mut list = Vec::new();
    for entry in entries.flatten() {
        let dev_name = entry.file_name();
        let dev_str = match dev_name.to_str() {
            Some(s) if s.starts_with("video") => s,
            _ => continue,
        };
        let device_path = format!("/dev/{}", dev_str);
        let dev = match Device::with_path(&device_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let caps = match dev.query_caps() {
            Ok(c) => c,
            Err(_) => continue,
        };
        // v4l crate 的 Capabilities 已使用内核 device_caps，避免误用父设备聚合能力。
        let has_video = caps.capabilities.contains(Flags::VIDEO_CAPTURE);
        let has_meta_only = caps.capabilities.contains(Flags::META_CAPTURE) && !has_video;
        if has_meta_only || !has_video {
            continue;
        }
        let name = caps.card.trim().to_string();
        let name = if name.is_empty() {
            let name_path = Path::new(v4l_dir).join(dev_str).join("name");
            fs::read_to_string(&name_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "Unknown Camera".to_string())
        } else {
            name
        };
        if name_suggests_non_rgb_camera(&name) {
            continue;
        }
        let formats = match dev.enum_formats() {
            Ok(formats) => formats,
            Err(_) => continue,
        };
        if formats
            .iter()
            .any(|d| is_non_rgb_sensor_format(&d.fourcc.repr))
        {
            continue;
        }
        if !formats
            .iter()
            .any(|d| is_supported_color_format(&d.fourcc.repr))
        {
            continue;
        }
        list.push((name, device_path));
    }
    list.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(list)
}

#[cfg(not(target_os = "linux"))]
pub fn list_rgb_video_devices() -> Result<Vec<(String, String)>> {
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::{is_non_rgb_sensor_format, is_supported_color_format};

    #[test]
    fn rejects_depth_or_ir_formats_even_when_mjpeg_is_available() {
        let depth_node_formats = [*b"Y10 ", *b"GREY", *b"MJPG", *b"Y16 "];
        assert!(depth_node_formats.iter().any(is_non_rgb_sensor_format));
        assert!(depth_node_formats.iter().any(is_supported_color_format));
    }

    #[test]
    fn accepts_normal_rgb_format_sets() {
        let rgb_node_formats = [*b"YUYV", *b"MJPG"];
        assert!(!rgb_node_formats.iter().any(is_non_rgb_sensor_format));
        assert!(rgb_node_formats.iter().any(is_supported_color_format));
    }
}
