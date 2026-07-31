use bytes::Bytes;
use eyre::Result;

use crate::config::ImageFormat;

/// 原始采集帧的像素格式。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    /// MJPEG 压缩帧。
    Mjpeg,
    /// RGB888，HWC，连续内存。
    Rgb8,
    /// BGR888，HWC，连续内存。
    Bgr8,
    /// YUYV / YUY2（4:2:2 packed），每像素 2 字节。
    Yuyv,
    /// 灰度，单通道（如 V4L2 GREY / Y800）。
    Gray8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvRange {
    Full,
    Limited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvColorimetry {
    pub range: YuvRange,
    pub matrix: YuvMatrix,
}

/// 从后端采集到的一帧数据。
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// 每行实际占用的字节数；压缩格式为 0。
    pub stride: usize,
    pub pixel_format: PixelFormat,
    pub yuv_colorimetry: Option<YuvColorimetry>,
    /// 后端取得该物理帧时记录的 Unix epoch 纳秒时间。
    pub capture_timestamp_ns: Option<i64>,
    pub data: Bytes,
}

/// 采集后端统一接口。
pub trait CaptureBackend {
    fn capture_frame(&mut self) -> Result<CapturedFrame>;

    /// 阻塞直到收到**下一帧**（不返回缓存的旧帧）。用于 snapshot 先丢弃若干帧再拍摄。
    fn capture_fresh_frame(&mut self) -> Result<CapturedFrame> {
        self.capture_frame()
    }
}

/// 为当前平台创建合适的采集后端。
pub fn create_backend(config: &crate::config::CameraConfig) -> Result<Box<dyn CaptureBackend>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(crate::backend_v4l::V4lBackend::new(
            config.clone(),
        )?))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        eyre::bail!("camera backend is only implemented for Linux at the moment");
    }
}

/// 根据底层帧和期望的输出格式，判断是否可以直接使用 MJPEG 直通。
pub fn can_direct_use_mjpeg(pixel_format: PixelFormat, image_format: ImageFormat) -> bool {
    matches!(pixel_format, PixelFormat::Mjpeg) && matches!(image_format, ImageFormat::Jpeg)
}
