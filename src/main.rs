mod anim;
mod bar;
mod canvas;
mod cli;
mod config;
mod geom;
mod notify;
mod text;
mod theme;
mod wayland;
mod widget;

use std::ffi::{CString, c_void};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Linux `SIGUSR1`. A debug key binding sends it (`pkill -USR1 -x cornice`) and the process replaces
/// itself with a fresh image, so config is re-read and no PID/supervisor changes.
const SIGUSR1: c_int = 10;
static RESTART_EXE: AtomicPtr<c_char> = AtomicPtr::new(ptr::null_mut());
static RESTART_ARGV: AtomicPtr<*const c_char> = AtomicPtr::new(ptr::null_mut());

// Raw libc declarations keep the dependency set at nine crates. `execv` and `_exit` are
// async-signal-safe, and the paths below are prepared before the handler is installed.
unsafe extern "C" {
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> *mut c_void;
    fn execv(path: *const c_char, argv: *const *const c_char) -> c_int;
    fn _exit(status: c_int) -> !;
}

extern "C" fn restart_handler(_signal: c_int) {
    let exe = RESTART_EXE.load(Ordering::Acquire);
    let argv = RESTART_ARGV.load(Ordering::Acquire);
    if !exe.is_null() && !argv.is_null() {
        unsafe { execv(exe, argv) };
    }
    unsafe { _exit(1) }
}

/// The daemon path is entered without arguments, so the restart keeps `argv[0]` and the current environment (`execv`).
fn install_restart_handler() {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/proc/self/exe"));
    let exe = CString::new(exe.as_os_str().as_bytes()).expect("the executable path cannot contain NUL").into_raw();
    let argv: Box<[*const c_char]> = vec![exe, ptr::null()].into_boxed_slice();
    let argv = Box::leak(argv);
    RESTART_EXE.store(exe, Ordering::Release);
    RESTART_ARGV.store(argv.as_ptr() as *mut *const c_char, Ordering::Release);
    unsafe { signal(SIGUSR1, restart_handler) };
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() { std::process::exit(cli::run(&args)); }
    run_daemon();
}

fn run_daemon() {
    install_restart_handler();
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
