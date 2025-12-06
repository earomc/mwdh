use std::convert::Infallible;
use std::net::SocketAddr;
use std::process;

use anyhow::{Context, Result};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::fs::{self};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use zip::write::SimpleFileOptions;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// Path to the minecraft server directory that contains /world, /world_nether and /world_the_end
    #[arg(short = 'p', long = "path")]
    pub path: String,

    /// Include the Nether dimension ("world_nether")
    /// Short flag: -n   Combined: -ne
    #[arg(short = 'n', long = "include-nether", default_value_t = false)]
    pub include_nether: bool,

    /// Include the End dimension ("world_the_end")
    /// Short flag: -e   Combined: -ne
    #[arg(short = 'e', long = "include-end", default_value_t = false)]
    pub include_end: bool,

    /// Specify the output file name - Note: (mwdh will append '.zip' to it)
    #[arg(short = 'o', long = "output-file", default_value = "world")]
    pub output_file_name: String,

    /// Port to serve on
    #[arg(short = 'P', long = "port", default_value_t = 3000)]
    pub port: u16,
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    args: std::sync::Arc<Args>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/ping" => Ok(Response::new(Full::new(Bytes::from("Pong!")))),
        "/world" => {
            let base = PathBuf::from(&args.path);
            let mut paths_to_zip = vec![base.join("world")];

            // Add optional dimension folders

            if args.include_nether {
                paths_to_zip.push(base.join("world_nether"));
            }

            if args.include_end {
                paths_to_zip.push(base.join("world_the_end"));
            }
            serve_multiple_paths_zipped(paths_to_zip, &args.output_file_name).await
        }

        _ => {
            let mut not_found = Response::new(Full::new(Bytes::from("Not Found")));
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

async fn serve_multiple_paths_zipped(
    paths: Vec<PathBuf>,
    file_name: &str,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match try_serve_multiple_paths_zipped(paths, file_name).await {
        Ok(resp) => Ok(resp),
        Err(err) => {
            eprintln!("ZIP generation error: {err:#}");
            let mut resp = Response::new(Full::new(Bytes::from("Failed to generate ZIP")));
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            Ok(resp)
        }
    }
}

async fn try_serve_multiple_paths_zipped(
    paths: Vec<PathBuf>,
    file_name: &str,
) -> Result<Response<Full<Bytes>>> {

    let mut zip_buf = Vec::new();
    let cursor = std::io::Cursor::new(&mut zip_buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    for path in paths {
        let name = path.file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid path (no filename): {}", path.display()))?
            .to_string_lossy();

        let meta = fs::metadata(&path)
            .await
            .with_context(|| format!("Failed to stat path: {}", path.display()))?;

        if meta.is_file() {
            add_single_file(&mut zip, &path, &name, options).await?;
        } else {
            add_directory_iterative(&mut zip, &path, &name, options).await?;
        }
    }

    zip.finish().context("Failed to finish ZIP")?;

    let mut resp = Response::new(Full::new(Bytes::from(zip_buf)));
    resp.headers_mut()
        .insert(CONTENT_TYPE, "application/zip".parse().unwrap());
    resp.headers_mut().insert(
        CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}.zip\"", file_name)
            .parse()
            .unwrap(),
    );

    Ok(resp)
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
    let full_path = fs::canonicalize(&path).await.unwrap_or(path.into()); // if canonicalization fails, use the relative path
    println!("Server directory: {}", full_path.to_string_lossy());
    println!("Include Nether: {}", args.include_nether);
    println!("Include End: {}", args.include_end);
    println!("Output file name: {}.zip", args.output_file_name);
    run_server(args).await
}

async fn run_server(args: Args) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = TcpListener::bind(addr).await?;

    // Clone the args for the handler closure
    let shared_args = std::sync::Arc::new(args);
    println!("Hosting world files at {}/world", addr);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let args = shared_args.clone();

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let args = args.clone();
                        async move { handle(req, args.clone()).await }
                    }),
                )
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}

async fn add_single_file(
    zip: &mut zip::ZipWriter<std::io::Cursor<&mut Vec<u8>>>,
    src_path: &Path,
    zip_path: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    let mut file = File::open(src_path)
        .await
        .with_context(|| format!("Failed to open file: {}", src_path.display()))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .await
        .with_context(|| format!("Failed to read file: {}", src_path.display()))?;

    zip.start_file(zip_path, options)
        .with_context(|| format!("Failed to start ZIP file entry: {zip_path}"))?;

    zip.write_all(&buf)
        .with_context(|| format!("Failed writing file into ZIP: {zip_path}"))?;

    Ok(())
}

async fn add_directory_iterative(
    zip: &mut zip::ZipWriter<std::io::Cursor<&mut Vec<u8>>>,
    base_dir: &Path,
    zip_prefix: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    // Stack of (filesystem path, path inside zip)
    let mut stack = vec![(base_dir.to_path_buf(), zip_prefix.to_string())];

    while let Some((curr_fs_path, curr_zip_path)) = stack.pop() {
        let mut read_dir = fs::read_dir(&curr_fs_path)
            .await
            .with_context(|| format!("Failed to read directory: {}", curr_fs_path.display()))?;

        zip.add_directory(&curr_zip_path, options)
            .with_context(|| format!("Failed to add directory to ZIP: {curr_zip_path}"))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .context("Failed to iterate directory")?
        {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let child_zip_path = format!("{}/{}", curr_zip_path, name);

            let meta = entry
                .metadata()
                .await
                .with_context(|| format!("Failed to read metadata: {}", path.display()))?;

            if meta.is_dir() {
                stack.push((path, child_zip_path));
            } else if meta.is_file() {
                add_single_file(zip, &path, &child_zip_path, options).await?;
            }
        }
    }

    Ok(())
}
