fn main() {
    #[cfg(target_arch = "wasm32")]
    cocat_web::app::start();
}
