//! Compositor capability probing (no guessing from env vars where a real
//! answer is available).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Names of all Wayland globals the compositor advertises.
pub fn wayland_globals() -> Result<HashSet<String>> {
    let conn = Connection::connect_to_env()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn)?;
    Ok(globals
        .contents()
        .with_list(|list| list.iter().map(|g| g.interface.clone()).collect()))
}

#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub layer_shell: bool,
    pub data_control: bool,
    pub desktop: String,
}

impl Capabilities {
    pub fn probe() -> Result<Self> {
        let globals = probe_with_retry(Duration::from_secs(10), wayland_globals)?;
        Ok(Self {
            layer_shell: globals.contains("zwlr_layer_shell_v1"),
            data_control: globals.contains("ext_data_control_manager_v1")
                || globals.contains("zwlr_data_control_manager_v1"),
            desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        })
    }

    pub fn is_gnome(&self) -> bool {
        self.desktop
            .split(':')
            .any(|d| d.eq_ignore_ascii_case("GNOME"))
    }
}

fn probe_with_retry(
    timeout: Duration,
    mut probe: impl FnMut() -> Result<HashSet<String>>,
) -> Result<HashSet<String>> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe() {
            Ok(globals) => return Ok(globals),
            Err(e) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(e).context("cannot query Wayland compositor after startup retries; check WAYLAND_DISPLAY and the compositor session");
                }
                tracing::debug!("Wayland compositor not ready ({e}); retrying");
                std::thread::sleep(remaining.min(Duration::from_millis(250)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_compositor_without_protocols_is_valid() {
        let globals = probe_with_retry(Duration::ZERO, || Ok(HashSet::new())).unwrap();
        assert!(globals.is_empty());
    }

    #[test]
    fn unavailable_compositor_is_an_error() {
        assert!(probe_with_retry(Duration::ZERO, || anyhow::bail!("no socket")).is_err());
    }

    #[test]
    fn startup_retries_a_failed_connection() {
        let mut attempts = 0;
        probe_with_retry(Duration::from_secs(1), || {
            attempts += 1;
            if attempts == 1 {
                anyhow::bail!("not ready");
            }
            Ok(HashSet::new())
        })
        .unwrap();
        assert_eq!(attempts, 2);
    }
}
