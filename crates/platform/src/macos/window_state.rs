//! AX state is optional enrichment, never a prerequisite for window discovery.

use nexus_cua_protocol::WindowState;
use nexus_cua_runtime::DriverError;
use std::future::Future;
use std::time::Duration;

pub(super) async fn enhance<T, F>(
    windows: Vec<T>,
    trusted: bool,
    visible: impl Fn(&T) -> bool,
    mut query: impl FnMut(&T) -> F,
) -> Vec<(T, WindowState)>
where
    F: Future<Output = Result<bool, DriverError>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(750);
    let mut output = Vec::with_capacity(windows.len());
    for (index, window) in windows.into_iter().enumerate() {
        let mut state = if visible(&window) {
            WindowState::Unknown
        } else {
            WindowState::Offscreen
        };
        if trusted
            && index < 16
            && tokio::time::Instant::now() < deadline
            && let Ok(Ok(minimized)) = tokio::time::timeout_at(deadline, query(&window)).await
        {
            state = if minimized {
                WindowState::Minimized
            } else if visible(&window) {
                WindowState::Normal
            } else {
                WindowState::Offscreen
            };
        }
        output.push((window, state));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cua_runtime::DriverErrorKind;
    use std::cell::Cell;

    #[tokio::test]
    async fn no_permission_never_queries_ax_or_drops_windows() {
        let result = enhance(
            vec![true, false],
            false,
            |visible| *visible,
            |_| async { panic!("AX must not be called without permission") },
        )
        .await;
        assert_eq!(
            result,
            vec![
                (true, WindowState::Unknown),
                (false, WindowState::Offscreen)
            ]
        );
    }

    #[tokio::test]
    async fn eighty_windows_survive_failed_and_bounded_enrichment() {
        let calls = Cell::new(0);
        let result = enhance(
            (0..80).collect(),
            true,
            |_| true,
            |_| {
                calls.set(calls.get() + 1);
                async {
                    Err(DriverError::new(
                        DriverErrorKind::TargetUnavailable,
                        "identity mismatch",
                    ))
                }
            },
        )
        .await;
        assert_eq!(result.len(), 80);
        assert_eq!(calls.get(), 16);
        assert!(
            result
                .iter()
                .all(|(_, state)| *state == WindowState::Unknown)
        );
    }

    #[tokio::test]
    async fn stalled_provider_exhausts_shared_budget_without_losing_windows() {
        let calls = Cell::new(0);
        let result = enhance(
            vec![0; 80],
            true,
            |_| true,
            |_| {
                calls.set(calls.get() + 1);
                std::future::pending::<Result<bool, DriverError>>()
            },
        )
        .await;
        assert_eq!(result.len(), 80);
        assert_eq!(calls.get(), 1);
    }
}
