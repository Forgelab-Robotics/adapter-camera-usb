mod backend;
#[cfg(target_os = "linux")]
mod backend_v4l;
mod config;
mod list_devices;

use std::fs;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use arrow_array::{RecordBatch, StructArray};
use bytes::Bytes;
use clap::{Parser, Subcommand};
use dora_node_api::{
    DoraNode, Event, EventStream, MetadataParameters, Parameter, dora_core::config::DataId,
};
use eyre::{Result, WrapErr};
use forge_common::logger::init_tracing;
use forge_msgs::{CompressedImage, Image};
use image as image_crate;
use image::ImageEncoder;
use ndarray::Array3;
use serde::Serialize;
use tracing::{error, info, warn};

use crate::backend::{
    CaptureBackend, CapturedFrame, PixelFormat, YuvColorimetry, YuvMatrix, YuvRange,
    can_direct_use_mjpeg,
};
use crate::config::{CameraConfig, ImageFormat};

const CAPTURE_TIMESTAMP_KEY: &str = "capture_timestamp_ns";

/// 判断是否为「摄像头已断开」类错误，此类错误应让节点退出而非仅打日志。
fn is_usb_camera_disconnected_error(msg: &str) -> bool {
    msg.contains("摄像头已断开连接")
        || msg.contains("摄像头采集线程已退出")
        || msg.contains("设备可能已被拔出")
}

/// CLI 子命令：仅执行后退出，不启动 Dora。
#[derive(Debug, Clone)]
enum CliCommand {
    /// 正常启动 Dora 节点（需 --config 或环境变量）
    Run { config_path: Option<String> },
    /// 列出可用视频设备（每行 name\taddress）
    ListDevices { json: bool },
    /// 只检查运行环境和设备节点权限，不打开摄像头
    CheckEnvironment { json: bool },
    /// 对指定设备截一帧保存为 JPEG
    Snapshot {
        config_path: Option<String>,
        device: Option<String>,
        output: String,
    },
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "usb_camera",
    version,
    about = "Camera node with utility subcommands"
)]
struct Cli {
    /// 启动节点时指定配置文件路径
    #[arg(long)]
    config: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
    /// 兼容旧参数：列出可用视频设备
    #[arg(long, hide = true)]
    list_devices: bool,
    /// 兼容旧参数：对指定设备截一帧
    #[arg(long, hide = true)]
    snapshot: Option<String>,
    /// 兼容旧参数：snapshot 输出文件路径
    #[arg(long, hide = true, default_value = "snapshot.jpg")]
    output: String,
    /// 以 JSON 格式输出工具命令结果（当前用于 list-devices）
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Subcommand)]
enum Commands {
    /// 正常启动 Dora 节点（需 --config 或环境变量）
    Run {
        #[arg(long)]
        config: Option<String>,
    },
    /// 列出可用视频设备（每行 name\taddress）
    ListDevices {
        /// 以 JSON 数组输出设备列表
        #[arg(long)]
        json: bool,
    },
    /// 检查 Linux、/dev/video* 和权限位；不会打开硬件
    CheckEnvironment {
        /// 以 JSON 输出检查结果
        #[arg(long)]
        json: bool,
    },
    /// 对指定设备截一帧保存为 JPEG
    Snapshot {
        #[arg(long, short = 'd')]
        device: Option<String>,
        #[arg(long)]
        config: Option<String>,
        #[arg(long, short = 'o', default_value = "snapshot.jpg")]
        output: String,
    },
}

fn parse_args() -> CliCommand {
    let Cli {
        config: top_level_config,
        command,
        list_devices,
        snapshot,
        output,
        json,
    } = Cli::parse();

    if list_devices {
        return CliCommand::ListDevices { json };
    }
    if let Some(device) = snapshot {
        return CliCommand::Snapshot {
            config_path: top_level_config,
            device: Some(device),
            output,
        };
    }

    match command {
        Some(Commands::ListDevices { json: command_json }) => CliCommand::ListDevices {
            json: command_json || json,
        },
        Some(Commands::CheckEnvironment { json: command_json }) => CliCommand::CheckEnvironment {
            json: command_json || json,
        },
        Some(Commands::Snapshot {
            device,
            config,
            output,
        }) => CliCommand::Snapshot {
            config_path: config.or(top_level_config),
            device,
            output,
        },
        Some(Commands::Run { config }) => CliCommand::Run {
            config_path: config.or(top_level_config),
        },
        None => CliCommand::Run {
            config_path: top_level_config,
        },
    }
}

#[derive(Debug, Serialize)]
struct ListedDevice {
    name: String,
    address: String,
}

#[derive(Debug, Serialize)]
struct EnvironmentCheck {
    os: &'static str,
    supported: bool,
    video_devices: Vec<VideoDevicePermission>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct VideoDevicePermission {
    path: String,
    mode: String,
    owner_uid: u32,
    owner_gid: u32,
    has_read_bit: bool,
    has_write_bit: bool,
}

fn inspect_environment() -> EnvironmentCheck {
    let supported = cfg!(target_os = "linux");
    let mut notes = Vec::new();
    let mut video_devices = Vec::new();

    if !supported {
        notes.push("当前 backend 仅支持 Linux。".to_string());
        return EnvironmentCheck {
            os: std::env::consts::OS,
            supported,
            video_devices,
            notes,
        };
    }

    #[cfg(target_os = "linux")]
    {
        match fs::read_dir("/dev") {
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .filter(|name| name.starts_with("video"))
                            .map(|_| entry.path())
                    })
                    .collect();
                paths.sort();
                for path in paths {
                    match fs::symlink_metadata(&path) {
                        Ok(metadata) => {
                            let mode = metadata.mode() & 0o777;
                            video_devices.push(VideoDevicePermission {
                                path: path.display().to_string(),
                                mode: format!("{mode:03o}"),
                                owner_uid: metadata.uid(),
                                owner_gid: metadata.gid(),
                                has_read_bit: mode & 0o444 != 0,
                                has_write_bit: mode & 0o222 != 0,
                            });
                        }
                        Err(err) => {
                            notes.push(format!("无法读取 {} 的元数据：{err}", path.display()))
                        }
                    }
                }
            }
            Err(err) => notes.push(format!("无法枚举 /dev：{err}")),
        }
        if video_devices.is_empty() {
            notes.push("未发现 /dev/video*；请检查 USB 连接和 V4L2 驱动。".to_string());
        }
        notes.push(
            "权限结果仅检查设备节点元数据和读写权限位，不调用 open/ioctl，不验证设备可采集性。"
                .to_string(),
        );
        notes.push(
            "若当前用户仍无法访问，请检查其是否属于设备节点所属组（通常为 video），并重新登录。"
                .to_string(),
        );
    }

    EnvironmentCheck {
        os: std::env::consts::OS,
        supported,
        video_devices,
        notes,
    }
}

fn run_check_environment(json: bool) -> Result<()> {
    let report = inspect_environment();
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
        return Ok(());
    }

    writeln!(
        out,
        "OS: {} ({})",
        report.os,
        if report.supported {
            "supported"
        } else {
            "unsupported"
        }
    )?;
    for device in &report.video_devices {
        writeln!(
            out,
            "{} mode={} uid={} gid={} read_bit={} write_bit={}",
            device.path,
            device.mode,
            device.owner_uid,
            device.owner_gid,
            device.has_read_bit,
            device.has_write_bit
        )?;
    }
    for note in &report.notes {
        writeln!(out, "NOTE: {note}")?;
    }
    Ok(())
}

fn run_list_devices(json: bool) -> Result<()> {
    let list = list_devices::list_rgb_video_devices()?;
    let mut out = std::io::stdout().lock();
    if json {
        let devices: Vec<ListedDevice> = list
            .into_iter()
            .map(|(name, address)| ListedDevice { name, address })
            .collect();
        serde_json::to_writer_pretty(&mut out, &devices)?;
        writeln!(out)?;
    } else {
        for (name, address) in list {
            writeln!(out, "{}\t{}", name, address)?;
        }
    }
    Ok(())
}

/// 与 Python OpenCV snapshot 一致：先丢掉若干帧，减轻自动曝光未稳定 / USB 首帧异常。
const SNAPSHOT_DISCARD_FRAMES: usize = 2;
/// 丢弃预热后仍可能拿到全黑帧时，最多再抓几帧直到画面有亮度。
const SNAPSHOT_MAX_BLACK_SKIPS: u32 = 24;
/// 设备忙、超时等间歇性失败时重试次数（含首次共 N 次）。
const SNAPSHOT_MAX_ATTEMPTS: u32 = 5;
const SNAPSHOT_RETRY_BASE_MS: u64 = 200;

fn should_retry_snapshot(err: &eyre::Report) -> bool {
    let s = err.to_string();
    !s.contains("摄像头已断开连接") && !s.contains("设备可能已被拔出")
}

/// 采样亮度，过滤 UVC 重开后的全黑/空缓冲帧（第二次 snapshot 常见）。
fn snapshot_frame_has_visible_content(frame: &CapturedFrame) -> bool {
    const MIN_MEAN: u64 = 10;
    match frame.pixel_format {
        PixelFormat::Mjpeg => {
            if frame.data.len() < 512 {
                return false;
            }
            let Ok(img) = image_crate::load_from_memory(&frame.data) else {
                return false;
            };
            let rgb = img.to_rgb8();
            let w = rgb.width() as usize;
            let h = rgb.height() as usize;
            if w == 0 || h == 0 {
                return false;
            }
            let step_y = (h / 24).max(1);
            let step_x = (w / 24).max(1);
            let mut sum = 0u64;
            let mut n = 0u64;
            for y in (0..h).step_by(step_y) {
                for x in (0..w).step_by(step_x) {
                    let p = rgb.get_pixel(x as u32, y as u32);
                    sum += p.0[0] as u64 + p.0[1] as u64 + p.0[2] as u64;
                    n += 3;
                }
            }
            n > 0 && sum / n > MIN_MEAN
        }
        PixelFormat::Yuyv => {
            let d = &frame.data;
            if d.len() < 64 {
                return false;
            }
            let mut sum = 0u64;
            let mut n = 0u64;
            for i in (0..d.len()).step_by(8) {
                sum += d[i] as u64;
                n += 1;
            }
            n > 0 && sum / n > MIN_MEAN
        }
        PixelFormat::Rgb8 | PixelFormat::Bgr8 | PixelFormat::Gray8 => {
            if frame.data.is_empty() {
                return false;
            }
            let step = (frame.data.len() / 2000).max(1);
            let mut sum = 0u64;
            let mut n = 0u64;
            for i in (0..frame.data.len()).step_by(step) {
                sum += frame.data[i] as u64;
                n += 1;
            }
            n > 0 && sum / n > MIN_MEAN
        }
    }
}

fn load_snapshot_config(config_path: Option<&str>, device: Option<&str>) -> Result<CameraConfig> {
    match (config_path, device) {
        (Some(path), device_override) => {
            let mut config = CameraConfig::from_yaml_path(path)?;
            if let Some(device) = device_override {
                config.device = device.to_string();
            }
            Ok(config)
        }
        (None, Some(device)) => CameraConfig::for_snapshot(device.to_string()),
        (None, None) => eyre::bail!("snapshot requires --config <path.yaml> or --device <device>"),
    }
}

fn run_snapshot_once(config: &CameraConfig, output_path: &str) -> Result<()> {
    let mut backend = backend::create_backend(config)?;
    for _ in 0..SNAPSHOT_DISCARD_FRAMES {
        backend.capture_fresh_frame()?;
    }
    let mut frame = None;
    for attempt in 0..SNAPSHOT_MAX_BLACK_SKIPS {
        let f = backend.capture_fresh_frame()?;
        if snapshot_frame_has_visible_content(&f) {
            frame = Some(f);
            break;
        }
        if attempt > 0 && attempt % 8 == 0 {
            warn!("snapshot: 跳过疑似全黑帧，继续抓取...");
        }
    }
    let frame = frame.ok_or_else(|| {
        eyre::eyre!(
            "snapshot: 连续 {} 帧画面仍接近全黑（可稍等再试，或检查摄像头曝光/光照）",
            SNAPSHOT_MAX_BLACK_SKIPS
        )
    })?;
    let rgb = frame_to_rgb_array(&frame)?;
    let jpeg = encode_rgb_jpeg(&rgb, config.image_jpeg_quality)?;
    fs::write(output_path, jpeg).wrap_err_with(|| format!("failed to write {}", output_path))?;
    info!(output_path, "snapshot written");
    Ok(())
}

fn run_snapshot(config_path: Option<&str>, device: Option<&str>, output_path: &str) -> Result<()> {
    let config = load_snapshot_config(config_path, device)?;
    for attempt in 0..SNAPSHOT_MAX_ATTEMPTS {
        if attempt > 0 {
            let delay = SNAPSHOT_RETRY_BASE_MS * attempt as u64;
            warn!(
                delay_ms = delay,
                retry_attempt = attempt + 1,
                max_attempts = SNAPSHOT_MAX_ATTEMPTS,
                "snapshot failed, will retry"
            );
            std::thread::sleep(Duration::from_millis(delay));
        }
        match run_snapshot_once(&config, output_path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !should_retry_snapshot(&e) {
                    return Err(e);
                }
                if attempt + 1 >= SNAPSHOT_MAX_ATTEMPTS {
                    return Err(e);
                }
                warn!(error = %e, "snapshot failed, retrying");
            }
        }
    }
    eyre::bail!("snapshot: 重试用尽")
}

fn main() -> Result<()> {
    let cmd = parse_args();

    match &cmd {
        CliCommand::ListDevices { json } => {
            run_list_devices(*json)?;
            return Ok(());
        }
        CliCommand::CheckEnvironment { json } => {
            run_check_environment(*json)?;
            return Ok(());
        }
        CliCommand::Snapshot {
            config_path,
            device,
            output,
        } => {
            init_tracing("usb_camera");
            run_snapshot(config_path.as_deref(), device.as_deref(), output)?;
            return Ok(());
        }
        CliCommand::Run { config_path: _ } => {}
    }

    init_tracing("usb_camera");

    let config_path = match &cmd {
        CliCommand::Run { config_path } => config_path.as_deref(),
        _ => None,
    };
    let config = CameraConfig::load(config_path)?;

    // 初始化 Dora 节点
    let (mut node, mut events): (DoraNode, EventStream) = DoraNode::init_from_env()?;

    // 创建采集后端
    let mut backend = backend::create_backend(&config)?;

    let output_id = DataId::from(config.output_id.clone());

    info!(
        "[usb_camera] started rust camera node on {}, output_id={}, image_format={:?}",
        config.device, config.output_id, config.image_format
    );

    // 事件循环
    loop {
        let event = match events.recv() {
            Some(ev) => ev,
            None => break,
        };

        match event {
            Event::Input {
                id,
                metadata,
                data: _,
            } => {
                if id.as_str() != "tick" {
                    continue;
                }

                match capture_and_build_image(&mut *backend, &config) {
                    Ok((batch, capture_timestamp_ns)) => {
                        let struct_array: StructArray = batch.into();
                        let parameters =
                            output_parameters(metadata.parameters, capture_timestamp_ns);
                        if let Err(e) =
                            node.send_output(output_id.clone(), parameters, struct_array)
                        {
                            warn!(error = %e, "send_output failed");
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if is_usb_camera_disconnected_error(&msg) {
                            error!(error = %e, "{} - exiting", msg);
                            return Err(e);
                        }
                        warn!(error = %e, "capture/build image failed");
                    }
                }
            }
            Event::Stop(_cause) => {
                info!("received stop event, exiting");
                break;
            }
            Event::Error(msg) => {
                error!(error_message = %msg, "error event from Dora");
                return Err(eyre::eyre!("Dora error: {msg}"));
            }
            _ => {}
        }
    }

    Ok(())
}

fn capture_and_build_image(
    backend: &mut dyn CaptureBackend,
    config: &CameraConfig,
) -> Result<(RecordBatch, Option<i64>)> {
    let frame = backend.capture_frame()?;
    let capture_timestamp_ns = frame.capture_timestamp_ns;
    let batch = image_from_captured_frame(frame, config)?;
    Ok((batch, capture_timestamp_ns))
}

fn output_parameters(
    mut trigger_parameters: MetadataParameters,
    capture_timestamp_ns: Option<i64>,
) -> MetadataParameters {
    trigger_parameters.remove(CAPTURE_TIMESTAMP_KEY);
    if let Some(timestamp_ns) = capture_timestamp_ns {
        trigger_parameters.insert(
            CAPTURE_TIMESTAMP_KEY.to_string(),
            Parameter::Integer(timestamp_ns),
        );
    }
    trigger_parameters
}

fn normalize_mjpeg_payload(data: Bytes) -> Result<Bytes> {
    if !data.starts_with(&[0xff, 0xd8]) {
        eyre::bail!(
            "MJPEG 直通校验失败：缺少 JPEG SOI；请确认选择的是 RGB/彩色节点，而非深度、红外或元数据节点"
        );
    }

    if let Some(index) = data.windows(2).rposition(|marker| marker == [0xff, 0xd9]) {
        // 部分 UVC 设备会在 EOI 后附带对齐字节，发送前一并裁掉。
        return Ok(data.slice(..index + 2));
    }

    // 部分 UVC 设备依赖 V4L2 buffer 边界标识一帧结束，不在每帧末尾写 EOI。
    // 为下游 JPEG 解码器补齐标准结束标记；V4L2 标坏的截断帧已在 backend 丢弃。
    let mut payload = Vec::with_capacity(data.len() + 2);
    payload.extend_from_slice(&data);
    payload.extend_from_slice(&[0xff, 0xd9]);
    Ok(Bytes::from(payload))
}

fn image_from_captured_frame(frame: CapturedFrame, config: &CameraConfig) -> Result<RecordBatch> {
    fn map_img_err(e: impl std::fmt::Display) -> eyre::Report {
        eyre::eyre!("{}", e)
    }

    if can_direct_use_mjpeg(frame.pixel_format, config.image_format) {
        // 完整解码会把 30 FPS 直通退化成 CPU 解码链路；这里只规范化 JPEG 边界。
        let payload = normalize_mjpeg_payload(frame.data)?;
        return CompressedImage::new("jpeg", payload)
            .and_then(|img| img.to_record_batch())
            .map_err(map_img_err);
    }

    let rgb = frame_to_rgb_array(&frame)?;

    match config.image_format {
        ImageFormat::Raw => Image::from_rgb8_ndarray(rgb.view())
            .and_then(|img| img.to_record_batch())
            .map_err(map_img_err),
        ImageFormat::Jpeg => {
            let jpeg = encode_rgb_jpeg(&rgb, config.image_jpeg_quality)?;
            CompressedImage::new("jpeg", Bytes::from(jpeg))
                .and_then(|img| img.to_record_batch())
                .map_err(map_img_err)
        }
        ImageFormat::Png => {
            let png = encode_rgb_png(&rgb)?;
            CompressedImage::new("png", Bytes::from(png))
                .and_then(|img| img.to_record_batch())
                .map_err(map_img_err)
        }
    }
}

fn encode_rgb_jpeg(rgb: &Array3<u8>, quality: u8) -> Result<Vec<u8>> {
    let (h, w, _) = rgb.dim();
    let (buf, _offset) = rgb.to_owned().into_raw_vec_and_offset();
    let mut jpeg_bytes = Vec::new();
    let encoder =
        image_crate::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, quality);
    encoder
        .write_image(
            &buf,
            w as u32,
            h as u32,
            image_crate::ExtendedColorType::Rgb8,
        )
        .map_err(|e| eyre::eyre!("failed to encode jpeg: {e}"))?;
    Ok(jpeg_bytes)
}

fn encode_rgb_png(rgb: &Array3<u8>) -> Result<Vec<u8>> {
    let (h, w, _) = rgb.dim();
    let (buf, _offset) = rgb.to_owned().into_raw_vec_and_offset();
    let mut png_bytes = Vec::new();
    let encoder = image_crate::codecs::png::PngEncoder::new_with_quality(
        &mut png_bytes,
        image_crate::codecs::png::CompressionType::Fast,
        image_crate::codecs::png::FilterType::NoFilter,
    );
    encoder
        .write_image(
            &buf,
            w as u32,
            h as u32,
            image_crate::ExtendedColorType::Rgb8,
        )
        .map_err(|e| eyre::eyre!("failed to encode png: {e}"))?;
    Ok(png_bytes)
}

fn frame_to_rgb_array(frame: &CapturedFrame) -> Result<Array3<u8>> {
    match frame.pixel_format {
        PixelFormat::Mjpeg => {
            let dyn_img = image_crate::load_from_memory(&frame.data)
                .wrap_err("failed to decode mjpeg frame")?;
            let rgb = dyn_img.to_rgb8();
            let (w, h) = rgb.dimensions();
            let buf = rgb.into_raw();
            let arr = Array3::from_shape_vec((h as usize, w as usize, 3), buf)
                .wrap_err("failed to build ndarray from rgb buffer")?;
            Ok(arr)
        }
        PixelFormat::Rgb8 => {
            let w = frame.width as usize;
            let h = frame.height as usize;
            let c = 3usize;
            let data = packed_frame_data(frame, c)?;
            let arr = Array3::from_shape_vec((h, w, c), data)
                .context("failed to build ndarray from rgb8 buffer")?;
            Ok(arr)
        }
        PixelFormat::Bgr8 => {
            let w = frame.width as usize;
            let h = frame.height as usize;
            let c = 3usize;
            let mut buf = packed_frame_data(frame, c)?;
            for pix in buf.chunks_exact_mut(3) {
                pix.swap(0, 2);
            }
            let arr = Array3::from_shape_vec((h, w, c), buf)
                .wrap_err("failed to build ndarray from bgr8 buffer")?;
            Ok(arr)
        }
        PixelFormat::Yuyv => yuyv_frame_to_rgb_array(frame),
        PixelFormat::Gray8 => {
            let w = frame.width as usize;
            let h = frame.height as usize;
            let c = 1usize;
            let gray = Array3::from_shape_vec((h, w, c), packed_frame_data(frame, c)?)
                .wrap_err("failed to build ndarray from gray8 buffer")?;
            // 扩展成 RGB
            let mut rgb = Array3::<u8>::zeros((h, w, 3));
            for y in 0..h {
                for x in 0..w {
                    let g = gray[(y, x, 0)];
                    rgb[(y, x, 0)] = g;
                    rgb[(y, x, 1)] = g;
                    rgb[(y, x, 2)] = g;
                }
            }
            Ok(rgb)
        }
    }
}

fn packed_frame_data(frame: &CapturedFrame, bytes_per_pixel: usize) -> Result<Vec<u8>> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let packed_stride = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| eyre::eyre!("frame row size overflow"))?;
    let stride = if frame.stride == 0 {
        packed_stride
    } else {
        frame.stride
    };
    if stride < packed_stride {
        eyre::bail!(
            "frame stride {} is smaller than packed row size {}",
            stride,
            packed_stride
        );
    }
    let required = stride
        .checked_mul(height)
        .ok_or_else(|| eyre::eyre!("frame buffer size overflow"))?;
    if frame.data.len() < required {
        eyre::bail!(
            "unexpected frame buffer size: {} < {} ({}x{}, stride={})",
            frame.data.len(),
            required,
            width,
            height,
            stride
        );
    }

    let packed_len = packed_stride
        .checked_mul(height)
        .ok_or_else(|| eyre::eyre!("packed frame size overflow"))?;
    let mut packed = Vec::with_capacity(packed_len);
    for row in frame.data[..required].chunks_exact(stride) {
        packed.extend_from_slice(&row[..packed_stride]);
    }
    Ok(packed)
}

/// YUYV/YUY2（4:2:2）转 RGB888，遵循 V4L2 协商出的量化范围和色彩矩阵。
fn yuyv422_to_rgb(y: i32, u: i32, v: i32, colorimetry: YuvColorimetry) -> (u8, u8, u8) {
    let d = u - 128;
    let e = v - 128;
    let (r, g, b) = match (colorimetry.range, colorimetry.matrix) {
        (YuvRange::Limited, YuvMatrix::Bt601) => {
            let c = (y - 16).max(0);
            (
                (298 * c + 409 * e + 128) >> 8,
                (298 * c - 100 * d - 208 * e + 128) >> 8,
                (298 * c + 516 * d + 128) >> 8,
            )
        }
        (YuvRange::Limited, YuvMatrix::Bt709) => {
            let c = (y - 16).max(0);
            (
                (298 * c + 459 * e + 128) >> 8,
                (298 * c - 55 * d - 136 * e + 128) >> 8,
                (298 * c + 541 * d + 128) >> 8,
            )
        }
        (YuvRange::Full, YuvMatrix::Bt601) => (
            y + ((359 * e) >> 8),
            y - ((88 * d + 183 * e) >> 8),
            y + ((454 * d) >> 8),
        ),
        (YuvRange::Full, YuvMatrix::Bt709) => (
            y + ((403 * e) >> 8),
            y - ((48 * d + 120 * e) >> 8),
            y + ((475 * d) >> 8),
        ),
    };
    (
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    )
}

fn yuyv_frame_to_rgb_array(frame: &CapturedFrame) -> Result<Array3<u8>> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w & 1 != 0 {
        eyre::bail!("YUYV width must be even, got {}", w);
    }
    let yuyv = packed_frame_data(frame, 2)?;
    let colorimetry = frame.yuv_colorimetry.unwrap_or(YuvColorimetry {
        range: YuvRange::Limited,
        matrix: YuvMatrix::Bt601,
    });
    let rgb_len = w
        .checked_mul(h)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| eyre::eyre!("RGB frame size overflow"))?;
    let mut rgb = Vec::with_capacity(rgb_len);
    for pair in yuyv.chunks_exact(4) {
        let y0 = pair[0] as i32;
        let u = pair[1] as i32;
        let y1 = pair[2] as i32;
        let v = pair[3] as i32;
        let (r0, g0, b0) = yuyv422_to_rgb(y0, u, v, colorimetry);
        let (r1, g1, b1) = yuyv422_to_rgb(y1, u, v, colorimetry);
        rgb.extend_from_slice(&[r0, g0, b0, r1, g1, b1]);
    }
    Array3::from_shape_vec((h, w, 3), rgb).wrap_err("failed to build ndarray from yuyv rgb buffer")
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use clap::{Parser, error::ErrorKind};
    use dora_node_api::{MetadataParameters, Parameter};

    use super::{CAPTURE_TIMESTAMP_KEY, Cli, normalize_mjpeg_payload, output_parameters};

    #[test]
    fn version_flag_reports_package_version() {
        let error = Cli::try_parse_from(["usb_camera", "--version"])
            .expect_err("--version should exit after displaying the version");

        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(error.to_string(), "usb_camera 1.0.2\n");
        assert_eq!(env!("CARGO_PKG_VERSION"), "1.0.2");
    }

    #[test]
    fn normalize_mjpeg_payload_trims_trailing_uvc_padding() {
        let payload =
            normalize_mjpeg_payload(Bytes::from_static(&[0xff, 0xd8, 1, 2, 0xff, 0xd9, 0, 0]))
                .unwrap();
        assert_eq!(payload.as_ref(), &[0xff, 0xd8, 1, 2, 0xff, 0xd9]);
    }

    #[test]
    fn normalize_mjpeg_payload_appends_missing_eoi() {
        let payload = normalize_mjpeg_payload(Bytes::from_static(&[0xff, 0xd8, 1, 2])).unwrap();
        assert_eq!(payload.as_ref(), &[0xff, 0xd8, 1, 2, 0xff, 0xd9]);
    }

    #[test]
    fn normalize_mjpeg_payload_rejects_missing_soi() {
        let error = normalize_mjpeg_payload(Bytes::from_static(&[1, 2, 0xff, 0xd9]))
            .expect_err("payload without SOI must be rejected");
        assert!(error.to_string().contains("缺少 JPEG SOI"));
    }

    #[test]
    fn output_parameters_preserve_trigger_values_and_use_frame_timestamp() {
        let mut trigger = MetadataParameters::default();
        trigger.insert(
            "request_id".to_string(),
            Parameter::String("request-1".to_string()),
        );
        trigger.insert(CAPTURE_TIMESTAMP_KEY.to_string(), Parameter::Integer(1));

        let output = output_parameters(trigger, Some(1_234_567_890));

        assert_eq!(
            output.get("request_id"),
            Some(&Parameter::String("request-1".to_string()))
        );
        assert_eq!(
            output.get(CAPTURE_TIMESTAMP_KEY),
            Some(&Parameter::Integer(1_234_567_890))
        );
    }

    #[test]
    fn output_parameters_remove_untrusted_trigger_timestamp_when_frame_has_none() {
        let mut trigger = MetadataParameters::default();
        trigger.insert(CAPTURE_TIMESTAMP_KEY.to_string(), Parameter::Integer(1));

        let output = output_parameters(trigger, None);

        assert!(!output.contains_key(CAPTURE_TIMESTAMP_KEY));
    }

    #[test]
    fn yuyv_conversion_produces_two_rgb_pixels_per_pair() {
        let frame = crate::backend::CapturedFrame {
            width: 2,
            height: 1,
            stride: 4,
            pixel_format: crate::backend::PixelFormat::Yuyv,
            yuv_colorimetry: Some(crate::backend::YuvColorimetry {
                range: crate::backend::YuvRange::Limited,
                matrix: crate::backend::YuvMatrix::Bt601,
            }),
            capture_timestamp_ns: None,
            data: bytes::Bytes::from_static(&[16, 128, 235, 128]),
        };
        let rgb = super::yuyv_frame_to_rgb_array(&frame).unwrap();
        assert_eq!(rgb.shape(), &[1, 2, 3]);
        assert_eq!(rgb[(0, 0, 0)], 0);
        assert_eq!(rgb[(0, 1, 0)], 255);
    }

    #[test]
    fn yuyv_conversion_ignores_row_padding() {
        let frame = crate::backend::CapturedFrame {
            width: 2,
            height: 2,
            stride: 6,
            pixel_format: crate::backend::PixelFormat::Yuyv,
            yuv_colorimetry: Some(crate::backend::YuvColorimetry {
                range: crate::backend::YuvRange::Full,
                matrix: crate::backend::YuvMatrix::Bt601,
            }),
            capture_timestamp_ns: None,
            data: bytes::Bytes::from_static(&[
                10, 128, 20, 128, 255, 255, 30, 128, 40, 128, 255, 255,
            ]),
        };
        let rgb = super::yuyv_frame_to_rgb_array(&frame).unwrap();
        assert_eq!(rgb[(0, 0, 0)], 10);
        assert_eq!(rgb[(0, 1, 0)], 20);
        assert_eq!(rgb[(1, 0, 0)], 30);
        assert_eq!(rgb[(1, 1, 0)], 40);
    }
}
