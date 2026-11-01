//! Process entry point for the IPTV media server scaffold.

fn main() {
    if let Err(error) = iptv_media_server::load_config() {
        let exit_code = error.exit_code();
        if exit_code == 0 {
            println!("{error}");
        } else {
            eprintln!("error: {error}");
        }
        std::process::exit(exit_code);
    }

    eprintln!("media server startup is not implemented in this scaffold");
}
