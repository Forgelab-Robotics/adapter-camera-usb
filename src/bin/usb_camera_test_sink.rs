use std::ops::Deref;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_array::{RecordBatch, StructArray};
use clap::Parser;
use dora_node_api::{DoraNode, Event};
use eyre::{Result, WrapErr, eyre};
use forge_msgs::{CompressedImage, Image};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(about = "Decode and report USB camera Dora messages")]
struct Cli {
    #[arg(long)]
    config: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SinkConfig {
    #[serde(default = "default_input_id")]
    input_id: String,
    #[serde(default = "default_log_every")]
    log_every: u64,
}

fn default_input_id() -> String {
    "image".to_string()
}

fn default_log_every() -> u64 {
    30
}

fn receive_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn should_log(count: u64, log_every: u64) -> bool {
    count == 1 || count.is_multiple_of(log_every.max(1))
}

fn decode_and_describe(data: &dora_node_api::ArrowData) -> Result<String> {
    let array = data.deref();
    let struct_array = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| eyre!("input is not an Arrow StructArray"))?;
    let batch = RecordBatch::from(struct_array.clone());

    if let Ok(image) = Image::from_record_batch(&batch) {
        return Ok(format!(
            "message=Image encoding={} width={} height={} bytes={}",
            image.encoding,
            image.width,
            image.height,
            image.data.len()
        ));
    }

    let image = CompressedImage::from_record_batch(&batch)
        .map_err(|err| eyre!("not Image or CompressedImage: {err}"))?;
    let decoded = image
        .to_rgb8_ndarray()
        .map_err(|err| eyre!("failed to decode {} image: {err}", image.format))?;
    let (height, width, _) = decoded.dim();
    Ok(format!(
        "message=CompressedImage encoding={} width={} height={} bytes={}",
        image.format,
        width,
        height,
        image.data.len()
    ))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_text = std::fs::read_to_string(&cli.config)
        .wrap_err_with(|| format!("failed to read {}", cli.config.display()))?;
    let config: SinkConfig = serde_yaml::from_str(&config_text)
        .wrap_err_with(|| format!("failed to parse {}", cli.config.display()))?;
    let (_node, mut events) = DoraNode::init_from_env()?;
    let mut count: u64 = 0;

    while let Some(event) = events.recv() {
        match event {
            Event::Input { id, data, .. } if id.as_str() == config.input_id.as_str() => {
                count += 1;
                if !should_log(count, config.log_every) {
                    continue;
                }
                let received_at_ms = receive_time_ms();
                match decode_and_describe(&data) {
                    Ok(description) => {
                        println!("count={count} received_at_unix_ms={received_at_ms} {description}")
                    }
                    Err(err) => eprintln!(
                        "count={count} received_at_unix_ms={received_at_ms} decode_error={err}"
                    ),
                }
            }
            Event::Stop(_) => break,
            Event::Error(error) => return Err(eyre!("Dora error: {error}")),
            _ => {}
        }
    }
    Ok(())
}
