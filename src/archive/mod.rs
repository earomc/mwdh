pub mod zip;
pub mod zstd;
pub mod progress;

use crate::{ArchiveOptions, CompressionFormat, FileToCompress, ProgressMessage, archive, collect_files_recursive, paths_to_be_archived};
use anyhow::{Context, Result};
use scopeguard::ScopeGuard;
use std::{path::{Path, PathBuf}, process, sync::mpsc::Sender};

fn print_archiving_info(options: &ArchiveOptions) {
    let path = Path::new(&options.world_path);
    if !path.exists() {
        eprintln!("ERR: Given path does not exist");
        process::exit(1);
    }
    if !path.is_dir() {
        eprintln!("ERR: Path should be a directory");
        process::exit(1);
    }
    let absolute_path = std::fs::canonicalize(&path).unwrap_or(path.into());
    println!(
        "(Server) worlds directory: {}",
        absolute_path.to_string_lossy()
    );

    let mut inclusions = String::from("Including ");
    let mut i = 0;
    if options.include_overworld {
        inclusions.push_str("Overworld");
        i += 1;
    }
    if options.include_nether {
        if i > 0 {
            inclusions.push_str(", ");
        }
        inclusions.push_str("Nether");
        i += 1;
    }
    if options.include_end {
        if i > 0 && i < 3 {
            inclusions.push_str(", ");
        }
        inclusions.push_str("The End");
    }
    println!("{}", inclusions);
    println!(
        "Compressing to \"{}\" using {} at level {} with {} threads",
        format!("{}.{}", options.archive_name, options.compression_format.get_file_ending()),
        options.compression_format,
        options.compression_level,
        options.threads
    );
}

pub async fn do_compression(
    options: ArchiveOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    print_archiving_info(&options);
    let archive_output_path =
        Path::new(&options.archive_name).with_extension(options.compression_format.get_file_ending());
    let paths_to_be_archived = paths_to_be_archived(&options);
    match options.compression_format {
        CompressionFormat::ZipDeflate => {
            archive::zip::generate_zip_with_progress(
                paths_to_be_archived,
                archive_output_path.clone(),
                options.clone(),
            )
            .await
            .context("Failed to generate ZIP file")?;
        }
        CompressionFormat::TarZstd => {
            archive::zstd::generate_zstd_with_progress(
                paths_to_be_archived,
                archive_output_path.clone(),
                options.clone(),
            )
            .await
            .context("Failed to generate tar.zst file")?;
        }
    }
    Ok(())
}

#[must_use]
pub fn create_temp_dir() -> Result<(PathBuf, ScopeGuard<(), impl FnOnce(())>)> {
    let temp_dir = std::env::temp_dir().join(format!("mwdh_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;
    let temp_dir_clone = temp_dir.clone();
    let cleanup_guard = scopeguard::guard((), move |_| {
        let _ = std::fs::remove_dir_all(&temp_dir_clone);
    });
    Ok((temp_dir, cleanup_guard))
}

pub fn scan_files(tx: &Sender<ProgressMessage>, paths_to_be_archived: Vec<PathBuf>, args: &ArchiveOptions) -> Result<Vec<FileToCompress>> {
    // Scan files
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
    Ok(all_files)
}
