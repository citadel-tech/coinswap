pub mod maker;
pub mod market;
pub mod recovery;
pub mod taker;
pub mod wallet;
pub mod watchtower;

use std::sync::Once;

static INIT: Once = Once::new();

/// Setup function that will only run once, even if called multiple times.
pub fn setup_logger() {
    INIT.call_once(|| {
        env_logger::Builder::from_env(
            env_logger::Env::default()
                .default_filter_or("teleport=info,main=info,wallet=info")
                .default_write_style_or("always"),
        )
        .init();
    });
}
