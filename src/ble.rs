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

    let mut conversation = Conversation::new(app);
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
                conversation.reset();
                notifier = Some(new_notifier);
            }
            BleEvent::Chunk { data, mtu } => {
                let response_chunks = conversation.on_chunk(&data, mtu).await?;
                send_chunks(&mut notifier, response_chunks).await;
            }
        }
    }
}

/// The part of the server that has nothing to do with Bluetooth: request
/// chunks go in, response chunks come out. It is separate from `serve` so
/// that it can be tested without BlueZ.
struct Conversation {
    app: Arc<App>,
    reassembler: Reassembler,
}

impl Conversation {
    fn new(app: Arc<App>) -> Conversation {
        Conversation {
            app,
            reassembler: Reassembler::new(),
        }
    }

    /// Forget a half-received request (the phone reconnected).
    fn reset(&mut self) {
        self.reassembler.reset();
    }

    /// Take one chunk the phone wrote. If it completes a request, handle the
    /// request and return the response as chunks sized for `mtu`. Otherwise
    /// return an empty list: more chunks are needed, or the chunk was bad.
    async fn on_chunk(&mut self, data: &[u8], mtu: u16) -> anyhow::Result<Vec<Vec<u8>>> {
        let message = match self.reassembler.push(data) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(Vec::new()),
            Err(error) => {
                log::warn!("bad chunk from the phone: {error}");
                return Ok(Vec::new());
            }
        };

        // Handling a request may transmit (about 0.3 s of blocking,
        // precisely timed work), so it runs on a separate thread and we wait
        // for the result without blocking Bluetooth.
        let app = Arc::clone(&self.app);
        let response = tokio::task::spawn_blocking(move || handle_request(&app, &message))
            .await
            .context("request handler crashed")?;

        Ok(chunk_message(&response, response_chunk_len(mtu)))
    }
}

/// How large a response chunk may be on a connection with this MTU.
///
/// The MTU is the packet size the phone and the Pi agreed on; BlueZ reports
/// it with every write. A notification can carry MTU - 3 bytes.
fn response_chunk_len(mtu: u16) -> usize {
    usize::from(mtu)
        .saturating_sub(3)
        .clamp(MIN_CHUNK_LEN, MAX_CHUNK_LEN)
}

/// Send response chunks to the phone, one notification each.
async fn send_chunks(notifier: &mut Option<CharacteristicNotifier>, chunks: Vec<Vec<u8>>) {
    if chunks.is_empty() {
        return;
    }
    // `as_mut` lets us borrow the notifier inside the Option mutably.
    let Some(active) = notifier.as_mut() else {
        log::warn!("request handled, but the phone is not subscribed to responses");
        return;
    };
    for chunk in chunks {
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
                async move { forward_chunk(&events, data, request.offset, request.mtu).await }
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
                async move { read_from_offset(&info, request.offset) }.boxed()
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

/// What the write callback does: pass one chunk on to the event loop.
async fn forward_chunk(
    events: &mpsc::Sender<BleEvent>,
    data: Vec<u8>,
    offset: u16,
    mtu: u16,
) -> Result<(), ReqError> {
    // We do our own chunking, so BLE "long writes" (which arrive with an
    // offset) are not supported.
    if offset != 0 {
        return Err(ReqError::InvalidOffset);
    }
    // Fails only if the event loop has gone away.
    events
        .send(BleEvent::Chunk { data, mtu })
        .await
        .map_err(|_| ReqError::Failed)
}

/// What the read callback does: a phone with a small packet size reads a
/// long value in pieces, each read starting at the offset it has reached.
fn read_from_offset(value: &[u8], offset: u16) -> Result<Vec<u8>, ReqError> {
    let offset = usize::from(offset);
    if offset > value.len() {
        return Err(ReqError::InvalidOffset);
    }
    Ok(value[offset..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::tests::test_app;
    use crate::protocol::{CHUNK_FIRST, CHUNK_LAST};
    use serde_json::Value;

    /// Glue response chunks back together and parse the JSON.
    fn reassemble(chunks: &[Vec<u8>]) -> Value {
        let mut reassembler = Reassembler::new();
        let mut message = None;
        for chunk in chunks {
            message = reassembler.push(chunk).unwrap();
        }
        serde_json::from_slice(&message.expect("incomplete response")).unwrap()
    }

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

    #[test]
    fn response_chunks_fit_the_connection() {
        // The BLE default MTU of 23 leaves 20 bytes.
        assert_eq!(response_chunk_len(23), 20);
        // A typical negotiated Android MTU.
        assert_eq!(response_chunk_len(247), 244);
        // The largest MTU (517) is capped by the 512-byte value limit.
        assert_eq!(response_chunk_len(517), 512);
        assert_eq!(response_chunk_len(u16::MAX), 512);
        // Nonsense values fall back to the smallest size that always works.
        assert_eq!(response_chunk_len(0), 20);
        assert_eq!(response_chunk_len(5), 20);
    }

    #[tokio::test]
    async fn a_write_reaches_the_event_loop_as_a_chunk() {
        let (events, mut receiver) = mpsc::channel(4);

        forward_chunk(&events, vec![0x03, b'{', b'}'], 0, 185)
            .await
            .unwrap();
        match receiver.recv().await.unwrap() {
            BleEvent::Chunk { data, mtu } => {
                assert_eq!(data, vec![0x03, b'{', b'}']);
                assert_eq!(mtu, 185);
            }
            BleEvent::Subscribed(_) => panic!("expected a chunk"),
        }

        // A "long write" (non-zero offset) is refused and forwards nothing.
        let refused = forward_chunk(&events, vec![1, 2, 3], 18, 185).await;
        assert!(matches!(refused, Err(ReqError::InvalidOffset)));
        assert!(receiver.try_recv().is_err());

        // If the event loop is gone, the phone is told the write failed.
        drop(receiver);
        let failed = forward_chunk(&events, vec![0x03], 0, 185).await;
        assert!(matches!(failed, Err(ReqError::Failed)));
    }

    #[test]
    fn a_value_can_be_read_from_any_offset() {
        let value = b"0123456789";
        assert_eq!(read_from_offset(value, 0).unwrap(), value);
        assert_eq!(read_from_offset(value, 4).unwrap(), b"456789");
        // Reading exactly at the end gives an empty rest, not an error.
        assert_eq!(read_from_offset(value, 10).unwrap(), b"");
        assert!(matches!(
            read_from_offset(value, 11),
            Err(ReqError::InvalidOffset)
        ));
    }

    #[tokio::test]
    async fn a_one_chunk_request_gets_a_response() {
        let test = test_app();
        let mut conversation = Conversation::new(Arc::clone(&test.app));

        let mut chunk = vec![CHUNK_FIRST | CHUNK_LAST];
        chunk.extend_from_slice(br#"{"v":1,"id":7,"op":"list"}"#);
        let response = conversation.on_chunk(&chunk, 247).await.unwrap();

        assert_eq!(response.len(), 1, "a short response is one notification");
        let value = reassemble(&response);
        assert_eq!(value["ok"], true);
        assert_eq!(value["id"], 7);
        assert_eq!(value["alarms"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn a_whole_session_at_the_smallest_mtu() {
        // The connect sequence from PROTOCOL.md, with every message cut into
        // 20-byte chunks as on a connection that never negotiated an MTU.
        let test = test_app();
        let mut conversation = Conversation::new(Arc::clone(&test.app));
        let requests = [
            r#"{"v":1,"id":1,"op":"set_time","unix_ms":1794334050000,"utc_offset_min":-360}"#,
            r#"{"v":1,"id":2,"op":"status"}"#,
            r#"{"v":1,"id":3,"op":"put","alarm":{"id":"work","label":"Work","hour":6,"minute":30,"days":["mon","tue","wed","thu","fri"],"enabled":true,"steps":[{"offset_s":-600,"action":"light_toggle"},{"offset_s":0,"action":"fan_3"}]}}"#,
            r#"{"v":1,"id":4,"op":"list"}"#,
            r#"{"v":1,"id":5,"op":"action","name":"light_toggle"}"#,
        ];

        let mut responses = Vec::new();
        for request in requests {
            let chunks = chunk_message(request.as_bytes(), MIN_CHUNK_LEN);
            let last = chunks.len() - 1;
            for (index, chunk) in chunks.iter().enumerate() {
                let out = conversation.on_chunk(chunk, 23).await.unwrap();
                if index < last {
                    assert!(out.is_empty(), "no response before the request is complete");
                } else {
                    // Every response chunk fits a 23-byte MTU.
                    assert!(out.iter().all(|chunk| chunk.len() <= 20));
                    responses.push(reassemble(&out));
                }
            }
        }

        assert_eq!(responses.len(), 5);
        for (index, response) in responses.iter().enumerate() {
            assert_eq!(response["ok"], true, "request {}: {response}", index + 1);
            assert_eq!(response["id"], index + 1);
        }
        // set_time moved the mock clock (it was 7.5 minutes behind).
        assert_eq!(responses[0]["adjusted"], true);
        assert_eq!(test.clock.set_calls().len(), 1);
        // status lists the three actions.
        assert_eq!(responses[1]["actions"].as_array().unwrap().len(), 3);
        // put stored the alarm and list returns it.
        assert_eq!(responses[2]["created"], true);
        assert_eq!(responses[3]["alarms"][0]["id"], "work");
        // action transmitted on the (mock) radio.
        assert_eq!(test.sent.count(), 1);
    }

    #[tokio::test]
    async fn bad_chunks_are_ignored_and_the_conversation_recovers() {
        let test = test_app();
        let mut conversation = Conversation::new(Arc::clone(&test.app));

        // An unknown header, an empty write, and a continuation with no start.
        assert!(conversation
            .on_chunk(&[0x80, 1], 23)
            .await
            .unwrap()
            .is_empty());
        assert!(conversation.on_chunk(&[], 23).await.unwrap().is_empty());
        assert!(conversation
            .on_chunk(&[0x00, 1], 23)
            .await
            .unwrap()
            .is_empty());

        // Half a request, then the phone reconnects.
        assert!(conversation
            .on_chunk(&[CHUNK_FIRST, b'{', b'"'], 23)
            .await
            .unwrap()
            .is_empty());
        conversation.reset();

        let mut chunk = vec![CHUNK_FIRST | CHUNK_LAST];
        chunk.extend_from_slice(br#"{"v":1,"op":"list"}"#);
        let response = conversation.on_chunk(&chunk, 23).await.unwrap();
        assert_eq!(reassemble(&response)["ok"], true);
    }

    #[tokio::test]
    async fn invalid_json_still_gets_an_error_response() {
        let test = test_app();
        let mut conversation = Conversation::new(Arc::clone(&test.app));
        let mut chunk = vec![CHUNK_FIRST | CHUNK_LAST];
        chunk.extend_from_slice(b"this is not json");
        let response = conversation.on_chunk(&chunk, 247).await.unwrap();
        let value = reassemble(&response);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "bad_json");
    }
}
