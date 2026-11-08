//! Process entry point for the IPTV media server.

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = match iptv_media_server::load_config() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "configuration failed");
            std::process::exit(1);
        }
    };
    if let Err(error) = iptv_media_server::run(config).await {
        tracing::error!(%error, "server failed");
        std::process::exit(1);
    }
}
