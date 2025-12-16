pub mod zip;
pub mod zstd;

use crate::{ArchiveOptions, CompressionFormat, compression, paths_to_be_archived};
use anyhow::Context;
use std::{path::Path, process};

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
            compression::zip::generate_zip_with_progress(
                paths_to_be_archived,
                archive_output_path.clone(),
                options.clone(),
            )
            .await
            .context("Failed to generate ZIP file")?;
        }
        CompressionFormat::TarZstd => {
            compression::zstd::generate_zstd_with_progress(
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
