use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

use crate::{Args, FileToCompress, ProgressMessage, collect_files_recursive};
use anyhow::{Context, Result};
use crossbeam::channel;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

enum MemoryManagerMessage {
    RequestAllocation(u64, channel::Sender<bool>),
}

pub async fn generate_zstd_with_progress(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    args: Args,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    // Spawn blocking task for ZSTD creation
    let zstd_handle = tokio::task::spawn_blocking(move || {
        generate_zstd_blocking(paths_to_be_archived, archive_output_path, tx, args)
    });

    // Handle progress updates on main thread
    let progress_handle = tokio::task::spawn_blocking(move || {
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
    });

    // Wait for both tasks
    zstd_handle.await??;
    progress_handle.await?;

    Ok(())
}

struct CompressedFileData {
    file_name: String,
    data: CompressedDataLocation,
}

enum CompressedDataLocation {
    Memory(Vec<u8>),
    Disk(PathBuf, u64), // path and size
}

pub fn generate_zstd_blocking(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: Args,
) -> Result<()> {
    // First pass: count all files
    tx.send(ProgressMessage::StartScanning).ok();
    let mut all_files = Vec::new();

    for path in &paths_to_be_archived {
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid path: {}", path.display()))?
            .to_string_lossy()
            .to_string();

        let meta = std::fs::metadata(path)
            .with_context(|| format!("Failed to stat: {}", path.display()))?;

        if meta.is_file() {
            all_files.push(FileToCompress {
                src_path: path.clone(),
                file_name: name,
            });
            tx.send(ProgressMessage::FileFound(path.display().to_string()))
                .ok();
        } else {
            collect_files_recursive(path, &name, &mut all_files, &args, &tx)?;
        }
    }

    let total_files = all_files.len() as u64;
    tx.send(ProgressMessage::StartCompression(total_files)).ok();

    // Second pass: compress files in parallel to temp files or memory
    let temp_dir = std::env::temp_dir().join(format!("mwdh_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;

    let temp_dir_clone = temp_dir.clone();
    let _cleanup = scopeguard::guard((), move |_| {
        let _ = std::fs::remove_dir_all(&temp_dir_clone);
    });

    // Memory manager channel
    let (mem_tx, mem_rx) = channel::unbounded::<MemoryManagerMessage>();
    let memory_limit = args.memory_limit_mb * 1024 * 1024; // Convert MB to bytes

    // Spawn memory manager thread
    let mem_manager_handle = std::thread::spawn(move || {
        let mut current_usage = 0u64;

        while let Ok(msg) = mem_rx.recv() {
            let MemoryManagerMessage::RequestAllocation(size, response_tx) = msg;
            let can_allocate = current_usage + size <= memory_limit;
            if can_allocate {
                current_usage += size;
            }
            let _ = response_tx.send(can_allocate);
        }
    });

    let (work_tx, work_rx) = channel::unbounded::<(usize, FileToCompress)>();
    let (result_tx, result_rx) = channel::unbounded::<Result<(usize, CompressedFileData)>>();

    // Spawn worker threads
    let workers: Vec<_> = (0..args.compression_threads)
        .map(|worker_id| {
            let work_rx = work_rx.clone();
            let result_tx = result_tx.clone();
            let tx = tx.clone();
            let temp_dir = temp_dir.clone();
            let mem_tx = mem_tx.clone();

            std::thread::Builder::new()
                .name(format!("worker-{}", worker_id))
                .spawn(move || {
                    while let Ok((idx, file_info)) = work_rx.recv() {
                        tx.send(ProgressMessage::Compressing(
                            worker_id,
                            file_info.file_name.clone(),
                        ))
                        .ok();

                        let result = compress_single_file_to_zstd(
                            &file_info,
                            &temp_dir,
                            idx,
                            args.compression_level,
                            &mem_tx,
                        );

                        tx.send(ProgressMessage::FileCompressed(
                            worker_id,
                            file_info.file_name.clone(),
                        ))
                        .ok();

                        if result_tx.send(result.map(|data| (idx, data))).is_err() {
                            break;
                        }
                    }
                })
                .expect("Failed to spawn thread")
        })
        .collect();

    // Send work to workers
    for (idx, file_info) in all_files.iter().enumerate() {
        work_tx.send((idx, file_info.clone())).ok();
    }
    drop(work_tx);
    drop(result_tx);
    drop(mem_tx); // Close memory manager channel

    // Collect results
    let mut compressed_files = Vec::with_capacity(all_files.len());
    for result in result_rx {
        let (_, data) = result?;
        compressed_files.push(data);
    }

    // Wait for workers
    for worker in workers {
        worker.join().ok();
    }

    // Wait for memory manager
    mem_manager_handle.join().ok();

    // Third pass: write all compressed files into a single tar.zst archive
    tx.send(ProgressMessage::StartWriting(all_files.len() as u64))
        .ok();

    let output_file = std::fs::File::create(&archive_output_path)?;
    let mut encoder = zstd::Encoder::new(output_file, 0)?; // Use fast compression for tar layer

    let mut tar_builder = tar::Builder::new(&mut encoder);

    for compressed_file in compressed_files.iter() {
        tx.send(ProgressMessage::WritingFile(
            compressed_file.file_name.clone(),
        ))
        .ok();

        match &compressed_file.data {
            CompressedDataLocation::Memory(data) => {
                // Data is in memory - write directly
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();

                let archived_name = format!("{}.zst", compressed_file.file_name);
                tar_builder.append_data(&mut header, &archived_name, &data[..])?;
            }
            CompressedDataLocation::Disk(temp_file_path, compressed_size) => {
                // Data is on disk - read from temp file
                let mut temp_file = std::fs::File::open(temp_file_path)?;

                let mut header = tar::Header::new_gnu();
                header.set_size(*compressed_size);
                header.set_mode(0o644);
                header.set_cksum();

                let archived_name = format!("{}.zst", compressed_file.file_name);
                tar_builder.append_data(&mut header, &archived_name, &mut temp_file)?;
            }
        }
    }

    tar_builder.finish()?;
    drop(tar_builder);
    encoder.finish()?;

    let final_size = std::fs::metadata(&archive_output_path)
        .context("Failed to get archive file size")?
        .len();

    tx.send(ProgressMessage::Complete(final_size)).ok();

    Ok(())
}

fn compress_single_file_to_zstd(
    file_info: &FileToCompress,
    temp_dir: &Path,
    idx: usize,
    compression_level: i8,
    mem_tx: &channel::Sender<MemoryManagerMessage>,
) -> Result<CompressedFileData> {
    // Compress to memory first
    let mut compressed_data = Vec::new();
    let mut encoder = zstd::Encoder::new(&mut compressed_data, compression_level as i32)?;

    let mut input_file = std::fs::File::open(&file_info.src_path)?;
    std::io::copy(&mut input_file, &mut encoder)?;
    encoder.finish()?;

    let compressed_size = compressed_data.len() as u64;

    // Ask memory manager if we can keep this in memory (non-blocking)
    let (response_tx, response_rx) = channel::bounded(1);
    mem_tx
        .send(MemoryManagerMessage::RequestAllocation(
            compressed_size,
            response_tx,
        ))
        .ok();

    // Don't wait for response - check immediately if available
    let can_keep_in_memory = response_rx.try_recv().unwrap_or(false);

    if can_keep_in_memory {
        // Keep in memory
        Ok(CompressedFileData {
            file_name: file_info.file_name.clone(),
            data: CompressedDataLocation::Memory(compressed_data),
        })
    } else {
        // Write to disk to free memory
        let temp_file_path = temp_dir.join(format!("file_{}.zst", idx));
        std::fs::write(&temp_file_path, &compressed_data)?;

        Ok(CompressedFileData {
            file_name: file_info.file_name.clone(),
            data: CompressedDataLocation::Disk(temp_file_path, compressed_size),
        })
    }
}
