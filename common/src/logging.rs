use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    fmt,
};

pub fn init_logging() {
    let env_filter =
        EnvFilter::builder().with_default_directive(LevelFilter::INFO.into()).from_env_lossy();

    fmt().with_target(true).with_level(true).with_env_filter(env_filter).init();
}
