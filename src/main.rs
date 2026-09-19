mod canvas;
mod geom;
mod text;
mod wayland;

fn main() {
    if let Err(e) = wayland::run() {
        eprintln!("cornice: {e}");
        std::process::exit(1);
    }
}
