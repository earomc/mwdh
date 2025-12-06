use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use std::fs::File;
use std::net::SocketAddr;
use std::process;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tokio::fs::{self};
use tokio::net::TcpListener;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::mpsc;
use crossbeam::channel;
use std::sync::Mutex;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// Path to the (minecraft server) directory that contains /world, /world_nether and /world_the_end
    #[arg(short = 'p', long = "path")]
    pub path: String,

    /// Include the Nether dimension ("world_nether")
    #[arg(short = 'n', long = "include-nether", default_value_t = false)]
    pub include_nether: bool,

    /// Include the End dimension ("world_the_end")
    #[arg(short = 'e', long = "include-end", default_value_t = false)]
    pub include_end: bool,

    /// Specify the download file name - Note: (mwdh will append '.zip' to it)
    #[arg(short = 'f', long = "file-name", default_value = "world")]
    pub download_file_name: String,

    /// Host path from where to download the world files
    #[arg(short = 'H', long = "host-path", default_value = "world")]
    pub host_path: String,

    /// IP address to serve on
    #[arg(long = "host-ip", default_value = "0.0.0.0")]
    pub host_ip: String,

    /// Port to serve on
    #[arg(short = 'P', long = "port", default_value_t = 3000)]
    pub port: u16,

    /// Number of threads for parallel compression (0 = auto-detect)
    #[arg(short = 't', long = "threads", default_value_t = 0)]
    pub threads: usize,
}

#[derive(Debug, Clone)]
enum ProgressMessage {
    StartScanning,
    FileFound(String),
    StartCompression(u64), // total files to compress
    StartCompressing(usize, String), // worker_id, filename
    FileCompressed(usize, String), // worker_id, filename
    StartWriting(u64), // total files to write
    WritingFile(String), // filename being written to final ZIP
    Complete,
}

#[derive(Clone)]
struct CompressedFileRef {
    zip_path: String,
    temp_file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let path = Path::new(&args.path);
    if !path.exists() {
        eprintln!("ERR: Given path does not exist");
        process::exit(1);
    }
    if !path.is_dir() {
        eprintln!("ERR: Path should be a directory");
        process::exit(1);
    }
    let full_path = fs::canonicalize(&path).await.unwrap_or(path.into());
    println!("(Server) worlds directory: {}", full_path.to_string_lossy());
    println!("Include Nether: {}", args.include_nether);
    println!("Include End: {}", args.include_end);
    println!("Output file name: {}.zip", args.download_file_name);
    
    let thread_count = if args.threads == 0 {
        num_cpus::get()
    } else {
        args.threads
    };
    println!("Using {} compression threads", thread_count);
    
    run_server(args, thread_count).await
}

async fn run_server(args: Args, thread_count: usize) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let zip_file_path = Path::new(&args.download_file_name).with_extension("zip");
    let base = PathBuf::from(&args.path);
    let mut paths_to_zip = vec![base.join("world")];

    if args.include_nether {
        paths_to_zip.push(base.join("world_nether"));
    }
    if args.include_end {
        paths_to_zip.push(base.join("world_the_end"));
    }

    // Generate ZIP with progress updates
    generate_zip_with_progress(paths_to_zip, zip_file_path.clone(), thread_count)
        .await
        .context("Failed to generate ZIP file")?;

    let addr = SocketAddr::from_str(&format!("{}:{}", args.host_ip, args.port))?;
    let listener = TcpListener::bind(addr).await?;
    println!("\nHosting world files at {}/{}", addr, args.host_path);

    let zip_file_path = std::sync::Arc::new(zip_file_path);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let zip_file_path = zip_file_path.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let zip_file_path = zip_file_path.clone();
                        async move { handle(req, zip_file_path.clone()).await }
                    }),
                )
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    zip_file_path: Arc<PathBuf>,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {
    match req.uri().path() {
        "/ping" => Ok(Response::new(
            Full::new(Bytes::from("Pong!"))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                .boxed()
        )),
        "/world" => serve_multiple_paths_zipped(zip_file_path.clone()).await,

        _ => {
            let mut not_found = Response::new(
                Full::new(Bytes::from("Not Found"))
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                    .boxed()
            );
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

async fn serve_multiple_paths_zipped(
    zip_file_path: Arc<PathBuf>,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {
    let file = tokio::fs::File::open(zip_file_path.as_ref()).await;
    match file {
        Ok(file) => {
            let file_size = file.metadata().await?.len();
            let reader_stream = ReaderStream::new(file);
            let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            let response = Response::builder()
                .header(CONTENT_TYPE, "application/zip")
                .header(
                    CONTENT_DISPOSITION,
                    format!(
                        "attachment; filename=\"{}\"",
                        zip_file_path.file_name().unwrap().to_string_lossy()
                    ),
                )
                .header("Content-Length", file_size.to_string())
                .status(StatusCode::OK)
                .body(boxed_body)
                .unwrap();

            Ok(response)
        }
        Err(err) => {
            eprintln!("Failed to read the ZIP file: {}", err);
            let mut resp = Response::new(
                Full::new(Bytes::from("Failed to serve ZIP"))
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                    .boxed()
            );
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            Ok(resp)
        }
    }
}

async fn generate_zip_with_progress(
    paths: Vec<PathBuf>,
    output_path: PathBuf,
    thread_count: usize,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    
    // Spawn blocking task for ZIP creation
    let zip_handle = tokio::task::spawn_blocking(move || {
        generate_zip_blocking(paths, output_path, tx, thread_count)
    });

    // Handle progress updates on main thread
    let progress_handle = tokio::task::spawn_blocking(move || {
        let multi = MultiProgress::new();
        
        let scan_bar = multi.add(ProgressBar::new_spinner());
        scan_bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .unwrap()
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
                    scan_bar.set_message(format!("Found: {}", 
                        Path::new(&name).file_name().unwrap_or_default().to_string_lossy()));
                }
                ProgressMessage::StartCompression(total) => {
                    scan_bar.finish_with_message(format!("Found {} files", total));
                    
                    // Create compression progress bar
                    let pb = multi.add(ProgressBar::new(total));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("{spinner} Compressing: [{elapsed_precise}] {wide_bar} {percent}% {pos}/{len} (ETA: {eta})")
                            .unwrap()
                    );
                    compression_bar = Some(pb);
                }
                ProgressMessage::StartCompressing(worker_id, filename) => {
                    // Ensure we have enough worker bars with bounds checking
                    while worker_bars.len() <= worker_id {
                        let bar_id = worker_bars.len();
                        let pb = multi.add(ProgressBar::new_spinner());
                        pb.set_style(
                            ProgressStyle::default_spinner()
                                .template(&format!("{{spinner}} Worker {}: {{msg}}", bar_id))
                                .unwrap()
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
                ProgressMessage::Complete => {
                    if let Some(ref pb) = write_bar {
                        pb.finish_with_message("ZIP file created successfully!");
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

fn generate_zip_blocking(
    paths: Vec<PathBuf>,
    output_path: PathBuf,
    tx: mpsc::Sender<ProgressMessage>,
    thread_count: usize,
) -> Result<()> {
    // Create temp directory for compressed files
    let temp_dir = std::env::temp_dir().join(format!("mwdh_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)
        .context("Failed to create temp directory")?;
    
    // Ensure cleanup on exit
    let temp_dir_clone = temp_dir.clone();
    let _cleanup = scopeguard::guard((), move |_| {
        let _ = std::fs::remove_dir_all(&temp_dir_clone);
    });

    // First pass: count all files
    tx.send(ProgressMessage::StartScanning).ok();
    let mut all_files = Vec::new();
    
    for path in &paths {
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid path: {}", path.display()))?
            .to_string_lossy()
            .to_string();

        let meta = std::fs::metadata(path)
            .with_context(|| format!("Failed to stat: {}", path.display()))?;

        if meta.is_file() {
            all_files.push((path.clone(), name));
            tx.send(ProgressMessage::FileFound(path.display().to_string())).ok();
        } else {
            collect_files_recursive(path, &name, &mut all_files, &tx)?;
        }
    }

    let total_files = all_files.len() as u64;
    tx.send(ProgressMessage::StartCompression(total_files)).ok();

    // Second pass: compress files in parallel using a worker pool
    let (work_tx, work_rx) = channel::unbounded::<(usize, PathBuf, String)>();
    let (result_tx, result_rx) = channel::unbounded::<Result<CompressedFileRef>>();
    
    // Spawn worker threads with explicit IDs
    let workers: Vec<_> = (0..thread_count)
        .map(|worker_id| {
            let work_rx = work_rx.clone();
            let result_tx = result_tx.clone();
            let tx = tx.clone();
            let temp_dir = temp_dir.clone();
            
            std::thread::Builder::new()
                .name(format!("worker-{}", worker_id))
                .spawn(move || {
                    while let Ok((idx, src_path, zip_path)) = work_rx.recv() {
                        tx.send(ProgressMessage::StartCompressing(worker_id, zip_path.clone())).ok();
                        
                        let result = compress_file_to_temp(&src_path, &zip_path, &temp_dir, idx);
                        
                        tx.send(ProgressMessage::FileCompressed(worker_id, zip_path.clone())).ok();
                        
                        if result_tx.send(result).is_err() {
                            break;
                        }
                    }
                })
                .unwrap()
        })
        .collect();
    
    // Send work to workers
    for (idx, (path, zip_path)) in all_files.iter().enumerate() {
        work_tx.send((idx, path.clone(), zip_path.clone())).ok();
    }
    drop(work_tx); // Signal no more work
    drop(result_tx); // Drop our copy so receiver knows when workers are done
    
    // Collect results in order
    let mut compressed_files = vec![None; all_files.len()];
    for result in result_rx {
        let compressed = result?;
        // Extract index from temp filename
        let idx_str = compressed.temp_file
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('_').last())
            .ok_or_else(|| anyhow::anyhow!("Invalid temp filename"))?;
        let idx: usize = idx_str.parse()?;
        compressed_files[idx] = Some(compressed);
    }
    
    // Wait for all workers to finish
    for worker in workers {
        worker.join().ok();
    }

    // Third pass: write all compressed data to ZIP sequentially
    tx.send(ProgressMessage::StartWriting(all_files.len() as u64)).ok();
    
    let file = std::fs::File::create(&output_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored) // Already compressed
        .large_file(true);

    for compressed_opt in compressed_files {
        let compressed = compressed_opt.ok_or_else(|| anyhow::anyhow!("Missing compressed file"))?;
        
        tx.send(ProgressMessage::WritingFile(compressed.zip_path.clone())).ok();
        
        zip.start_file(&compressed.zip_path, options)
            .with_context(|| format!("Failed to start ZIP entry: {}", compressed.zip_path))?;
        
        // Stream from temp file to ZIP
        let mut temp_file = File::open(&compressed.temp_file)
            .with_context(|| format!("Failed to open temp file: {}", compressed.temp_file.display()))?;
        
        std::io::copy(&mut temp_file, &mut zip)
            .with_context(|| format!("Failed writing to ZIP: {}", compressed.zip_path))?;
        
        // Clean up temp file immediately after writing
        std::fs::remove_file(&compressed.temp_file).ok();
    }

    zip.finish().context("Failed to finish ZIP")?;
    tx.send(ProgressMessage::Complete).ok();
    
    Ok(())
}

fn compress_file_to_temp(
    src_path: &Path,
    zip_path: &str,
    temp_dir: &Path,
    idx: usize,
) -> Result<CompressedFileRef> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    
    let temp_file_path = temp_dir.join(format!("compressed_{}.tmp", idx));
    let mut temp_file = File::create(&temp_file_path)
        .with_context(|| format!("Failed to create temp file: {}", temp_file_path.display()))?;
    
    let mut input_file = File::open(src_path)
        .with_context(|| format!("Failed to open: {}", src_path.display()))?;
    
    // Compress directly to temp file
    let mut encoder = DeflateEncoder::new(&mut temp_file, Compression::default());
    std::io::copy(&mut input_file, &mut encoder)
        .with_context(|| format!("Failed to compress: {}", src_path.display()))?;
    encoder.finish()
        .with_context(|| format!("Failed to finish compression: {}", src_path.display()))?;

    Ok(CompressedFileRef {
        zip_path: zip_path.to_string(),
        temp_file: temp_file_path,
    })
}

fn collect_files_recursive(
    base_dir: &Path,
    zip_prefix: &str,
    all_files: &mut Vec<(PathBuf, String)>,
    tx: &mpsc::Sender<ProgressMessage>,
) -> Result<()> {
    let mut stack = vec![(base_dir.to_path_buf(), zip_prefix.to_string())];

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
                all_files.push((path.clone(), child_zip_path));
                tx.send(ProgressMessage::FileFound(path.display().to_string())).ok();
            }
        }
    }

    Ok(())
}