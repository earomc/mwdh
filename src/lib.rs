pub mod zip_compression;

use std::{path::{Path, PathBuf}, str::FromStr, sync::mpsc};
use anyhow::{Context, Result};
use clap::ValueEnum;

#[derive(Debug, Clone)]
pub enum ProgressMessage {
    StartScanning,
    FileFound(String),
    StartCompression(u64),           // total files to compress
    Compressing(usize, String), // worker_id, filename
    FileCompressed(usize, String),   // worker_id, filename
    StartWriting(u64),               // total files to write
    WritingFile(String),             // filename being written to final ZIP
    Complete(u64),                   // final zip file size in bytes
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompressionFormat {
    ZipDeflate,
    TarZstd
}

#[derive(Clone)]
pub struct FileToCompress {
    pub src_path: PathBuf,
    pub file_name: String, // when compressing with Deflate/ZIP, this is the path to a compressed file located in the temp folder
}

impl CompressionFormat {
    pub fn get_mime_type(&self) -> &'static str {
        match self {
            CompressionFormat::ZipDeflate => "application/zip",
            CompressionFormat::TarZstd => "application/zstd",
        }
    }
}

pub struct CompressionFormatParseError;
impl FromStr for CompressionFormat {
    type Err = CompressionFormatParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "zip" => Ok(CompressionFormat::ZipDeflate),
            "zstd" => Ok(CompressionFormat::TarZstd),
            _ => Err(CompressionFormatParseError)
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn collect_files_recursive(
    base_dir: &Path,
    archive_prefix: &str,
    all_files: &mut Vec<FileToCompress>,
    tx: &mpsc::Sender<ProgressMessage>,
) -> Result<()> {
    let mut stack = vec![(base_dir.to_path_buf(), archive_prefix.to_string())]; // current path, current zip path

    while let Some((curr_fs_path, curr_zip_path)) = stack.pop() {
        let read_dir = std::fs::read_dir(&curr_fs_path)
            .with_context(|| format!("Failed to read: {}", curr_fs_path.display()))?;

        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let child_zip_path = format!("{}/{}", curr_zip_path, name);

            let meta = entry.metadata()?;

            if meta.is_dir() {
                stack.push((path, child_zip_path));
            } else if meta.is_file() {
                all_files.push(FileToCompress {
                    src_path: path.clone(),
                    file_name: child_zip_path,
                });
                tx.send(ProgressMessage::FileFound(path.display().to_string()))
                    .ok();
            }
        }
    }

    Ok(())
}
