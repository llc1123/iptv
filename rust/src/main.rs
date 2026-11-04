//! Process entry point for the IPTV media server.

#[tokio::main]
async fn main() {
    let config = match iptv_media_server::load_config() {
        Ok(config) => config,
        Err(error) => {
            let exit_code = error.exit_code();
            if exit_code == 0 {
                println!("{error}");
            } else {
                eprintln!("error: {error}");
            }
            std::process::exit(exit_code);
        }
    };
    if let Err(error) = iptv_media_server::run(config).await {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code());
    }
}
