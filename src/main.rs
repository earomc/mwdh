use anyhow::{Context, Result};
use clap::builder::{ArgPredicate, ValueParser};
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use mwdh::{CompressionFormat, zip_compression};
use std::net::SocketAddr;
use std::process;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use clap::{
    Arg, ArgAction, ArgGroup, Command, Parser, crate_authors, crate_description, crate_name,
    crate_version, value_parser,
};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::net::TcpListener;

// #[derive(Parser, Debug)]
// #[command(author, version, about)]
// pub struct Args {
//     /// Path to the (minecraft server) directory that contains /world, /world_nether and /world_the_end
//     #[arg(short = 'p', long = "path")]
//     pub path: String,

//     /// Include the Nether dimension ("world_nether")
//     #[arg(short = 'n', long = "include-nether", default_value_t = false)]
//     pub include_nether: bool,

//     /// Include the End dimension ("world_the_end")
//     #[arg(short = 'e', long = "include-end", default_value_t = false)]
//     pub include_end: bool,

//     /// Specify the download file name - Note: (mwdh will append '.zip' to it)
//     #[arg(short = 'f', long = "file-name", default_value = "world")]
//     pub download_file_name: String,

//     /// Host path from where to download the world files
//     #[arg(short = 'H', long, default_value = "world")]
//     pub host_path: String,

//     /// IP address to serve on
//     #[arg(long = "host-ip", default_value = "0.0.0.0")]
//     pub host_ip: String,

//     /// Port to serve on
//     #[arg(short = 'P', long = "port", default_value_t = 3000)]
//     pub port: u16,

//     /// Number of threads for parallel compression (0 = auto-detect)
//     #[arg(short = 't', long, default_value_t = 0)]
//     pub threads: usize,

//     /// The level of compression to apply. COMPRESSION_LEVEL should be an integer from 0-9 where 0 means "no compression" and 9 means "take as long as you'd like"
//     #[arg(short = 'l', long, value_parser=clap::value_parser!(u8).range(0..=9), default_value_t = 6)]
//     compression_level: u8,

//     #[arg(short = 'F')]
//     compression_format: CompressionFormat
// }
struct Args {
    /// Path to the (minecraft server) directory that contains /world, /world_nether and /world_the_end
    pub worlds_path: String,

    /// Include the Nether dimension ("world_nether")
    pub include_nether: bool,

    /// Include the End dimension ("world_the_end")
    pub include_end: bool,

    /// Include the Overworld
    pub include_overworld: bool,

    /// Specify the download file name - Note: (mwdh will append a file-ending to it)
    pub download_file_name: String,

    /// Host path from where to download the world files
    pub host_path: String,

    /// IP address to serve on
    pub host_ip: String,

    /// Port to serve on
    pub port: u16,

    /// Number of threads for parallel compression (0 = auto-detect)
    pub threads: usize,

    /// The level of compression to apply. COMPRESSION_LEVEL should be an integer from 0-9 where 0 means "no compression" and 9 means "take as long as you'd like"
    compression_level: u8,

    compression_format: CompressionFormat,
}

fn create_cli() -> Command {
    let cli = Command::new(crate_name!())
        .about(crate_description!())
        .author(crate_authors!())
        .version(crate_version!())
        .arg_required_else_help(true)
        .arg(Arg::new("worlds-path")
            .help("Path to the (minecraft server) directory that contains /world, /world_nether and /world_the_end")
            .short('w')
            .long("worlds-path")
            .value_parser(value_parser!(PathBuf))
            .default_value(".") // current dir
            .num_args(1) // TODO: test if num_args is needed
        )
        .arg(Arg::new("include-nether").short('n').long("include-nether").action(ArgAction::SetTrue))
        .arg(Arg::new("include-end").short('e').long("include-end").action(ArgAction::SetTrue))
        .arg(Arg::new("include-overworld").short('o').long("include-overworld").action(ArgAction::SetTrue))
        .arg(Arg::new("compression-format").default_value("zstd").short('F').long("compression-format")) // TODO: maybe put compression into one argument
        .arg(Arg::new("compression-level").short('l').long("compression-level").help("For zstd use -7 to 22, for zip use 0 to 9")
            .default_value_ifs(
                [
                    ("compression-format", ArgPredicate::Equals("zstd".into()), "-7"), // when using zstd, optimizing for speed by default
                    ("compression-format", ArgPredicate::Equals("zip".into()), "6")
                ]
            )
            .value_parser(value_parser!(i8).range(-7..=22)) // zstd compression levels go from -7 to 22
        )
        .arg(Arg::new("compression-threads").short('t').long("threads").default_value("0").help("Number of threads for parallel compression (0 = auto-detect)"))
        .group(ArgGroup::new("compressing") // group containing all the args needed to compress a world. not necessary when just serving a world file
            .args(["worlds-path", "include-nether", "include-end", "include-overworld", "compression-format", "compression-level"])
        )
        .arg(Arg::new("download-file-name").short('f').long("file-name").help("Specify the download file name - Note: (mwdh will append '.zip' or '.tar.zst' to it)"))
        .arg(Arg::new("host-path").short('H').long("host-path").default_value("world").help("Host path from where to download the world files"))
        .arg(Arg::new("host-ip").long("host-ip").default_value("0.0.0.0").help("IP address to serve the world download on"))
        .arg(Arg::new("port").short('p').long("port").value_parser(value_parser!(u16).range(1024..=65535)).help("What port to serve the world download on").default_value("3000"))
    ;
    cli
}

fn parse_args(cli: Command) -> Args {
    let matches = cli.get_matches();

    let worlds_path = matches.get_one::<String>("worlds-path");
    todo!()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = create_cli();
    let args = parse_args(cli);
    let path = Path::new(&args.worlds_path);
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
    println!("Compression level {}", args.compression_level);

    run_server(args, thread_count).await
}

async fn run_server(
    args: Args,
    thread_count: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = PathBuf::from(&args.worlds_path);

    let mut paths_to_be_archived = Vec::with_capacity(3);
    if args.include_overworld {
        paths_to_be_archived.push(base.join("world"));
    }
    if args.include_nether {
        paths_to_be_archived.push(base.join("world_nether"));
    }
    if args.include_end {
        paths_to_be_archived.push(base.join("world_the_end"));
    }

    let archive_output_path = Path::new(&args.download_file_name);
    match args.compression_format {
        CompressionFormat::ZipDeflate => {
            let archive_output_path = archive_output_path.with_extension("zip");
            zip_compression::generate_zip_with_progress(
                paths_to_be_archived,
                archive_output_path.into(),
                thread_count,
                args.compression_level as u32,
            )
            .await
            .context("Failed to generate ZIP file")?;
        }
        CompressionFormat::TarZstd => {
            let _archive_output_path = archive_output_path.with_extension("tar.zst");
            todo!("not yet implemented")
        }
    }

    let addr = SocketAddr::from_str(&format!("{}:{}", args.host_ip, args.port))?;
    let listener = TcpListener::bind(addr).await?;
    println!("\nHosting world files at {}/{}", addr, args.host_path);

    let archive_output_path: Arc<PathBuf> = std::sync::Arc::new(archive_output_path.into());
    let host_path = Arc::new(args.host_path);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        let host_path = host_path.clone();
        let archive_output_path = archive_output_path.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let host_path = host_path.clone();
                        let archive_output_path = archive_output_path.clone();
                        async move {
                            handle(
                                req,
                                &host_path.clone(),
                                archive_output_path,
                                args.compression_format,
                            )
                            .await
                        }
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
    serve_on_path: &str,
    path_to_archive: Arc<PathBuf>,
    format: CompressionFormat,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {
    let path = req.uri().path();
    match path {
        "/ping" => Ok(Response::new(
            Full::new(Bytes::from("Pong!"))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                .boxed(),
        )),
        _ => {
            if &path[1..] == serve_on_path {
                return get_archive_file_as_response(path_to_archive.clone(), format).await;
            }
            let mut not_found = Response::new(
                Full::new(Bytes::from("Not Found"))
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                    .boxed(),
            );
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

async fn get_archive_file_as_response(
    path_to_archive: Arc<PathBuf>,
    format: CompressionFormat,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {
    let file = tokio::fs::File::open(path_to_archive.as_ref()).await;
    match file {
        Ok(file) => {
            let file_size = file.metadata().await?.len();
            let reader_stream = ReaderStream::new(file);
            let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            let content_type = format.get_mime_type();
            let response = Response::builder()
                .header(CONTENT_TYPE, content_type)
                .header(
                    CONTENT_DISPOSITION,
                    format!(
                        "attachment; filename=\"{}\"",
                        path_to_archive
                            .file_name()
                            .expect("Should be a file path") // expect/unwrap here is okay, because the path should always end with .zip, pointing to an actual file
                            .to_string_lossy()
                    ),
                )
                .header("Content-Length", file_size.to_string())
                .status(StatusCode::OK)
                .body(boxed_body)
                .unwrap();

            Ok(response)
        }
        Err(err) => {
            eprintln!("Failed to read the archive file: {}", err);
            let mut resp = Response::new(
                Full::new(Bytes::from("Failed to serve archive file"))
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "infallible"))
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            Ok(resp)
        }
    }
}
