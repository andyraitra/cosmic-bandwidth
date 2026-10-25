// SPDX-License-Identifier: GPL-3.0-only

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    cosmic_bandwidth::app::run()
}
