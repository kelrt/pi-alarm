//! The Bluetooth Low Energy (BLE) GATT server.
//!
//! BLE devices expose "characteristics": small named values a phone can
//! write to, read, or subscribe to. We offer one service with three:
//!
//! | Characteristic | The phone can | Used for                            |
//! |----------------|---------------|-------------------------------------|
//! | request        | write         | sending request chunks to the Pi    |
//! | response       | subscribe     | receiving response chunks (notify)  |
//! | info           | read          | a quick "is this the right device"  |
//!
//! All real work happens in `protocol.rs`; this file only moves bytes
//! between BlueZ (the Linux Bluetooth stack, reached through the `bluer`
//! crate) and `protocol::handle_request`.
//!
//! # About the async code
//!
//! `bluer` is asynchronous and callback based: we hand it closures, and it
//! calls them when the phone writes or subscribes. To keep that part tiny,
//! each closure does one thing: put an event on a channel. An ordinary loop
//! (`serve`) takes events off the channel one at a time, so all the logic
//! reads top to bottom.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bluer::adv::Advertisement;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicNotifier, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod,
    ReqError, Service,
};
use bluer::{SessionEvent, Uuid};
use futures::{FutureExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::app::App;
use crate::protocol::{
    chunk_message, handle_request, Reassembler, MIN_CHUNK_LEN, PROTOCOL_VERSION,
};

// These four UUIDs are random numbers made up for this project. The phone
// app has the same values (app/lib/ble/uuids.dart) and PROTOCOL.md lists them.
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x7a1e0001_5c9b_4f6a_9d2e_b1a2c3d4e5f6);
pub const REQUEST_UUID: Uuid = Uuid::from_u128(0x7a1e0002_5c9b_4f6a_9d2e_b1a2c3d4e5f6);
pub const RESPONSE_UUID: Uuid = Uuid::from_u128(0x7a1e0003_5c9b_4f6a_9d2e_b1a2c3d4e5f6);
pub const INFO_UUID: Uuid = Uuid::from_u128(0x7a1e0004_5c9b_4f6a_9d2e_b1a2c3d4e5f6);

/// Largest chunk we ever send. BLE limits a characteristic value to 512
/// bytes no matter how large the negotiated packet size is.
const MAX_CHUNK_LEN: usize = 512;

/// If Bluetooth is not available (for example `bluetoothd` is still
/// starting at boot), try again after this long.
const RETRY_DELAY: Duration = Duration::from_secs(10);

/// What the callbacks tell the main loop.
enum BleEvent {
    /// The phone wrote one chunk to the request characteristic.
    Chunk { data: Vec<u8>, mtu: u16 },
    /// The phone subscribed to the response characteristic.
    Subscribed(CharacteristicNotifier),
}

/// Run the BLE server forever. Errors are logged and retried, because the
/// alarm clock must keep working even when Bluetooth does not.
pub async fn run(app: Arc<App>, device_name: String) {
    loop {
        // `Arc::clone` gives `serve` its own handle to the shared `App`.
        match serve(Arc::clone(&app), &device_name).await {
            Ok(()) => log::warn!("BLE server stopped; restarting"),
            Err(error) => log::error!("BLE server failed: {error:#}"),
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

/// Register the GATT service, advertise, and process events.
async fn serve(app: Arc<App>, device_name: &str) -> anyhow::Result<()> {
    let session = bluer::Session::new()
        .await
        .context("cannot connect to BlueZ (is bluetoothd running?)")?;
    let adapter = session
        .default_adapter()
        .await
        .context("no Bluetooth adapter found")?;
    adapter
        .set_powered(true)
        .await
        .context("cannot power on the Bluetooth adapter")?;

    // The channel that carries events from the callbacks to the loop below.
    // `tx` (sender) can be cloned, one per callback; `rx` (receiver) is ours.
    let (events_tx, mut events_rx) = mpsc::channel::<BleEvent>(64);

    let application = build_application(events_tx);
    // Both handles must stay alive: dropping one unregisters it. The
    // leading underscore tells the compiler we know they are "unused".
    let _application_handle = adapter
        .serve_gatt_application(application)
        .await
        .context("cannot register the GATT service")?;

    let advertisement = Advertisement {
        // The phone finds the Pi by this service UUID, not by its name.
        service_uuids: [SERVICE_UUID].into_iter().collect(),
        local_name: Some(device_name.to_string()),
        discoverable: Some(true),
        ..Default::default()
    };
    let _advertisement_handle = adapter
        .advertise(advertisement)
        .await
        .context("cannot start BLE advertising")?;

    log::info!(
        "BLE ready on {} ({}), advertising as `{device_name}`",
        adapter.name(),
        adapter.address().await?
    );

    // BlueZ announces when an adapter appears or disappears. If that happens
    // to the adapter we are using (bluetoothd was restarted or updated, for
    // example), our service registration is gone with it. `Box::pin` puts
    // the stream on the heap at a fixed address, which `next()` requires.
    let adapter_name = adapter.name().to_string();
    let mut adapter_changes = Box::pin(
        session
            .events()
            .await
            .context("cannot watch for Bluetooth adapter changes")?,
    );

    let mut reassembler = Reassembler::new();
    // The current subscription, if the phone has one.
    let mut notifier: Option<CharacteristicNotifier> = None;

    loop {
        // Wait for the next thing to happen: an event from the phone, or a
        // change to the adapter.
        let event = tokio::select! {
            event = events_rx.recv() => match event {
                Some(event) => event,
                // Every sender is gone: the GATT application was torn down.
                None => return Ok(()),
            },
            change = adapter_changes.next() => match change {
                Some(SessionEvent::AdapterAdded(name)) | Some(SessionEvent::AdapterRemoved(name))
                    if name == adapter_name =>
                {
                    // Returning an error makes `run` register everything
                    // again after a short pause.
                    anyhow::bail!("Bluetooth adapter {name} was reset");
                }
                Some(_) => continue, // some other adapter
                None => anyhow::bail!("lost the connection to BlueZ"),
            },
        };

        match event {
            BleEvent::Subscribed(new_notifier) => {
                log::info!("phone subscribed to responses");
                // A new subscription means a new connection: drop leftovers.
                reassembler.reset();
                notifier = Some(new_notifier);
            }
            BleEvent::Chunk { data, mtu } => {
                let message = match reassembler.push(&data) {
                    Ok(Some(message)) => message,
                    Ok(None) => continue, // need more chunks
                    Err(error) => {
                        log::warn!("bad chunk from the phone: {error}");
                        continue;
                    }
                };

                // Handling a request may transmit (about 0.3 s of blocking,
                // precisely timed work), so it runs on a separate thread and
                // we wait for the result without blocking Bluetooth.
                let app_for_task = Arc::clone(&app);
                let response =
                    tokio::task::spawn_blocking(move || handle_request(&app_for_task, &message))
                        .await
                        .context("request handler crashed")?;

                send_response(&mut notifier, &response, mtu).await;
            }
        }
    }
}

/// Send a response to the phone as one or more notifications.
async fn send_response(notifier: &mut Option<CharacteristicNotifier>, response: &[u8], mtu: u16) {
    // `as_mut` lets us borrow the notifier inside the Option mutably.
    let Some(active) = notifier.as_mut() else {
        log::warn!("request handled, but the phone is not subscribed to responses");
        return;
    };

    // A notification can carry MTU - 3 bytes. The MTU is the packet size
    // the phone and the Pi agreed on; BlueZ tells us with every write.
    let chunk_len = usize::from(mtu)
        .saturating_sub(3)
        .clamp(MIN_CHUNK_LEN, MAX_CHUNK_LEN);

    for chunk in chunk_message(response, chunk_len) {
        if let Err(error) = active.notify(chunk).await {
            log::warn!("could not notify the phone (disconnected?): {error}");
            // Forget the dead subscription; a reconnect brings a new one.
            *notifier = None;
            return;
        }
    }
}

/// Describe our service to BlueZ.
fn build_application(events: mpsc::Sender<BleEvent>) -> Application {
    // Each closure below is called by bluer many times, and each call must
    // return a future that owns everything it uses. Hence the pattern:
    // clone the sender, then `async move` the clone into the future.
    // `.boxed()` (from `futures::FutureExt`) puts the future in the
    // heap-allocated form bluer's function types ask for.
    let write_events = events.clone();
    let notify_events = events;

    let info = json!({
        "v": PROTOCOL_VERSION,
        "name": "pi-alarm",
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string()
    .into_bytes();

    let request_characteristic = Characteristic {
        uuid: REQUEST_UUID,
        write: Some(CharacteristicWrite {
            // "Write with response": the phone gets a confirmation for
            // every chunk, so chunks cannot be lost or reordered.
            write: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |data, request| {
                let events = write_events.clone();
                async move {
                    // We do our own chunking, so BLE "long writes" (which
                    // arrive with an offset) are not supported.
                    if request.offset != 0 {
                        return Err(ReqError::InvalidOffset);
                    }
                    let event = BleEvent::Chunk {
                        data,
                        mtu: request.mtu,
                    };
                    // Fails only if the main loop has gone away.
                    events.send(event).await.map_err(|_| ReqError::Failed)
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    };

    let response_characteristic = Characteristic {
        uuid: RESPONSE_UUID,
        notify: Some(CharacteristicNotify {
            notify: true,
            method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                let events = notify_events.clone();
                async move {
                    // If the loop is gone there is nobody to tell; ignore.
                    let _ = events.send(BleEvent::Subscribed(notifier)).await;
                }
                .boxed()
            })),
            ..Default::default()
        }),
        ..Default::default()
    };

    let info_characteristic = Characteristic {
        uuid: INFO_UUID,
        read: Some(CharacteristicRead {
            read: true,
            fun: Box::new(move |request| {
                let info = info.clone();
                async move {
                    // A read may ask for the value starting at an offset.
                    let offset = usize::from(request.offset);
                    if offset > info.len() {
                        return Err(ReqError::InvalidOffset);
                    }
                    Ok(info[offset..].to_vec())
                }
                .boxed()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    Application {
        services: vec![Service {
            uuid: SERVICE_UUID,
            primary: true,
            characteristics: vec![
                request_characteristic,
                response_characteristic,
                info_characteristic,
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_match_the_documented_text_form() {
        // The same strings appear in PROTOCOL.md and in the Flutter app.
        assert_eq!(
            SERVICE_UUID.to_string(),
            "7a1e0001-5c9b-4f6a-9d2e-b1a2c3d4e5f6"
        );
        assert_eq!(
            REQUEST_UUID.to_string(),
            "7a1e0002-5c9b-4f6a-9d2e-b1a2c3d4e5f6"
        );
        assert_eq!(
            RESPONSE_UUID.to_string(),
            "7a1e0003-5c9b-4f6a-9d2e-b1a2c3d4e5f6"
        );
        assert_eq!(
            INFO_UUID.to_string(),
            "7a1e0004-5c9b-4f6a-9d2e-b1a2c3d4e5f6"
        );
    }

    #[test]
    fn the_application_has_one_service_with_three_characteristics() {
        let (events, _receiver) = mpsc::channel(1);
        let application = build_application(events);
        assert_eq!(application.services.len(), 1);
        let service = &application.services[0];
        assert_eq!(service.uuid, SERVICE_UUID);
        assert!(service.primary);

        let uuids: Vec<Uuid> = service.characteristics.iter().map(|c| c.uuid).collect();
        assert_eq!(uuids, vec![REQUEST_UUID, RESPONSE_UUID, INFO_UUID]);
        assert!(service.characteristics[0].write.is_some());
        assert!(service.characteristics[1].notify.is_some());
        assert!(service.characteristics[2].read.is_some());
    }
}
