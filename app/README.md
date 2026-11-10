# pi_alarm (Flutter app)

Phone app for pi-alarm. See `../BUILD.md` for setup and `../PROTOCOL.md` for
the messages it exchanges with the Pi.

**This project was written without the Flutter SDK installed. It has never
been compiled or run.** The platform folders (`android/`) do not exist yet;
`flutter create` generates them (BUILD.md, "Phone app").

```
lib/
  main.dart                     start-up: creates the three long-lived objects
  models/alarm.dart             Alarm and AlarmStep, same JSON as the daemon
  ble/uuids.dart                service and characteristic UUIDs
  ble/framing.dart              chunk framing (twin of src/protocol.rs)
  ble/pi_client.dart            small interface + the two error types
  ble/pi_connection.dart        BLE link: auto-connect, retries, set_time
  data/local_store.dart         what the phone remembers (SharedPreferences)
  data/alarm_repository.dart    alarm list + offline queue + sync
  screens/home_screen.dart      manual buttons and the alarm list
  screens/alarm_editor_screen.dart
  widgets/status_indicator.dart the small status dot in the app bar
  widgets/manual_buttons.dart
test/                           unit tests for framing, model and the queue
```
