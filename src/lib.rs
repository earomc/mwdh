pub mod cli;
pub mod compression;
pub mod server;

use anyhow::{Context, Result};
use clap::ValueEnum;
use std::{
    error,
    fmt::Display,
    path::{Path, PathBuf},
    str::FromStr,
    sync::mpsc,
};

#[derive(Debug, Clone)]
pub enum ProgressMessage {
    StartScanning,
    FileFound(String),
    StartCompression(u64),         // total files to compress
    Compressing(usize, String),    // worker_id, filename
    FileCompressed(usize, String), // worker_id, filename
    StartWriting(u64),             // total files to write
    WritingFile(String),           // filename being written to final ZIP
    Complete(u64),                 // final zip file size in bytes
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
    pub fn get_file_ending(&self) -> &'static str {
        match self {
            CompressionFormat::ZipDeflate => "zip",
            CompressionFormat::TarZstd => "tar.zst",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompressionFormat {
    ZipDeflate,
    TarZstd,
}

impl Display for CompressionFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CompressionFormat::ZipDeflate => "zip",
            CompressionFormat::TarZstd => "zstd",
        })
    }
}

impl FromStr for CompressionFormat {
    type Err = CompressionFormatParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "zip" => Ok(CompressionFormat::ZipDeflate),
            "zstd" => Ok(CompressionFormat::TarZstd),
            _ => Err(CompressionFormatParseError),
        }
    }
}

#[derive(Debug)]
pub struct CompressionFormatParseError;

impl error::Error for CompressionFormatParseError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        None
    }
}

impl Display for CompressionFormatParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompressionFormatParseError")
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

#[derive(Clone)]
pub enum MwdhOptions {
    Server(ServerOptions),
    Archive(ArchiveOptions),
    Both {
        server: ServerOptions,
        archive: ArchiveOptions,
    },
}

#[derive(Clone)]
pub struct ArchiveOptions {
    /// Path to the minecraft server/saves directory that contains /world, /world_nether and /world_the_end
    pub world_path: String,

    /// Name of the world directory defined in server.properties if you're hosting a singleplayer world on a desktop system
    pub world_name: String,

    /// Specify the name of the archive - Note: (mwdh will append a file-ending to it)
    pub archive_name: String,

    /// Include the Nether dimension ("world_nether")
    pub include_nether: bool,

    /// Include the End dimension ("world_the_end")
    pub include_end: bool,

    /// Include the Overworld ("world")
    pub include_overworld: bool,

    /// Number of threads for parallel compression (0 = auto-detect)
    pub threads: usize,

    /// The level of compression to apply. For zstd use -7 to 22, for zip use 0 to 9
    pub compression_level: i8,

    /// The compression format to compress the world. Either zip or zstd
    pub compression_format: CompressionFormat,

    /// Whether or not the world format is Bukkit/Spigot/Paper-based. With those servers, the Nether and End dimensions are split up into their seperate directories (world_nether, world_the_end).
    /// If you're using a vanilla or Fabric server, dimensions will be inside of the world directory split up into DIM-1 (Nether) and DIM1 (The End).
    pub is_bukkit: bool, // TODO: Find out what format Forge or other loaders/servers use.

    /// Limit in MB until the compression algorithm stores the compression intermediaries on disk in a temp directory.
    pub memory_limit_mb: u64,
}

#[derive(Clone)]
pub struct ServerOptions {
    /// Host path from where to download the world files
    pub host_path: String,

    /// IP address to serve on
    pub bind: String,

    /// Port to serve on
    pub port: u16,

    /// Number of threads for file serving (0 = auto-detect)
    pub threads: usize,

    pub path_to_archive: Option<PathBuf>,
    
    /// Compression format used in the http header to signal to the browser what kind of data is downloaded.
    pub compression_format: CompressionFormat,
}

pub fn paths_to_be_archived(args: &ArchiveOptions) -> Vec<PathBuf> {
    let base = PathBuf::from(&args.world_path);

    let mut paths_to_be_archived = Vec::with_capacity(3);
    
    if args.is_bukkit {
        if args.include_overworld {
            paths_to_be_archived.push(base.join("world"));
        }
        if args.include_nether {
            paths_to_be_archived.push(base.join("world_nether"));
        }
        if args.include_end {
            paths_to_be_archived.push(base.join("world_the_end"));
        }
    } else {
        paths_to_be_archived.push(base.join("world"));
        // else: if is not bukkit and nether and/or end are not included we need to skip DIM-1 and/or DIM1 directories later in the file collection.
    } 
    paths_to_be_archived
}

pub fn collect_files_recursive(
    base_dir: &Path,
    archive_prefix: &str,
    all_files: &mut Vec<FileToCompress>,
    args: &ArchiveOptions,
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
                if !args.is_bukkit {
                    if !args.include_end && entry.file_name() == "DIM1" {
                        continue;
                    }
                    if !args.include_nether && entry.file_name() == "DIM-1" {
                        continue;
                    }
                    if !args.include_overworld
                        && entry
                            .path()
                            .parent()
                            .and_then(|parent| parent.file_name())
                            .and_then(|file_name| file_name.to_str())
                            .is_some_and(|file_name| file_name == args.world_name) // basically checks if parent dir is the world dir that contains the overworld. just looks crazy because of all the conversions and Options.
                        && (entry.file_name() == "regions" || entry.file_name() == "entities" || entry.file_name() == "poi")
                    {
                        continue; // skip regions, entities and poi directories in the main world directory.
                    }
                }
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
