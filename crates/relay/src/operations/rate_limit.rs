//! Account-local recovery after a native ChatGPT rate limit. API accounts are excluded.
use super::{AccountPolicy, Inner, Operations, runtime};
use crate::provider::Provider;
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

pub(super) struct Cooldown {
    generation: String,
    pub failures: u32,
    pub until: Instant,
    probe: Option<Uuid>,
}

pub(super) fn current<'a>(inner: &'a Inner, account: &AccountPolicy) -> Option<&'a Cooldown> {
    let provider = account.route.as_ref().filter(|p| p.is_chatgpt_account())?;
    inner
        .runtime
        .rate_limits
        .get(&account.id)
        .filter(|state| state.generation == runtime::fingerprint(provider))
}

pub(super) fn available(inner: &Inner, account: &AccountPolicy) -> bool {
    current(inner, account).is_none_or(|state| {
        state.until <= Instant::now()
            && state.probe.is_none()
            && Operations::active_count(inner, Some(account.id), None) == 0
    })
}

// Called under the same admission lock as selection and capacity reservation.
pub(super) fn claim(inner: &mut Inner, account: Option<Uuid>, request: Uuid) {
    let Some(account) = account else { return };
    let eligible = inner
        .settings
        .accounts
        .iter()
        .find(|a| a.id == account)
        .is_some_and(|a| current(inner, a).is_some() && available(inner, a));
    if eligible {
        inner.runtime.rate_limits.get_mut(&account).unwrap().probe = Some(request);
    }
}

pub(super) fn finished(
    inner: &mut Inner,
    account: Option<Uuid>,
    provider: Option<&Provider>,
    request: Uuid,
    success: bool,
    code: Option<&str>,
) {
    let (Some(account), Some(provider)) = (account, provider) else {
        return;
    };
    if !provider.is_chatgpt_account()
        || !inner
            .settings
            .accounts
            .iter()
            .any(|a| a.id == account && a.route.as_ref() == Some(provider))
    {
        return;
    }
    let generation = runtime::fingerprint(provider);
    if code == Some("UPSTREAM_RATE_LIMITED") {
        let now = Instant::now();
        let state = inner
            .runtime
            .rate_limits
            .entry(account)
            .or_insert_with(|| Cooldown {
                generation: generation.clone(),
                failures: 0,
                until: now,
                probe: None,
            });
        if state.generation != generation {
            *state = Cooldown {
                generation,
                failures: 0,
                until: now,
                probe: None,
            };
        }
        // A burst of sibling 429s counts as one cooldown, not one exponent per caller.
        if state.until <= now || state.probe == Some(request) {
            state.failures = state.failures.saturating_add(1);
        }
        let seconds = (30u64 << state.failures.saturating_sub(1).min(4)).min(300);
        state.until = state.until.max(now + Duration::from_secs(seconds));
        state.probe = None;
    } else if let Some(state) = inner
        .runtime
        .rate_limits
        .get_mut(&account)
        .filter(|state| state.generation == generation && state.probe == Some(request))
    {
        if success {
            inner.runtime.rate_limits.remove(&account);
        } else {
            // Cancellation, drop and unrelated errors must release the sole probe.
            state.probe = None;
        }
    }
}

#[cfg(test)]
mod tests;
