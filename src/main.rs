use std::convert::Infallible;
use std::net::SocketAddr;
use std::process;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::io::Write;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use zip::write::SimpleFileOptions;
use tokio::fs::{self};
use std::path::{Path, PathBuf};

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
    let mut zip_buf = Vec::new();
    let cursor = std::io::Cursor::new(&mut zip_buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    for path in paths {
        println!("Adding {}", path.to_string_lossy());
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Ok(meta) = fs::metadata(&path).await {
            if meta.is_file() {
                add_single_file(&mut zip, &path, &name, options).await?;
            } else {
                add_directory_iterative(&mut zip, &path, &name, options).await?;
            }
        }
    }
    zip.finish().unwrap();
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
) -> Result<(), Infallible> {
    // TODO: Improve error handling
    let mut file = match File::open(src_path).await {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };

    let mut buf = Vec::new();
    if let Err(e) = file.read_to_end(&mut buf).await {
        eprintln!("Error reading file {src_path:?}: {e}");
        return Ok(());
    }

    zip.start_file(zip_path, options).unwrap();
    zip.write_all(&buf).unwrap();

    Ok(())
}

async fn add_directory_iterative(
    zip: &mut zip::ZipWriter<std::io::Cursor<&mut Vec<u8>>>,
    base_dir: &Path,
    zip_prefix: &str,
    options: SimpleFileOptions,
) -> Result<(), Infallible> {
    // Stack of (filesystem path, path inside zip)
    let mut stack = vec![(base_dir.to_path_buf(), zip_prefix.to_string())];

    while let Some((curr_fs_path, curr_zip_path)) = stack.pop() {
        let mut read_dir = match fs::read_dir(&curr_fs_path).await {
            Ok(rd) => rd,
            Err(err) => {
                eprintln!("ERR: {}, skipping dir", err);
                continue;
            }
        };

        // Add the directory itself to the ZIP (optional)
        zip.add_directory(&curr_zip_path, options).unwrap();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let child_zip_path = format!("{}/{}", curr_zip_path, name);

            match entry.metadata().await {
                Ok(meta) if meta.is_dir() => {
                    // Push directory into stack
                    stack.push((path, child_zip_path));
                }

                Ok(meta) if meta.is_file() => {
                    add_single_file(zip, &path, &child_zip_path, options).await?;
                }

                _ => {}
            }
        }
    }

    Ok(())
}
