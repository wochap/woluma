//! Lists the names of the compositor's connected outputs.

use anyhow::{Context, Result};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

#[derive(Default)]
struct State {
    names: Vec<String>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        _: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            // `wl_output.name` needs version 4.
            if interface == wl_output::WlOutput::interface().name && version >= 4 {
                registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, ());
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.names.push(name);
        }
    }
}

/// Names of the currently connected outputs, in compositor order.
pub fn list() -> Result<Vec<String>> {
    let connection = Connection::connect_to_env().context("unable to connect to Wayland")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State::default();
    // Globals first, then the bound outputs' events.
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    Ok(state.names)
}
