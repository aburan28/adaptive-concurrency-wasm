mod config;
mod gradient2;
mod host_state;
mod http_context;
mod root_context;
mod stats;

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub fn _start() {
    use proxy_wasm::traits::RootContext;
    use proxy_wasm::types::LogLevel;

    proxy_wasm::set_log_level(LogLevel::Info);
    proxy_wasm::set_root_context(|_| -> Box<dyn RootContext> {
        Box::new(root_context::AdaptiveConcurrencyRoot::new())
    });
}
