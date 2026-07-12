use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use eyre::{Context, Result, eyre};
use rand::{RngExt, rng};
use std::io::ErrorKind;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use v4l::FourCC;
use v4l::buffer::Type;
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
/// tick 路径等待下一帧的最长时间，避免阻塞 Dora Stop 事件处理。
const FRAME_RECV_TIMEOUT: Duration = Duration::from_millis(500);
/// snapshot 必须等待真实新帧，允许更长超时。
const FRESH_FRAME_RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// 超过此时间没有收到新帧时，禁止继续返回缓存帧，避免断流后伪装成正常画面。
const STALE_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// 创建 mmap 流遇到 EBUSY 时的重试次数。Gemini 2 等 UVC 设备释放较慢，二次打开常需要等待。
const EBUSY_RETRY_ATTEMPTS: u32 = 12;
const EBUSY_RETRY_BASE_MS: u64 = 150;
/// 在 EBUSY 重试基础睡眠时间上的最大随机抖动（毫秒），用于避免多个进程同时抢占 USB。
const EBUSY_JITTER_MAX_MS: u64 = 500;

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
}

pub struct V4lBackend {
    receiver: Receiver<CaptureMessage>,
    stop_tx: Sender<()>,
    done_rx: Receiver<()>,
    capture_thread: Option<JoinHandle<()>>,
    /// 缓存最新的一帧，用于在获取过快时返回
    last_frame: Option<CapturedFrame>,
    last_frame_at: Option<Instant>,
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
            last_frame_at: None,
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
        // JPEG 走设备 MJPEG 直通；raw/PNG 优先请求 YUYV，避免先做一次 JPEG
        // 解码再转换/编码。设备不支持时，驱动可能协商为其他已支持格式。
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
            "V4L2 negotiated capture format"
        );

        let negotiated = NegotiatedFormat {
            width,
            height,
            stride,
            pixel_format,
            yuv_colorimetry,
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

    fn run_capture_loop(
        format: NegotiatedFormat,
        tx: crossbeam_channel::Sender<CaptureMessage>,
        rx_cleaner: Receiver<CaptureMessage>,
        stop_rx: Receiver<()>,
        mut stream: MmapStream<'_>,
    ) {
        let disconnected_msg = "摄像头已断开连接 (设备可能已被拔出或 USB 断开)".to_string();
        loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let (buf, meta) = match stream.next() {
                Ok(res) => res,
                Err(e) => {
                    if e.kind() == ErrorKind::TimedOut {
                        // v4l::MmapStream queues before dequeueing. If dequeue times out,
                        // reset streaming before the next next() call to avoid re-queueing
                        // an already queued buffer on devices that stop producing frames.
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
            self.last_frame_at = Some(Instant::now());
            return Ok(frame);
        }

        // 2. 短时间内没有新帧时允许复用缓存，以适配 tick 快于设备 FPS 的场景。
        if let (Some(cached_frame), Some(received_at)) = (&self.last_frame, self.last_frame_at)
            && received_at.elapsed() <= STALE_FRAME_TIMEOUT
        {
            return Ok(cached_frame.clone());
        }

        // 3. 首帧或缓存过期后，必须等待新帧；不能无限发布冻结画面。
        match self.receiver.recv_timeout(FRAME_RECV_TIMEOUT) {
            Ok(CaptureMessage::Frame(f)) => {
                self.last_frame = Some(f.clone());
                self.last_frame_at = Some(Instant::now());
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
                self.last_frame_at = Some(Instant::now());
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
    use super::*;

    fn test_frame(value: u8) -> CapturedFrame {
        CapturedFrame {
            width: 1,
            height: 1,
            stride: 1,
            pixel_format: PixelFormat::Gray8,
            yuv_colorimetry: None,
            data: Bytes::from(vec![value]),
        }
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
