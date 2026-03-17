mod check_tcp;
mod checker;
mod counter;
mod health;

pub use health::UpstreamHealth;

pub(crate) use checker::spawn_health_checkers;
