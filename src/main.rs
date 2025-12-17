use anyhow::{Result};
use mwdh::cli::{self};
use mwdh::{MwdhOptions, archive, server};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = cli::create_cli();
    let options = cli::parse_args(cli)?;

    let threads = match options {
        MwdhOptions::Server(ref server_options) => server_options.threads,
        MwdhOptions::Archive(ref archive_options) => archive_options.threads,
        MwdhOptions::Both { ref server, archive: _ } => server.threads,
    };

    tokio::runtime::Builder::new_multi_thread()
        .thread_name("mwdh")
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_mwdh(options))
}

async fn run_mwdh(options: MwdhOptions) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match options {
        MwdhOptions::Server(server_options) => server::run_server(server_options).await?,
        MwdhOptions::Archive(archive_options) => archive::do_compression(archive_options).await?,
        MwdhOptions::Both { server, archive } => {
            archive::do_compression(archive).await?;
            server::run_server(server).await?
        },
    }
    Ok(())
}
