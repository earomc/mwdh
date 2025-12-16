use anyhow::Result;
use mwdh::cli::{self};
use mwdh::server;
use std::path::Path;
use std::process;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = cli::create_cli();
    let mut args = cli::parse_args(cli)?;
    let path = Path::new(&args.world_path);
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
    if args.include_overworld {
        inclusions.push_str("Overworld");
        i += 1;
    }
    if args.include_nether {
        if i > 0 {
            inclusions.push_str(", ");
        }
        inclusions.push_str("Nether");
        i += 1;
    }
    if args.include_end {
        if i > 0 && i < 3 {
            inclusions.push_str(", ");
        }
        inclusions.push_str("The End");
    }
    println!("{}", inclusions);

    if args.server_threads == 0 {
        args.server_threads = num_cpus::get();
    }
    if args.compression_threads == 0 {
        args.compression_threads = num_cpus::get();
    }
    println!(
        "Compressing to \"{}\" using {} at level {} with {} threads",
        args.download_file_name,
        args.compression_format,
        args.compression_level,
        args.compression_threads
    );
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("mwdh")
        .worker_threads(args.server_threads)
        .enable_all()
        .build()
        .unwrap()
        .block_on(server::run_server(args))
}
