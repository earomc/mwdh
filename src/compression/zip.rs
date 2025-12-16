use std::{path::{Path, PathBuf}, sync::mpsc};

use anyhow::{Context, Result};
use crossbeam::channel;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use crate::{Args, FileToCompress, ProgressMessage, collect_files_recursive};
use zip::{ZipWriter, write::SimpleFileOptions};

pub async fn generate_zip_with_progress(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    args: Args
) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    // Spawn blocking task for ZIP creation
    let zip_handle = tokio::task::spawn_blocking(move || {
        generate_zip_blocking(paths_to_be_archived, archive_output_path, tx, args)
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
                            .template("{spinner} Writing ZIP: [{elapsed_precise}] {wide_bar} {percent}% {pos}/{len} - {msg}")
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
                            "ZIP file created successfully! ({})",
                            crate::format_bytes(file_size)
                        ));
                    }
                    break;
                }
            }
        }
    });

    // Wait for both tasks
    zip_handle.await??;
    progress_handle.await?;

    Ok(())
}

pub fn generate_zip_blocking(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: Args
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

    // Second pass: compress files in parallel and write to individual temp ZIPs
    let temp_dir = std::env::temp_dir().join(format!("mwdh_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;

    let temp_dir_clone = temp_dir.clone();
    let _cleanup = scopeguard::guard((), move |_| {
        let _ = std::fs::remove_dir_all(&temp_dir_clone);
    });

    let (work_tx, work_rx) = channel::unbounded::<(usize, FileToCompress)>();
    let (result_tx, result_rx) = channel::unbounded::<Result<(usize, PathBuf)>>();

    // Spawn worker threads
    let workers: Vec<_> = (0..args.compression_threads)
        .map(|worker_id| {
            let work_rx = work_rx.clone();
            let result_tx = result_tx.clone();
            let tx = tx.clone();
            let temp_dir = temp_dir.clone();

            std::thread::Builder::new()
                .name(format!("worker-{}", worker_id))
                .spawn(move || {
                    while let Ok((idx, file_info)) = work_rx.recv() {
                        tx.send(ProgressMessage::Compressing(
                            worker_id,
                            file_info.file_name.clone(),
                        ))
                        .ok();

                        let result = compress_single_file_to_zip(
                            &file_info,
                            &temp_dir,
                            idx,
                            args.compression_level,
                        );

                        tx.send(ProgressMessage::FileCompressed(worker_id, file_info.file_name.clone()))
                            .ok();

                        if result_tx.send(result.map(|path| (idx, path))).is_err() {
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

    // Collect results
    let mut temp_zips = vec![None; all_files.len()];
    for result in result_rx {
        let (idx, temp_zip_path) = result?;
        temp_zips[idx] = Some(temp_zip_path);
    }

    // Wait for workers
    for worker in workers {
        worker.join().ok();
    }

    // Third pass: merge all individual ZIPs into final ZIP
    tx.send(ProgressMessage::StartWriting(all_files.len() as u64))
        .ok();

    let file = std::fs::File::create(&archive_output_path)?;
    let mut final_zip = ZipWriter::new(file);

    for (file_info, temp_zip_opt) in all_files.iter().zip(temp_zips.iter()) {
        let temp_zip_path = temp_zip_opt
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing temp ZIP"))?;

        tx.send(ProgressMessage::WritingFile(file_info.file_name.clone()))
            .ok();

        // Open temp ZIP and copy the file
        let temp_zip_file = std::fs::File::open(temp_zip_path)?;
        let mut temp_archive = zip::ZipArchive::new(temp_zip_file)?;
        
        // There should be exactly one file in each temp ZIP
        let file_in_zip = temp_archive.by_index(0)?;
        
        // Copy using raw_copy_file
        final_zip.raw_copy_file(file_in_zip)?;
    }

    final_zip.finish().context("Failed to finish ZIP")?;

    let final_size = std::fs::metadata(&archive_output_path)
        .context("Failed to get ZIP file size")?
        .len();

    tx.send(ProgressMessage::Complete(final_size)).ok();

    Ok(())
}

pub fn compress_single_file_to_zip(
    file_info: &FileToCompress,
    temp_dir: &Path,
    idx: usize,
    compression_level: i8,
) -> Result<PathBuf> {
    let temp_zip_path = temp_dir.join(format!("file_{}.zip", idx));
    let temp_file = std::fs::File::create(&temp_zip_path)?;
    let mut zip = ZipWriter::new(temp_file);
    
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(compression_level as i64))
        .large_file(true);

    zip.start_file(&file_info.file_name, options)?;
    
    let mut input_file = std::fs::File::open(&file_info.src_path)?;
    std::io::copy(&mut input_file, &mut zip)?;
    
    zip.finish()?;
    
    Ok(temp_zip_path)
}
