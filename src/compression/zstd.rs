use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
};

use crate::{collect_files_recursive, ArchiveOptions, FileToCompress, ProgressMessage};
use anyhow::{Context, Result};
use crossbeam::channel;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

enum MemoryManagerMessage {
    RequestAllocation(u64, channel::Sender<bool>),
}

pub async fn generate_zstd_with_progress(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    args: ArchiveOptions,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    // Spawn blocking task for ZSTD creation
    let zstd_handle = tokio::task::spawn_blocking(move || {
        generate_zstd(paths_to_be_archived, archive_output_path, tx, args)
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

                    // For batches, we might get a generic name or the current file being processed
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
    Disk(PathBuf), // path and size
}

struct BatchToCompress {
    files: Vec<FileToCompress>,
    total_size: u64,
}

pub fn generate_zstd(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: ArchiveOptions,
) -> Result<()> {
    // 1. Scan files
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

    // 2. Decide Mode
    if args.threads == 1 {
        // --- Sequential Mode (Best Ratio) ---
        println!("Using sequential mode");
        generate_zstd_sequential(all_files, archive_output_path, tx, args)
    } else {
        // --- Parallel Batch Mode (Fast + Good Ratio) ---
        println!("Using parallel mode");
        generate_zstd_parallel(all_files, archive_output_path, tx, args)
    }
}

/// Sequential Mode: Single Thread, Single Dictionary, Best Compression
fn generate_zstd_sequential(
    all_files: Vec<FileToCompress>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: ArchiveOptions,
) -> Result<()> {
    tx.send(ProgressMessage::StartWriting(all_files.len() as u64)).ok();

    let file = File::create(&archive_output_path)?;
    let mut encoder = zstd::Encoder::new(file, args.compression_level as i32)?;
    
    // We use standard tar builder here because we are strictly sequential
    let mut builder = tar::Builder::new(&mut encoder);

    for file_info in all_files.iter() {
        tx.send(ProgressMessage::Compressing(0, file_info.file_name.clone())).ok();
        
        let path_in_tar = Path::new(&file_info.file_name);
        
        builder.append_path_with_name(&file_info.src_path, path_in_tar)?;
        
        // Sequential mode updates both compression and writing stats simultaneously
        tx.send(ProgressMessage::FileCompressed(0, file_info.file_name.clone())).ok();
        tx.send(ProgressMessage::WritingFile(file_info.file_name.clone())).ok();
    }

    builder.finish()?; 
    drop(builder); 

    encoder.finish()?; // Finalizes Zstd stream

    let final_size = std::fs::metadata(&archive_output_path)?.len();
    tx.send(ProgressMessage::Complete(final_size)).ok();

    Ok(())
}

/// Parallel Mode: Chunked Files, Parallel Compression, Concatenated Frames
fn generate_zstd_parallel(
    all_files: Vec<FileToCompress>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: ArchiveOptions,
) -> Result<()> {
    // Prepare Temp Directory
    let temp_dir = std::env::temp_dir().join(format!("mwdh_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;
    let temp_dir_clone = temp_dir.clone();
    let _cleanup = scopeguard::guard((), move |_| {
        let _ = std::fs::remove_dir_all(&temp_dir_clone);
    });

    // Memory Manager Setup
    let global_memory_limit_bytes = args.memory_limit_mb * 1024 * 1024;
    
    let (mem_tx, mem_rx) = channel::unbounded::<MemoryManagerMessage>();

    let mem_manager_handle = std::thread::spawn(move || {
        let mut current_usage = 0u64;
        while let Ok(msg) = mem_rx.recv() {
            let MemoryManagerMessage::RequestAllocation(size, response_tx) = msg;
            let can_allocate = current_usage + size <= global_memory_limit_bytes;
            if can_allocate {
                current_usage += size;
            }
            let _ = response_tx.send(can_allocate);
        }
    });

    // Channels for Workers
    let (work_tx, work_rx) = channel::unbounded::<(usize, BatchToCompress)>();
    let (result_tx, result_rx) = channel::unbounded::<Result<(usize, CompressedFileData)>>();

    // Spawn Workers
    let workers: Vec<_> = (0..args.threads)
        .map(|worker_id| {
            let work_rx = work_rx.clone();
            let result_tx = result_tx.clone();
            let tx = tx.clone();
            let temp_dir = temp_dir.clone();
            let mem_tx = mem_tx.clone();

            std::thread::Builder::new()
                .name(format!("worker-{}", worker_id))
                .spawn(move || {
                    // Send an immediate "Idle" message to ensure the progress bar is created for this worker.
                    tx.send(ProgressMessage::Compressing(worker_id, "Idle".to_string())).ok();

                    while let Ok((batch_idx, batch)) = work_rx.recv() {
                        let result = compress_batch_to_zstd_frame(
                            &batch,
                            &temp_dir,
                            batch_idx,
                            args.compression_level,
                            global_memory_limit_bytes,
                            &mem_tx,
                            &tx, 
                            worker_id
                        );

                        if result_tx.send(result.map(|data| (batch_idx, data))).is_err() {
                            break;
                        }
                    }
                })
                .expect("Failed to spawn thread")
        })
        .collect();

    // --- Dynamic Batching Logic (Uses Total Size and Thread Count) ---
    
    // 1. Calculate total uncompressed size and store files with their sizes
    let mut files_with_size: Vec<(FileToCompress, u64)> = Vec::new();
    let mut total_uncompressed_size: u64 = 0;

    for file_info in all_files {
        // Assuming file metadata is fast enough to fetch here
        let size = std::fs::metadata(&file_info.src_path)
            .map(|m| m.len())
            .unwrap_or(0);
        total_uncompressed_size += size;
        files_with_size.push((file_info, size));
    }

    // 2. Define Batch Limits and calculate Dynamic Batch Size
    const MIN_BATCH_SIZE_BYTES: u64 = 64 * 1024 * 1024; // 64MB min for dictionary building
    const MAX_BATCH_SIZE_BYTES: u64 = 512 * 1024 * 1024; // 512MB max to prevent starvation on large files

    let num_threads = args.threads.max(1) as u64;

    // Calculate target size per thread. Use checked_div for safety.
    let target_size_per_thread = total_uncompressed_size.checked_div(num_threads).unwrap_or(MAX_BATCH_SIZE_BYTES);

    // Set batch threshold: Clamp the target size between MIN and MAX.
    let mut batch_threshold = target_size_per_thread
        .max(MIN_BATCH_SIZE_BYTES)
        .min(MAX_BATCH_SIZE_BYTES);

    // Handle edge case: if total size is smaller than the calculated threshold, use total size.
    // Use .max(1) to avoid a zero-sized batch_threshold if total_uncompressed_size is 0.
    batch_threshold = batch_threshold.min(total_uncompressed_size.max(1));

    println!("Total size: {}, Threads: {}, Calculated batch threshold: {}", 
        crate::format_bytes(total_uncompressed_size), num_threads, crate::format_bytes(batch_threshold));

    // 3. Batching Logic
    let mut current_batch = Vec::new();
    let mut current_batch_size = 0u64;
    let mut batch_index = 0;

    for (file_info, size) in files_with_size {
        current_batch.push(file_info);
        current_batch_size += size;

        // Check if we hit the dynamically calculated threshold
        // We ensure the current batch is not empty to prevent sending a batch with just padding/headers
        if current_batch_size >= batch_threshold && !current_batch.is_empty() {
            // Send the batch
            work_tx.send((batch_index, BatchToCompress {
                files: current_batch,
                total_size: current_batch_size
            })).ok();
            
            current_batch = Vec::new();
            current_batch_size = 0;
            batch_index += 1;
        }
    }
    
    // Send remaining files
    if !current_batch.is_empty() {
         work_tx.send((batch_index, BatchToCompress {
                files: current_batch,
                total_size: current_batch_size
            })).ok();
    }

    drop(work_tx);
    drop(result_tx);
    drop(mem_tx);

    // Collect Results
    let mut compressed_batches: Vec<(usize, CompressedFileData)> = Vec::new();
    for result in result_rx {
        compressed_batches.push(result?);
    }
    compressed_batches.sort_by_key(|(idx, _)| *idx);

    for worker in workers {
        worker.join().ok();
    }
    mem_manager_handle.join().ok();

    // Writing Phase
    tx.send(ProgressMessage::StartWriting(compressed_batches.len() as u64)).ok(); 
    let mut output_file = std::fs::File::create(&archive_output_path)?;

    for (_, compressed_file) in compressed_batches.iter() {
        tx.send(ProgressMessage::WritingFile(
            compressed_file.file_name.clone(),
        )).ok();

        match &compressed_file.data {
            CompressedDataLocation::Memory(data) => {
                output_file.write_all(data)?;
            }
            CompressedDataLocation::Disk(temp_file_path) => {
                let mut temp_file = std::fs::File::open(temp_file_path)?;
                std::io::copy(&mut temp_file, &mut output_file)?;
            }
        }
    }

    // Append Final Tar EOFs
    {
        let mut end_marker_data = Vec::new();
        let mut encoder =
            zstd::Encoder::new(&mut end_marker_data, args.compression_level as i32)?;
        let zeros = [0u8; 1024];
        encoder.write_all(&zeros)?;
        encoder.finish()?;
        output_file.write_all(&end_marker_data)?;
    }

    output_file.sync_all()?;
    let final_size = std::fs::metadata(&archive_output_path)?.len();
    tx.send(ProgressMessage::Complete(final_size)).ok();

    Ok(())
}

fn compress_batch_to_zstd_frame(
    batch: &BatchToCompress,
    temp_dir: &Path,
    batch_idx: usize,
    compression_level: i8,
    global_memory_limit_bytes: u64,
    mem_tx: &channel::Sender<MemoryManagerMessage>,
    progress_tx: &mpsc::Sender<ProgressMessage>,
    worker_id: usize,
) -> Result<CompressedFileData> {
    // If batch's uncompressed size is larger than the global memory limit, 
    // write straight to disk to avoid OOM by holding compressed data in memory.
    let direct_to_disk = batch.total_size > global_memory_limit_bytes;

    let mut disk_file: Option<File>; 
    let mut mem_buffer: Option<Vec<u8>> = None;
    
    let mut sink: Box<dyn Write + Send> = if direct_to_disk {
        let temp_file_path = temp_dir.join(format!("batch_{}.zst", batch_idx));
        let f = File::create(&temp_file_path)?;
        disk_file = Some(f);
        Box::new(disk_file.as_mut().unwrap())
    } else {
        mem_buffer = Some(Vec::new());
        Box::new(mem_buffer.as_mut().unwrap())
    };

    {
        let mut encoder = zstd::Encoder::new(&mut sink, compression_level as i32)?;

        // Iterate files in the batch
        for file_info in &batch.files {
            // Send progress update
            progress_tx.send(ProgressMessage::Compressing(worker_id, file_info.file_name.clone())).ok();

            // 1. Manual Tar Header
            let mut header = tar::Header::new_gnu();
            let meta = std::fs::metadata(&file_info.src_path)?;
            header.set_metadata(&meta);
            header.set_size(meta.len());
            
            let path_in_tar = Path::new(&file_info.file_name);
            if let Err(e) = header.set_path(path_in_tar) {
                return Err(anyhow::anyhow!("Failed to set path: {}", e));
            }
            header.set_cksum();
            encoder.write_all(header.as_bytes())?;

            // 2. File Content
            let mut input_file = File::open(&file_info.src_path)?;
            std::io::copy(&mut input_file, &mut encoder)?;

            // 3. Padding
            const TAR_BLOCK_SIZE: u64 = 512;
            
            // AI helped here
            let padding_needed = (TAR_BLOCK_SIZE - (meta.len() % TAR_BLOCK_SIZE)) % TAR_BLOCK_SIZE;
            if padding_needed > 0 {
                let zeros = vec![0u8; padding_needed as usize];
                encoder.write_all(&zeros)?;
            }

            // Mark this file as done in the UI
            progress_tx.send(ProgressMessage::FileCompressed(worker_id, file_info.file_name.clone())).ok();
        }
        
        encoder.finish()?;
    }
    
    drop(sink);

    let batch_name = format!("Batch {}", batch_idx);

    if direct_to_disk {
        let temp_file_path = temp_dir.join(format!("batch_{}.zst", batch_idx));
        Ok(CompressedFileData {
            file_name: batch_name,
            data: CompressedDataLocation::Disk(temp_file_path),
        })
    } else {
        let compressed_data = mem_buffer.unwrap();
        let compressed_size = compressed_data.len() as u64;

        let (response_tx, response_rx) = channel::bounded(1);
        mem_tx.send(MemoryManagerMessage::RequestAllocation(compressed_size, response_tx)).ok();

        // The Memory Manager checks if the global limit is exceeded.
        if response_rx.try_recv().unwrap_or(false) {
            // Allocation successful, keep in memory
            Ok(CompressedFileData {
                file_name: batch_name,
                data: CompressedDataLocation::Memory(compressed_data),
            })
        } else {
            // Allocation failed (global limit reached), write to disk as a fallback
            let temp_file_path = temp_dir.join(format!("batch_{}.zst", batch_idx));
            std::fs::write(&temp_file_path, &compressed_data)?;
            Ok(CompressedFileData {
                file_name: batch_name,
                data: CompressedDataLocation::Disk(temp_file_path),
            })
        }
    }
}
