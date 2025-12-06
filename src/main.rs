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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tokio::fs::{self};
use tokio::net::TcpListener;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::mpsc;

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
}

#[derive(Debug)]
enum ProgressMessage {
    StartScanning,
    FileFound(String),
    StartZipping(u64), // total files
    FileProcessed(String, u64), // filename, current count
    Complete,
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
    run_server(args).await
}

async fn run_server(args: Args) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    generate_zip_with_progress(paths_to_zip, zip_file_path.clone())
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

async fn generate_zip_with_progress(paths: Vec<PathBuf>, output_path: PathBuf) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    
    // Spawn blocking task for ZIP creation
    let zip_handle = tokio::task::spawn_blocking(move || {
        generate_zip_blocking(paths, output_path, tx)
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
        
        let mut zip_bar: Option<ProgressBar> = None;
        
        while let Ok(msg) = rx.recv() {
            match msg {
                ProgressMessage::StartScanning => {
                    scan_bar.set_message("Scanning directories...");
                }
                ProgressMessage::FileFound(name) => {
                    scan_bar.set_message(format!("Found: {}", name));
                }
                ProgressMessage::StartZipping(total) => {
                    scan_bar.finish_with_message(format!("Found {} files", total));
                    
                    let pb = multi.add(ProgressBar::new(total));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("{spinner} [{elapsed_precise}] {wide_bar} {percent}% {pos}/{len} (ETA: {eta}) {msg}")
                            .unwrap()
                    );
                    zip_bar = Some(pb);
                }
                ProgressMessage::FileProcessed(name, current) => {
                    if let Some(ref pb) = zip_bar {
                        pb.set_position(current);
                        pb.set_message(format!("Compressing: {}", 
                            Path::new(&name)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ));
                    }
                }
                ProgressMessage::Complete => {
                    if let Some(pb) = zip_bar {
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
) -> Result<()> {
    let file = std::fs::File::create(&output_path)?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

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
    tx.send(ProgressMessage::StartZipping(total_files)).ok();

    // Second pass: actually zip files
    for (idx, (path, zip_path)) in all_files.iter().enumerate() {
        add_file_to_zip(&mut zip, path, zip_path, options)?;
        tx.send(ProgressMessage::FileProcessed(
            zip_path.clone(),
            (idx + 1) as u64
        )).ok();
    }

    zip.finish().context("Failed to finish ZIP")?;
    tx.send(ProgressMessage::Complete).ok();
    
    Ok(())
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

fn add_file_to_zip(
    zip: &mut ZipWriter<File>,
    src_path: &Path,
    zip_path: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    let mut file = File::open(src_path)
        .with_context(|| format!("Failed to open: {}", src_path.display()))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| format!("Failed to read: {}", src_path.display()))?;

    zip.start_file(zip_path, options)
        .with_context(|| format!("Failed to start ZIP entry: {zip_path}"))?;

    zip.write_all(&buf)
        .with_context(|| format!("Failed writing to ZIP: {zip_path}"))?;

    Ok(())
}