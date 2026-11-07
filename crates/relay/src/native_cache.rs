//! Process-wide rollback switch for native cache continuity.
use crate::error::{Error, Result};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub threads: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            threads: 64,
            bytes: 128 * 1024 * 1024,
        }
    }
}
fn parse(threads: Option<&str>, mib: Option<&str>) -> Result<Limits> {
    fn number(
        value: Option<&str>,
        default: usize,
        min: usize,
        max: usize,
        name: &str,
    ) -> Result<usize> {
        match value {
            None => Ok(default),
            Some(value) => value
                .parse::<usize>()
                .ok()
                .filter(|v| (min..=max).contains(v))
                .ok_or_else(|| {
                    Error::new(
                        "NATIVE_CACHE_CONFIG",
                        &format!("{name} 必须是 {min}–{max} 的整数。"),
                    )
                }),
        }
    }
    Ok(Limits {
        threads: number(threads, 64, 1, 256, "BRIDGE_NATIVE_MAX_THREADS")?,
        bytes: number(mib, 128, 32, 1024, "BRIDGE_NATIVE_CONTEXT_BUDGET_MIB")? * 1024 * 1024,
    })
}
pub(crate) fn limits() -> Result<Limits> {
    static LIMITS: OnceLock<Result<Limits>> = OnceLock::new();
    LIMITS
        .get_or_init(|| {
            if enabled() {
                parse(
                    std::env::var("BRIDGE_NATIVE_MAX_THREADS").ok().as_deref(),
                    std::env::var("BRIDGE_NATIVE_CONTEXT_BUDGET_MIB")
                        .ok()
                        .as_deref(),
                )
            } else {
                Ok(Limits {
                    threads: 32,
                    bytes: 64 * 1024 * 1024,
                })
            }
        })
        .clone()
}

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BRIDGE_NATIVE_CACHE_REUSE").as_deref() != Ok("0"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_bounds_and_defaults() {
        let defaults = parse(None, None).unwrap();
        assert_eq!((defaults.threads, defaults.bytes), (64, 128 * 1024 * 1024));
        for (threads, mib) in [
            ("0", "128"),
            ("257", "128"),
            ("64", "31"),
            ("64", "1025"),
            ("1.5", "64"),
            ("", "64"),
        ] {
            assert!(parse(Some(threads), Some(mib)).is_err());
        }
        assert!(parse(Some("1"), Some("32")).is_ok());
        assert!(parse(Some("256"), Some("1024")).is_ok());
    }
}
