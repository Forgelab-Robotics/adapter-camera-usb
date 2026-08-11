use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use eyre::{Context, Result, eyre};
use rand::{RngExt, rng};
use std::io::ErrorKind;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use v4l::FourCC;
use v4l::buffer::{Flags as BufferFlags, Metadata as BufferMetadata, Type};
use v4l::device::Device;
use v4l::format::{Colorspace, Format, Quantization};
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::{CaptureStream, Stream as V4lStream};
use v4l::video::Capture;

use crate::backend::{CapturedFrame, PixelFormat, YuvColorimetry, YuvMatrix, YuvRange};
use crate::config::{CameraConfig, ImageFormat};

/// 初始化超时：若在此时间内后台线程未完成打开设备/创建流，则报错退出。
const INIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Drop 最多同步等待采集线程退出的时间；异常驱动不得无限阻塞主线程。
const CAPTURE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// mmap 缓冲区数量
const MMAP_BUFFERS: u32 = 4;
/// 每次等待 V4L2 帧的最长时间，避免 stop 时卡在 poll/DQBUF。
const CAPTURE_POLL_TIMEOUT: Duration = Duration::from_secs(2);
/// tick 路径等待首帧的最长时间，避免阻塞 Dora Stop 事件处理。
const FRAME_RECV_TIMEOUT: Duration = Duration::from_millis(500);
/// snapshot 必须等待真实新帧，保留 Gemini2 慢启动所需的较长超时。
const FRESH_FRAME_RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// 创建 mmap 流遇到 EBUSY 时的重试次数。Gemini 2 等 UVC 设备释放较慢，二次打开常需要等待。
const EBUSY_RETRY_ATTEMPTS: u32 = 12;
const EBUSY_RETRY_BASE_MS: u64 = 150;
/// 在 EBUSY 重试基础睡眠时间上的最大随机抖动（毫秒），用于避免多个进程同时抢占 USB。
const EBUSY_JITTER_MAX_MS: u64 = 500;
/// 同类采集健康告警的最小输出间隔；持续异常只输出累计摘要，避免日志刷屏。
const HEALTH_WARNING_INTERVAL: Duration = Duration::from_secs(5);
/// 单次可置信的最大 sequence 跳帧数；更大的跳变视为驱动重置序号。
const MAX_PLAUSIBLE_SEQUENCE_GAP: u32 = 1_000_000;

fn capture_timestamp_ns() -> Option<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_nanos()).ok()
}

/// 采集线程发给主线程的消息：正常帧或设备已断开。
#[derive(Debug)]
enum CaptureMessage {
    Frame(CapturedFrame),
    /// 摄像头已断开（如被拔出），附带原因描述
    Disconnected(String),
}

#[derive(Debug, Clone, Copy)]
struct NegotiatedFormat {
    width: u32,
    height: u32,
    stride: usize,
    pixel_format: PixelFormat,
    yuv_colorimetry: Option<YuvColorimetry>,
    stall_warning_threshold: Duration,
}

pub struct V4lBackend {
    receiver: Receiver<CaptureMessage>,
    stop_tx: Sender<()>,
    done_rx: Receiver<()>,
    capture_thread: Option<JoinHandle<()>>,
    /// 缓存最新的一帧，用于在获取过快时返回
    last_frame: Option<CapturedFrame>,
}

impl V4lBackend {
    pub fn new(config: CameraConfig) -> Result<Self> {
        let (tx, rx) = bounded::<CaptureMessage>(1);
        let rx_cleaner = rx.clone();
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let (init_tx, init_rx) = bounded::<Result<(), String>>(1);
        let (done_tx, done_rx) = bounded::<()>(1);

        let capture_thread = thread::spawn(move || {
            let _ = Self::capture_loop(config, tx, rx_cleaner, stop_rx, init_tx);
            let _ = done_tx.try_send(());
        });

        match init_rx.recv_timeout(INIT_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                let _ = stop_tx.try_send(());
                drop(rx);
                capture_thread
                    .join()
                    .map_err(|_| eyre!("回收摄像头后台线程失败"))?;
                return Err(eyre!("摄像头初始化失败: {msg}"));
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // 保持 INIT_TIMEOUT 的真实上界：不在调用线程同步等待可能卡在
                // ioctl/mmap 的初始化线程。线程发送初始化结果时会发现接收端已
                // 关闭，并在进入采集循环前退出；reaper 仅负责回收 JoinHandle。
                let _ = stop_tx.try_send(());
                drop(rx);
                thread::spawn(move || {
                    let _ = capture_thread.join();
                });
                return Err(eyre!(
                    "摄像头初始化超时 ({}s)，请检查设备是否被其他程序占用或设备是否存在",
                    INIT_TIMEOUT.as_secs()
                ));
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                capture_thread
                    .join()
                    .map_err(|_| eyre!("回收摄像头后台线程失败"))?;
                return Err(eyre!("摄像头后台线程异常退出"));
            }
        }

        Ok(Self {
            receiver: rx,
            stop_tx,
            done_rx,
            capture_thread: Some(capture_thread),
            last_frame: None,
        })
    }

    fn capture_loop(
        config: CameraConfig,
        tx: crossbeam_channel::Sender<CaptureMessage>,
        rx_cleaner: Receiver<CaptureMessage>,
        stop_rx: Receiver<()>,
        init_tx: crossbeam_channel::Sender<Result<(), String>>,
    ) -> Result<()> {
        // 随机 sleep 一小段时间，避免多个进程在同一时刻同时去抢占 USB 设备。
        if Self::wait_with_ebusy_jitter(Duration::ZERO, &stop_rx) {
            return Ok(());
        }

        let send_init_err = |e: &eyre::Report| {
            let _ = init_tx.send(Err(e.to_string()));
        };

        let dev = if let Ok(idx) = config.device.parse::<usize>() {
            Device::new(idx)
                .with_context(|| format!("failed to open v4l device index {idx}"))
                .inspect_err(|e| {
                    send_init_err(e);
                })?
        } else {
            Device::with_path(&config.device)
                .with_context(|| format!("failed to open v4l device path {}", config.device))
                .inspect_err(|e| {
                    send_init_err(e);
                })?
        };

        let mut fmt: Format = dev
            .format()
            .context("failed to query v4l format")
            .inspect_err(|e| {
                send_init_err(e);
            })?;
        if let Some(w) = config.width {
            fmt.width = w;
        }
        if let Some(h) = config.height {
            fmt.height = h;
        }
        // JPEG 走设备 MJPEG 直通；raw/PNG 直接请求 YUYV，避免先解码 JPEG。
        fmt.fourcc = match config.image_format {
            ImageFormat::Jpeg => FourCC::new(b"MJPG"),
            ImageFormat::Raw | ImageFormat::Png => FourCC::new(b"YUYV"),
        };
        let fmt = dev
            .set_format(&fmt)
            .context("failed to set v4l capture format")
            .inspect_err(|e| {
                send_init_err(e);
            })?;

        if let Some(fps) = config.fps
            && let Ok(mut params) = dev.params()
        {
            params.interval = v4l::Fraction::new(1, fps.round() as u32);
            if let Err(e) = dev.set_params(&params) {
                tracing::warn!(fps, error = %e, "无法设置 V4L2 摄像头帧率");
            }
        }

        let negotiated_fps = dev.params().ok().and_then(|params| {
            (params.interval.numerator > 0 && params.interval.denominator > 0)
                .then(|| params.interval.denominator as f64 / params.interval.numerator as f64)
        });
        if let (Some(requested_fps), Some(negotiated_fps)) = (config.fps, negotiated_fps)
            && (negotiated_fps - f64::from(requested_fps)).abs() > f64::from(requested_fps) * 0.05
        {
            tracing::warn!(
                requested_fps,
                negotiated_fps,
                "V4L2 实际帧率与请求值偏差超过 5%，高帧率可能增加 USB/CPU 压力"
            );
        }

        let width = fmt.width;
        let height = fmt.height;
        let pixel_format = match Self::pixel_format_from_fourcc(fmt.fourcc.repr) {
            Ok(format) => format,
            Err(error) => {
                send_init_err(&error);
                return Err(error);
            }
        };

        let mut stream = Self::new_mmap_stream_with_retry(&dev, &stop_rx).inspect_err(|e| {
            send_init_err(e);
        })?;
        stream.set_timeout(CAPTURE_POLL_TIMEOUT);

        // 初始化等待方已经超时/退出时，不再进入采集循环占用设备。
        if init_tx.send(Ok(())).is_err() {
            return Ok(());
        }

        let stride = fmt.stride as usize;
        let yuv_colorimetry =
            (matches!(pixel_format, PixelFormat::Yuyv)).then(|| Self::yuv_colorimetry(&fmt));
        tracing::info!(
            width,
            height,
            fourcc = %fmt.fourcc,
            stride,
            colorspace = %fmt.colorspace,
            quantization = %fmt.quantization,
            requested_fps = ?config.fps,
            negotiated_fps = ?negotiated_fps,
            "V4L2 negotiated capture format"
        );

        let health_check_fps = negotiated_fps.or(config.fps.map(f64::from));
        let stall_warning_threshold = health_check_fps
            .map(|fps| Duration::from_secs_f64((5.0 / fps).max(0.5)))
            .unwrap_or(CAPTURE_POLL_TIMEOUT);
        let negotiated = NegotiatedFormat {
            width,
            height,
            stride,
            pixel_format,
            yuv_colorimetry,
            stall_warning_threshold,
        };
        Self::run_capture_loop(negotiated, tx, rx_cleaner, stop_rx, stream);
        Ok(())
    }

    fn yuv_colorimetry(fmt: &Format) -> YuvColorimetry {
        let range = match fmt.quantization {
            Quantization::FullRange => YuvRange::Full,
            Quantization::LimitedRange => YuvRange::Limited,
            Quantization::Default if matches!(fmt.colorspace, Colorspace::JPEG) => YuvRange::Full,
            Quantization::Default => YuvRange::Limited,
        };
        let matrix = if matches!(fmt.colorspace, Colorspace::Rec709) {
            YuvMatrix::Bt709
        } else {
            YuvMatrix::Bt601
        };
        YuvColorimetry { range, matrix }
    }

    fn pixel_format_from_fourcc(repr: [u8; 4]) -> Result<PixelFormat> {
        let format = match repr {
            [b'M', b'J', b'P', b'G'] => PixelFormat::Mjpeg,
            [b'R', b'G', b'B', b'3'] => PixelFormat::Rgb8,
            [b'B', b'G', b'R', b'3'] => PixelFormat::Bgr8,
            // YUYV / YUY2：每像素 2 字节，不可当作 Gray8
            [b'Y', b'U', b'Y', b'V'] | [b'Y', b'U', b'Y', b'2'] => PixelFormat::Yuyv,
            [b'G', b'R', b'E', b'Y'] | [b'Y', b'8', b'0', b'0'] => PixelFormat::Gray8,
            repr => {
                let name = String::from_utf8_lossy(&repr);
                return Err(eyre!("设备协商出不支持的 V4L2 FourCC: {name}"));
            }
        };
        Ok(format)
    }

    /// 判断错误是否像「设备已断开」（被拔出、USB 断开等），应停止采集并通知主线程。
    fn is_device_disconnected_error(e: &impl std::fmt::Display) -> bool {
        let msg = e.to_string();
        let msg_lower = msg.to_lowercase();
        msg_lower.contains("no such device")
            || msg_lower.contains("no such file")
            || msg_lower.contains("disconnected")
            || msg_lower.contains("input/output error")
            || msg_lower.contains("i/o error")
            || msg.contains("os error 5")   // EIO
            || msg.contains("os error 19")   // ENODEV
            || msg.contains("error 5")
            || msg.contains("error 19")
    }

    /// 创建 mmap 流。部分 UVC 设备在 STREAMOFF 后仍需要一点时间释放，因此对 EBUSY 做短退避重试。
    fn new_mmap_stream_with_retry<'a>(
        dev: &'a Device,
        stop_rx: &Receiver<()>,
    ) -> Result<MmapStream<'a>> {
        let mut last_busy = None;
        for attempt in 0..EBUSY_RETRY_ATTEMPTS {
            match MmapStream::with_buffers(dev, Type::VideoCapture, MMAP_BUFFERS) {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    let msg = e.to_string();
                    let is_ebusy = msg.contains("resource busy")
                        || msg.contains("os error 16")
                        || msg.contains("error 16");
                    if !is_ebusy {
                        return Err(e).context("failed to create v4l mmap stream");
                    }

                    last_busy = Some(e);
                    if attempt + 1 < EBUSY_RETRY_ATTEMPTS {
                        let backoff =
                            Duration::from_millis(EBUSY_RETRY_BASE_MS * (attempt + 1) as u64);
                        if Self::wait_with_ebusy_jitter(backoff, stop_rx) {
                            return Err(eyre!("摄像头初始化已取消"));
                        }
                    }
                }
            }
        }

        Err(last_busy.expect("EBUSY retry loop should record an error")).context(
            "摄像头设备正忙 (Device or resource busy)。请确认没有浏览器、VLC、其他相机节点或未退出的 usb_camera 进程正在占用该设备",
        )
    }

    /// 在基础 EBUSY 重试等待时间上加入一个 0..=EBUSY_JITTER_MAX_MS 的随机抖动，
    /// 用于尽量避免多个进程在完全相同的时间点同时重试抢占 USB 设备。
    /// 返回 true 表示等待期间收到停止信号。
    fn wait_with_ebusy_jitter(base: Duration, stop_rx: &Receiver<()>) -> bool {
        let mut rng = rng();
        let jitter_ms: u64 = rng.random_range(0..=EBUSY_JITTER_MAX_MS);
        let total = base + Duration::from_millis(jitter_ms);
        !matches!(
            stop_rx.recv_timeout(total),
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        )
    }

    fn buffer_is_corrupted(meta: &BufferMetadata) -> bool {
        meta.flags.contains(BufferFlags::ERROR)
    }

    fn sequence_gap(previous: u32, current: u32) -> u32 {
        let advance = current.wrapping_sub(previous);
        if advance > 1 && advance <= MAX_PLAUSIBLE_SEQUENCE_GAP + 1 {
            advance - 1
        } else {
            0
        }
    }

    fn should_log_health_warning(last_warning_at: &mut Option<Instant>, now: Instant) -> bool {
        if last_warning_at.is_some_and(|last| now.duration_since(last) < HEALTH_WARNING_INTERVAL) {
            return false;
        }
        *last_warning_at = Some(now);
        true
    }

    fn run_capture_loop(
        format: NegotiatedFormat,
        tx: crossbeam_channel::Sender<CaptureMessage>,
        rx_cleaner: Receiver<CaptureMessage>,
        stop_rx: Receiver<()>,
        mut stream: MmapStream<'_>,
    ) {
        let disconnected_msg = "摄像头已断开连接 (设备可能已被拔出或 USB 断开)".to_string();
        let mut corrupted_frames = 0u64;
        let mut last_sequence = None;
        let mut sequence_gap_events = 0u64;
        let mut sequence_gap_frames = 0u64;
        let mut last_sequence_warning_at = None;
        let mut capture_timeouts = 0u64;
        let mut consecutive_capture_timeouts = 0u64;
        let mut last_timeout_warning_at = None;
        let mut last_frame_received_at = None;
        let mut capture_stalls = 0u64;
        let mut last_stall_warning_at = None;
        loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let (buf, meta) = match stream.next() {
                Ok(res) => res,
                Err(e) => {
                    if e.kind() == ErrorKind::TimedOut {
                        if !matches!(stop_rx.try_recv(), Err(TryRecvError::Empty)) {
                            break;
                        }
                        capture_timeouts = capture_timeouts.saturating_add(1);
                        consecutive_capture_timeouts =
                            consecutive_capture_timeouts.saturating_add(1);
                        if consecutive_capture_timeouts == 1
                            || Self::should_log_health_warning(
                                &mut last_timeout_warning_at,
                                Instant::now(),
                            )
                        {
                            last_timeout_warning_at = Some(Instant::now());
                            eprintln!(
                                "V4L2 capture warning: frame timeout timeout_ms={} consecutive_timeouts={} total_timeouts={}",
                                CAPTURE_POLL_TIMEOUT.as_millis(),
                                consecutive_capture_timeouts,
                                capture_timeouts
                            );
                        }
                        // v4l::MmapStream queues before dequeueing. If dequeue times out,
                        // reset streaming before the next next() call to avoid re-queueing
                        // an already queued buffer on devices that stop producing frames.
                        last_frame_received_at = None;
                        last_sequence = None;
                        let _ = stream.stop();
                        continue;
                    }
                    if Self::is_device_disconnected_error(&e) {
                        eprintln!("V4L2 device disconnected: {}", e);
                        Self::replace_pending_message(
                            &tx,
                            &rx_cleaner,
                            CaptureMessage::Disconnected(disconnected_msg.clone()),
                        );
                        break;
                    }
                    eprintln!("V4L2 capture warning: {}", e);
                    continue;
                }
            };

            consecutive_capture_timeouts = 0;
            let received_at = Instant::now();
            if let Some(previous_received_at) = last_frame_received_at {
                let frame_interval = received_at.duration_since(previous_received_at);
                if frame_interval > format.stall_warning_threshold {
                    capture_stalls = capture_stalls.saturating_add(1);
                    if Self::should_log_health_warning(&mut last_stall_warning_at, received_at) {
                        eprintln!(
                            "V4L2 capture warning: long frame interval sequence={} interval_ms={} threshold_ms={} total_stalls={}",
                            meta.sequence,
                            frame_interval.as_millis(),
                            format.stall_warning_threshold.as_millis(),
                            capture_stalls
                        );
                    }
                }
            }
            last_frame_received_at = Some(received_at);

            if let Some(previous_sequence) = last_sequence {
                let missing = Self::sequence_gap(previous_sequence, meta.sequence);
                if missing > 0 {
                    sequence_gap_events = sequence_gap_events.saturating_add(1);
                    sequence_gap_frames = sequence_gap_frames.saturating_add(u64::from(missing));
                    if Self::should_log_health_warning(&mut last_sequence_warning_at, received_at) {
                        eprintln!(
                            "V4L2 capture warning: sequence gap previous_sequence={} sequence={} missing_frames={} total_gap_events={} total_missing_frames={}",
                            previous_sequence,
                            meta.sequence,
                            missing,
                            sequence_gap_events,
                            sequence_gap_frames
                        );
                    }
                }
            }
            last_sequence = Some(meta.sequence);

            if Self::buffer_is_corrupted(meta) {
                corrupted_frames = corrupted_frames.saturating_add(1);
                // UVC 在 USB 等时传输丢包时通常仍会返回 buffer，但通过
                // V4L2_BUF_FLAG_ERROR 标记内容已损坏；尾部色块是常见表现。
                // 丢弃后主线程会继续使用上一张完整帧。
                if corrupted_frames == 1 || corrupted_frames.is_multiple_of(100) {
                    eprintln!(
                        "V4L2 capture warning: dropping corrupted frame sequence={} bytesused={} total_dropped={}",
                        meta.sequence, meta.bytesused, corrupted_frames
                    );
                }
                continue;
            }

            let capture_timestamp_ns = capture_timestamp_ns();
            let bytes_used = meta.bytesused as usize;
            if bytes_used == 0 || bytes_used > buf.len() {
                eprintln!(
                    "V4L2 capture warning: invalid bytesused={} for mmap buffer size={}",
                    bytes_used,
                    buf.len()
                );
                continue;
            }

            let frame = CapturedFrame {
                width: format.width,
                height: format.height,
                stride: format.stride,
                pixel_format: format.pixel_format,
                yuv_colorimetry: format.yuv_colorimetry,
                capture_timestamp_ns,
                data: Bytes::copy_from_slice(&buf[..bytes_used]),
            };

            if !Self::replace_pending_message(&tx, &rx_cleaner, CaptureMessage::Frame(frame)) {
                break;
            }
        }
    }

    /// 用新状态替换容量为 1 的旧消息，且永不阻塞采集线程。
    fn replace_pending_message(
        tx: &Sender<CaptureMessage>,
        rx_cleaner: &Receiver<CaptureMessage>,
        mut message: CaptureMessage,
    ) -> bool {
        loop {
            match tx.try_send(message) {
                Ok(()) => return true,
                Err(TrySendError::Full(returned)) => {
                    message = returned;
                    let _ = rx_cleaner.try_recv();
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }
}

impl Drop for V4lBackend {
    fn drop(&mut self) {
        let _ = self.stop_tx.try_send(());
        if let Some(handle) = self.capture_thread.take() {
            match self.done_rx.recv_timeout(CAPTURE_SHUTDOWN_TIMEOUT) {
                Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    let _ = handle.join();
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        timeout_secs = CAPTURE_SHUTDOWN_TIMEOUT.as_secs(),
                        "V4L2 采集线程未及时退出，转为后台回收"
                    );
                    thread::spawn(move || {
                        let _ = handle.join();
                    });
                }
            }
        }
    }
}

impl crate::backend::CaptureBackend for V4lBackend {
    fn capture_frame(&mut self) -> Result<CapturedFrame> {
        // 1. 非阻塞地取完通道里所有新消息
        let mut newest_frame = None;
        loop {
            match self.receiver.try_recv() {
                Ok(CaptureMessage::Frame(f)) => newest_frame = Some(f),
                Ok(CaptureMessage::Disconnected(reason)) => return Err(eyre::eyre!("{}", reason)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(eyre::eyre!("摄像头采集线程已退出，请检查设备是否已断开"));
                }
            }
        }

        if let Some(frame) = newest_frame {
            self.last_frame = Some(frame.clone());
            return Ok(frame);
        }

        // 2. 没有新帧时返回缓存的上一帧。该行为与旧实现一致，可避免 tick
        // 快于 Gemini2 实际出帧速度时在主线程额外等待。
        if let Some(cached_frame) = &self.last_frame {
            return Ok(cached_frame.clone());
        }

        // 3. 等待第一帧或断开消息。
        match self.receiver.recv_timeout(FRAME_RECV_TIMEOUT) {
            Ok(CaptureMessage::Frame(f)) => {
                self.last_frame = Some(f.clone());
                Ok(f)
            }
            Ok(CaptureMessage::Disconnected(reason)) => Err(eyre::eyre!("{}", reason)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(eyre::eyre!(
                "等待摄像头帧超时 ({}ms)，请检查设备是否输出视频或是否被占用",
                FRAME_RECV_TIMEOUT.as_millis()
            )),
            Err(_) => Err(eyre::eyre!("摄像头采集线程已退出，请检查设备是否已断开")),
        }
    }

    fn capture_fresh_frame(&mut self) -> Result<CapturedFrame> {
        match self.receiver.recv_timeout(FRESH_FRAME_RECV_TIMEOUT) {
            Ok(CaptureMessage::Frame(f)) => {
                self.last_frame = Some(f.clone());
                Ok(f)
            }
            Ok(CaptureMessage::Disconnected(reason)) => Err(eyre::eyre!("{}", reason)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(eyre::eyre!(
                "等待摄像头新帧超时 ({}s)，请检查设备是否输出视频或是否被占用",
                FRESH_FRAME_RECV_TIMEOUT.as_secs()
            )),
            Err(_) => Err(eyre::eyre!("摄像头采集线程已退出，请检查设备是否已断开")),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::CaptureBackend as _;

    use super::*;

    fn test_frame(value: u8) -> CapturedFrame {
        CapturedFrame {
            width: 1,
            height: 1,
            stride: 1,
            pixel_format: PixelFormat::Gray8,
            yuv_colorimetry: None,
            capture_timestamp_ns: Some(i64::from(value)),
            data: Bytes::from(vec![value]),
        }
    }

    #[test]
    fn cached_frame_keeps_capture_timestamp_until_replaced() {
        let (frame_tx, receiver) = bounded(1);
        let (stop_tx, _stop_rx) = bounded(1);
        let (_done_tx, done_rx) = bounded(1);
        let mut backend = V4lBackend {
            receiver,
            stop_tx,
            done_rx,
            capture_thread: None,
            last_frame: Some(test_frame(7)),
        };

        let cached = backend.capture_frame().unwrap();
        frame_tx
            .try_send(CaptureMessage::Frame(test_frame(9)))
            .unwrap();
        let fresh = backend.capture_frame().unwrap();
        let cached_fresh = backend.capture_frame().unwrap();

        assert_eq!(cached.capture_timestamp_ns, Some(7));
        assert_eq!(fresh.capture_timestamp_ns, Some(9));
        assert_eq!(cached_fresh.capture_timestamp_ns, Some(9));
    }

    #[test]
    fn rejects_unknown_fourcc() {
        assert!(matches!(
            V4lBackend::pixel_format_from_fourcc(*b"MJPG").unwrap(),
            PixelFormat::Mjpeg
        ));
        assert!(V4lBackend::pixel_format_from_fourcc(*b"NV12").is_err());
    }

    #[test]
    fn detects_v4l2_corrupted_buffer_flag() {
        let mut meta = BufferMetadata::default();
        assert!(!V4lBackend::buffer_is_corrupted(&meta));

        meta.flags = BufferFlags::DONE | BufferFlags::ERROR;
        assert!(V4lBackend::buffer_is_corrupted(&meta));
    }

    #[test]
    fn detects_sequence_gaps_without_misreading_wrap_or_reset() {
        assert_eq!(V4lBackend::sequence_gap(10, 11), 0);
        assert_eq!(V4lBackend::sequence_gap(10, 14), 3);
        assert_eq!(V4lBackend::sequence_gap(u32::MAX, 0), 0);
        assert_eq!(V4lBackend::sequence_gap(100, 3), 0);
        assert_eq!(V4lBackend::sequence_gap(u32::MAX / 2 + 100, 0), 0);
    }

    #[test]
    fn health_warnings_are_limited_by_elapsed_time() {
        let start = Instant::now();
        let mut last_warning_at = None;
        assert!(V4lBackend::should_log_health_warning(
            &mut last_warning_at,
            start
        ));
        assert!(!V4lBackend::should_log_health_warning(
            &mut last_warning_at,
            start + Duration::from_secs(1)
        ));
        assert!(V4lBackend::should_log_health_warning(
            &mut last_warning_at,
            start + HEALTH_WARNING_INTERVAL
        ));
    }

    #[test]
    fn disconnected_message_replaces_pending_frame_without_blocking() {
        let (tx, rx) = bounded(1);
        tx.try_send(CaptureMessage::Frame(test_frame(1))).unwrap();
        assert!(V4lBackend::replace_pending_message(
            &tx,
            &rx,
            CaptureMessage::Disconnected("gone".to_string())
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            CaptureMessage::Disconnected(reason) if reason == "gone"
        ));
    }
}
