use std::convert::Infallible;
use std::net::SocketAddr;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use std::io::Write;

async fn handle(req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/hello" => Ok(Response::new(Full::new(Bytes::from("Hello, World!")))),

        "/file" => serve_file_zipped("test-server/world").await, // change to your file path

        _ => {
            let mut not_found = Response::new(Full::new(Bytes::from("Not Found")));
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}


use zip::write::{SimpleFileOptions};

use tokio::fs::{self};

use std::path::{Path, PathBuf};

async fn serve_file_zipped(path: &str) -> Result<Response<Full<Bytes>>, Infallible> {
    let metadata = match fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => {
            let mut resp = Response::new(Full::new(Bytes::from("File or directory not found")));
            *resp.status_mut() = StatusCode::NOT_FOUND;
            return Ok(resp);
        }
    };

    let mut zip_buf = Vec::new();
    let cursor = std::io::Cursor::new(&mut zip_buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    let base = PathBuf::from(path);
    let base_name = base.file_name().unwrap_or_default().to_string_lossy().to_string();

    if metadata.is_file() {
        add_single_file(&mut zip, &base, &base_name, options).await?;
    } else {
        add_directory_iterative(&mut zip, &base, &base_name, options).await?;
    }

    zip.finish().expect("zip finish failed");

    let zip_name = format!("{}.zip", base_name);
    let mut resp = Response::new(Full::new(Bytes::from(zip_buf)));

    resp.headers_mut().insert(
        CONTENT_TYPE,
        "application/zip".parse().unwrap(),
    );

    resp.headers_mut().insert(
        CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", zip_name).parse().unwrap(),
    );

    Ok(resp)
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _socket_addr) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(handle))
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
            Err(_) => continue,
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
