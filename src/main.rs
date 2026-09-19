mod canvas;
mod config;
mod geom;
mod text;
mod theme;
mod wayland;

fn main() {
    let path = config::default_path();
    let cfg = match config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cornice: config error: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = wayland::run(cfg) {
        eprintln!("cornice: {e}");
        std::process::exit(1);
    }
}
