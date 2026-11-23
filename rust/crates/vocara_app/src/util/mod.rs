//! UI utilities: the command and busy-gate latches view models run their
//! async work under (ports of `utils/command.dart` and
//! `utils/busy_gate.dart`), and the blocking-thread runner every screen uses
//! to turn a synchronous service call into an iced `Task`.

pub mod busy_gate;
pub mod command;

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Why an action run under a [`command::Command`] or a
/// [`busy_gate::BusyGate`] did not succeed.
///
/// Dart's actions returned an error result or threw; the latches turned a
/// throw into an observable error too (`Result.error(Exception('$e'))`,
/// `errorMessage = e.toString()`). In Rust the counterpart of a throw is a
/// panic, which is caught and kept as [`ActionError::Panicked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError<E> {
    /// The action returned an error.
    Failed(E),
    /// The action panicked; the panic's message.
    Panicked(String),
}

impl<E: fmt::Display> fmt::Display for ActionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::Failed(e) => write!(f, "{e}"),
            ActionError::Panicked(message) => write!(f, "{message}"),
        }
    }
}

/// Runs `action` on the current thread, turning a panic into
/// [`ActionError::Panicked`].
pub fn run_caught<T, E>(action: impl FnOnce() -> Result<T, E>) -> Result<T, ActionError<E>> {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(ActionError::Failed(e)),
        Err(payload) => Err(ActionError::Panicked(panic_message(payload.as_ref()))),
    }
}

/// The message a panic carried (`panic!("...")`'s text), or a placeholder.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked".to_string()
    }
}

/// Dart's `double.toStringAsFixed(digits)`: `digits` decimals, an exact tie
/// rounded away from zero (Rust's `{:.N}` rounds a tie to even), `NaN` /
/// `Infinity` / `-Infinity` spelled as Dart spells them.
pub fn to_string_as_fixed(x: f64, digits: usize) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let rounded = format!("{x:.digits$}");
    // A tie is a value whose exact decimal expansion stops at digit
    // `digits + 1` with a 5: only then can Rust's ties-to-even differ.
    let probe = format!("{:.*}", digits + 1, x.abs());
    if !probe.ends_with('5') {
        return rounded;
    }
    let exact = format!("{:.1100}", x.abs());
    let tail_start = exact.find('.').map_or(exact.len(), |dot| dot + 1) + digits + 1;
    if exact[tail_start..].bytes().any(|b| b != b'0') {
        return rounded;
    }
    // Round the magnitude half up (the probe without its final 5, plus one
    // unit in the last place kept), then put the sign back.
    let step = 10f64.powi(-(digits as i32));
    let truncated: f64 = probe[..probe.len() - 1].parse().unwrap_or(0.0);
    let away = format!("{:.*}", digits, truncated + step);
    if x < 0.0 {
        format!("-{away}")
    } else {
        away
    }
}

/// Runs `work` on tokio's blocking pool (the app's executor is tokio): the
/// way a screen runs a synchronous store or service call off the UI thread,
/// as `PORTING.md` prescribes. A panic in `work` comes back as `Err` with its
/// message instead of tearing the app down.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    match tokio::task::spawn_blocking(move || catch_unwind(AssertUnwindSafe(work))).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(payload)) => Err(panic_message(payload.as_ref())),
        Err(join) => Err(join.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::to_string_as_fixed;

    #[test]
    fn to_string_as_fixed_rounds_like_dart() {
        assert_eq!(to_string_as_fixed(0.25, 1), "0.3");
        assert_eq!(to_string_as_fixed(-0.25, 1), "-0.3");
        assert_eq!(to_string_as_fixed(2.5, 0), "3");
        assert_eq!(to_string_as_fixed(1.45, 1), "1.4"); // 1.4499999…
        assert_eq!(to_string_as_fixed(-13.979400086720377, 1), "-14.0");
        assert_eq!(to_string_as_fixed(-79.17, 1), "-79.2");
        assert_eq!(to_string_as_fixed(-0.04, 1), "-0.0");
        assert_eq!(to_string_as_fixed(f64::NAN, 1), "NaN");
        assert_eq!(to_string_as_fixed(f64::NEG_INFINITY, 1), "-Infinity");
    }
}
