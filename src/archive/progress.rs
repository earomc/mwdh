use std::{path::Path, sync::mpsc::Receiver};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::ProgressMessage;

pub fn handle_progress(rx: Receiver<ProgressMessage>) {
    let multi = MultiProgress::new();

    let scan_bar = multi.add(ProgressBar::new_spinner());
    scan_bar.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} {msg}")
            .unwrap(),
    );

    let mut worker_bars: Vec<ProgressBar> = Vec::new();
    let mut compression_bar: Option<ProgressBar> = None;
    let mut write_bar: Option<ProgressBar> = None;
    let mut compressed_count = 0u64;
    let mut written_count = 0u64;

    while let Ok(msg) = rx.recv() {
        match msg {
            ProgressMessage::StartScanning => {
                scan_bar.set_message("Scanning directories...");
            }
            ProgressMessage::FileFound(name) => {
                scan_bar.set_message(format!(
                    "Found: {}",
                    Path::new(&name)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }
            ProgressMessage::StartCompression(total) => {
                scan_bar.finish_with_message(format!("Found {} files", total));

                // Create compression progress bar
                let pg = multi.add(ProgressBar::new(total));
                pg.set_style(
                    ProgressStyle::default_bar()
                        .template("{spinner} Compressing: [{elapsed_precise}] {wide_bar} {percent}% {pos}/{len} (ETA: {eta})")
                        .unwrap()
                );
                compression_bar = Some(pg);
            }
            ProgressMessage::Compressing(worker_id, filename) => {
                // Ensure we have enough worker bars with bounds checking
                // This is where the bar is initialized for a worker_id
                while worker_bars.len() <= worker_id {
                    let bar_id = worker_bars.len();
                    let pb = multi.add(ProgressBar::new_spinner());
                    pb.set_style(
                        ProgressStyle::default_spinner()
                            .template(&format!("{{spinner}} Worker {}: {{msg}}", bar_id))
                            .unwrap(),
                    );
                    worker_bars.push(pb);
                }

                let short_name = Path::new(&filename)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();

                if let Some(bar) = worker_bars.get(worker_id) {
                    bar.set_message(format!("{}", short_name));
                }
            }
            ProgressMessage::FileCompressed(worker_id, _filename) => {
                compressed_count += 1;

                if let Some(ref pb) = compression_bar {
                    pb.set_position(compressed_count);
                }

                if let Some(bar) = worker_bars.get(worker_id) {
                    bar.set_message("Idle".to_string());
                }
            }
            ProgressMessage::StartWriting(total) => {
                // Finish compression phase
                if let Some(ref pb) = compression_bar {
                    pb.finish_with_message("All files compressed!");
                }
                for bar in &worker_bars {
                    bar.finish_and_clear();
                }

                // Create write progress bar
                let wb = multi.add(ProgressBar::new(total));
                wb.set_style(
                    ProgressStyle::default_bar()
                        .template("{spinner} Writing archive: [{elapsed_precise}] {wide_bar} {percent}% {pos}/{len} - {msg}")
                        .unwrap()
                );
                write_bar = Some(wb);
            }
            ProgressMessage::WritingFile(filename) => {
                written_count += 1;

                if let Some(ref pb) = write_bar {
                    pb.set_position(written_count);
                    let short_name = Path::new(&filename)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    pb.set_message(short_name.to_string());
                }
            }
            ProgressMessage::Complete(file_size) => {
                if let Some(ref pb) = write_bar {
                    pb.finish_with_message(format!(
                        "Archive created successfully! ({})",
                        crate::format_bytes(file_size)
                    ));
                }
                break;
            }
        }
    }
}
