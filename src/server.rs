use crate::{Args, CompressionFormat, compression, paths_to_be_archived};
use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use std::net::SocketAddr;
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
use tokio::net::TcpListener;

pub async fn run_server(args: Args) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let archive_output_path = Path::new(&args.download_file_name);
    let paths_to_be_archived = paths_to_be_archived(&args);
    match args.compression_format {
        CompressionFormat::ZipDeflate => {
            let archive_output_path = archive_output_path.with_extension("zip");
            compression::zip::generate_zip_with_progress(
                paths_to_be_archived,
                archive_output_path.into(),
                args.clone(),
            )
            .await
            .context("Failed to generate ZIP file")?;
        }
        CompressionFormat::TarZstd => {
            let archive_output_path = archive_output_path.with_extension("tar.zst");
            compression::zstd::generate_zstd_with_progress(
                paths_to_be_archived,
                archive_output_path,
                args.clone(),
            )
            .await
            .context("Failed to generate tar.zst file")?;
        }
    }

    let addr = SocketAddr::from_str(&format!("{}:{}", args.bind, args.port))?;
    let listener = TcpListener::bind(addr).await?;
    println!("\nHosting world files at {}/{}", addr, args.host_path);

    let archive_output_path: Arc<PathBuf> = std::sync::Arc::new(archive_output_path.into());
    let host_path = Arc::new(args.host_path);
    let compression_format = args.compression_format;
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
                                compression_format,
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
