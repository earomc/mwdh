use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self},
};

use crate::{
    ArchiveOptions, FileToCompress, ProgressMessage,
    archive::{create_temp_dir, progress::handle_progress, scan_files},
};
use anyhow::{Context, Result};
use crossbeam::channel;
use zip::{ZipWriter, write::SimpleFileOptions};

pub async fn generate_zip_with_progress(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    args: ArchiveOptions,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    // Spawn blocking task for ZIP creation
    let zip_handle = tokio::task::spawn_blocking(move || {
        generate_zip_parallel(paths_to_be_archived, archive_output_path, tx, args)
    });

    // Handle progress updates on main thread
    let progress_handle = tokio::task::spawn_blocking(move || handle_progress(rx));

    // Wait for both tasks
    zip_handle.await??;
    progress_handle.await?;

    Ok(())
}

pub fn generate_zip_parallel(
    paths_to_be_archived: Vec<PathBuf>,
    archive_output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    args: ArchiveOptions,
) -> Result<()> {
    let all_files = scan_files(&tx, paths_to_be_archived, &args)?;

    // Second pass: compress files in parallel and write to individual temp ZIPs
    let (temp_dir, _cleanup_guard) = create_temp_dir()?;

    let (work_tx, work_rx) = channel::unbounded::<(usize, FileToCompress)>();
    let (result_tx, result_rx) = channel::unbounded::<Result<(usize, PathBuf)>>();

    // Spawn worker threads
    let workers: Vec<_> = (0..args.threads)
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

                        tx.send(ProgressMessage::FileCompressed(
                            worker_id,
                            file_info.file_name.clone(),
                        ))
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
