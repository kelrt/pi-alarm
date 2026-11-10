import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';

import '../data/local_store.dart';
import 'framing.dart';
import 'pi_client.dart';
import 'uuids.dart';

/// Where the connection to the Pi stands, for the status indicator.
enum PiLinkState {
  /// The phone has no Bluetooth LE, or the app may not use it.
  unsupported,

  /// Bluetooth is switched off on the phone.
  bluetoothOff,

  /// No Pi has been remembered yet; the user must tap "Find Pi" once.
  noDevice,

  /// Scanning for a Pi (first-time setup).
  searching,

  /// Trying to reach the remembered Pi. Retries by itself.
  connecting,

  /// Connected and the clock has been sent. Requests can be made.
  connected,
}

/// A manual action offered by the Pi, e.g. `fan_3` labelled "Fan 3".
class PiAction {
  const PiAction(this.name, this.label);
  final String name;
  final String label;
}

/// Shown before the Pi has told us which actions it really has.
const List<PiAction> defaultActions = [
  PiAction('fan_off', 'Fan off'),
  PiAction('fan_3', 'Fan 3'),
  PiAction('light_toggle', 'Light'),
];

/// The BLE link to the Pi.
///
/// After [start], this class looks after the connection by itself: it
/// connects to the remembered Pi, retries while it is out of range, and on
/// every connect sends the phone's time (`set_time`) before anything else.
/// The UI only reads [state] and calls [request].
///
/// It is a [ChangeNotifier]: widgets and the repository listen to it and are
/// told whenever [state] or [actions] change.
class PiConnection extends ChangeNotifier implements PiClient {
  PiConnection(this._store);

  final LocalStore _store;

  PiLinkState _state = PiLinkState.connecting;
  PiLinkState get state => _state;

  /// The actions the Pi offers (from its `status` response).
  List<PiAction> actions = defaultActions;

  /// A problem the user should know about, e.g. a time zone mismatch.
  String? warning;

  /// The most recent connection error, for the status tooltip.
  String? lastError;

  BluetoothDevice? _device;
  BluetoothCharacteristic? _requestCharacteristic;
  StreamSubscription<BluetoothAdapterState>? _adapterSubscription;
  StreamSubscription<BluetoothConnectionState>? _connectionSubscription;
  StreamSubscription<List<int>>? _responseSubscription;

  final Reassembler _reassembler = Reassembler();

  /// The request currently waiting for its response, if any.
  Completer<Map<String, dynamic>>? _waiting;
  int _waitingId = 0;
  int _nextRequestId = 1;

  /// Requests are sent one at a time; each new one is chained onto this.
  Future<void> _queue = Future.value();

  bool _connectLoopRunning = false;
  bool _settingUp = false;
  bool _disposed = false;

  @override
  bool get isConnected => _state == PiLinkState.connected;

  /// Begin managing the connection. Call once at app start.
  Future<void> start() async {
    if (await FlutterBluePlus.isSupported == false) {
      _setState(PiLinkState.unsupported);
      return;
    }
    // Emits the current adapter state immediately, then every change.
    _adapterSubscription = FlutterBluePlus.adapterState.listen((adapter) {
      if (adapter == BluetoothAdapterState.on) {
        _connectToRememberedPi();
      } else if (adapter == BluetoothAdapterState.unauthorized) {
        _setState(PiLinkState.unsupported);
      } else if (adapter == BluetoothAdapterState.off) {
        _setState(PiLinkState.bluetoothOff);
      }
    });
  }

  /// First-time setup: scan for a Pi advertising our service, remember the
  /// first one found, and connect to it.
  Future<void> findPi() async {
    if (_state == PiLinkState.searching) return;
    _setState(PiLinkState.searching);
    lastError = null;

    StreamSubscription<List<ScanResult>>? results;
    try {
      final found = Completer<BluetoothDevice?>();
      results = FlutterBluePlus.onScanResults.listen((list) {
        if (list.isNotEmpty && !found.isCompleted) {
          found.complete(list.first.device);
        }
      });
      await FlutterBluePlus.startScan(
        withServices: [PiUuids.service],
        timeout: const Duration(seconds: 15),
      );
      final device = await found.future.timeout(
        const Duration(seconds: 16),
        onTimeout: () => null,
      );
      await FlutterBluePlus.stopScan();

      if (device == null) {
        lastError = 'No Pi found nearby. Is the pi-alarm service running?';
        _setState(PiLinkState.noDevice);
        return;
      }
      await _store.setPiRemoteId(device.remoteId.str);
      _attach(device);
    } catch (error) {
      lastError = 'Scan failed: $error';
      _setState(PiLinkState.noDevice);
    } finally {
      await results?.cancel();
    }
  }

  /// Forget the remembered Pi (to pair with a different one).
  Future<void> forgetPi() async {
    await _store.setPiRemoteId(null);
    final device = _device;
    _device = null;
    await _connectionSubscription?.cancel();
    _connectionSubscription = null;
    _requestCharacteristic = null;
    try {
      await device?.disconnect();
    } catch (_) {
      // Already disconnected: fine.
    }
    _setState(PiLinkState.noDevice);
  }

  void _connectToRememberedPi() {
    final id = _store.piRemoteId;
    if (id == null) {
      _setState(PiLinkState.noDevice);
      return;
    }
    // Already looking after this device.
    if (_device?.remoteId.str == id) {
      _runConnectLoop(_device!);
      return;
    }
    _attach(BluetoothDevice.fromId(id));
  }

  /// Start following [device]: watch its connection state and keep trying
  /// to connect.
  void _attach(BluetoothDevice device) {
    _connectionSubscription?.cancel();
    _device = device;
    _setState(PiLinkState.connecting);

    _connectionSubscription = device.connectionState.listen((connection) {
      if (connection == BluetoothConnectionState.connected) {
        _setUpAfterConnect(device);
      } else {
        _onDisconnected(device);
      }
    });
    _runConnectLoop(device);
  }

  /// Keep trying to connect until it works, with growing pauses. Runs only
  /// while the app is open; Android stops it when the app is closed.
  Future<void> _runConnectLoop(BluetoothDevice device) async {
    if (_connectLoopRunning) return;
    _connectLoopRunning = true;
    var attempt = 0;
    try {
      while (!_disposed && identical(_device, device) && device.isDisconnected) {
        if (FlutterBluePlus.adapterStateNow != BluetoothAdapterState.on) {
          return; // The adapter listener restarts us when Bluetooth is on.
        }
        try {
          // `License.nonprofit` covers personal use; see the package's
          // licence if that ever changes. A direct connect (not Android's
          // slower "autoConnect") makes opening the app at bedtime snappy.
          await device.connect(
            license: License.nonprofit,
            timeout: const Duration(seconds: 10),
          );
          return; // The connection listener takes over from here.
        } catch (error) {
          lastError = 'Pi not reachable: $error';
          attempt += 1;
          // 2 s, 4 s, 6 s ... capped at 15 s between attempts.
          final seconds = attempt * 2 > 15 ? 15 : attempt * 2;
          await Future<void>.delayed(Duration(seconds: seconds));
        }
      }
    } finally {
      _connectLoopRunning = false;
    }
  }

  /// Runs once per connection: find the characteristics, subscribe to
  /// responses, send the time, ask what the Pi offers.
  Future<void> _setUpAfterConnect(BluetoothDevice device) async {
    if (_settingUp || isConnected) return;
    _settingUp = true;
    try {
      final services = await device.discoverServices();
      final service =
          services.firstWhere((service) => service.uuid == PiUuids.service);
      final request = service.characteristics
          .firstWhere((c) => c.uuid == PiUuids.request);
      final response = service.characteristics
          .firstWhere((c) => c.uuid == PiUuids.response);

      _reassembler.reset();
      await _responseSubscription?.cancel();
      final subscription = response.onValueReceived.listen(_onResponseChunk);
      _responseSubscription = subscription;
      device.cancelWhenDisconnected(subscription);
      await response.setNotifyValue(true);

      _requestCharacteristic = request;

      // The clock goes first: the Pi has no clock of its own.
      await _sendTime();
      await _loadStatus();

      lastError = null;
      _setState(PiLinkState.connected);
    } catch (error) {
      lastError = 'Connected, but setup failed: $error';
      _requestCharacteristic = null;
      // Drop the link; the disconnect handler starts the retry loop.
      try {
        await device.disconnect();
      } catch (_) {
        // Already gone.
      }
    } finally {
      _settingUp = false;
    }
  }

  void _onDisconnected(BluetoothDevice device) {
    _requestCharacteristic = null;
    _reassembler.reset();
    // Whoever is waiting for a response will not get one now.
    _failWaiting(PiUnavailable('Disconnected from the Pi'));
    if (_disposed || !identical(_device, device)) return;
    if (_state == PiLinkState.connected) {
      _setState(PiLinkState.connecting);
    }
    _runConnectLoop(device);
  }

  /// Tell the Pi what time it is. Also learns whether the Pi's time zone
  /// agrees with the phone's.
  Future<void> _sendTime() async {
    final now = DateTime.now();
    final response = await _send('set_time', {
      'unix_ms': now.millisecondsSinceEpoch,
      'utc_offset_min': now.timeZoneOffset.inMinutes,
    });
    if (response['tz_match'] == false) {
      final time = response['time'] as Map<String, dynamic>?;
      warning = 'The Pi is set to time zone ${time?['tz']}, which differs '
          'from this phone. Alarms use the Pi\'s zone.';
    } else {
      warning = null;
    }
  }

  Future<void> _loadStatus() async {
    final response = await _send('status');
    final list = response['actions'];
    if (list is List && list.isNotEmpty) {
      actions = [
        for (final item in list)
          PiAction(
            (item as Map<String, dynamic>)['name'] as String,
            (item['label'] as String?) ?? item['name'] as String,
          ),
      ];
    }
  }

  @override
  Future<Map<String, dynamic>> request(
    String op, [
    Map<String, dynamic> params = const {},
  ]) {
    if (!isConnected) {
      return Future.error(PiUnavailable('The Pi is not connected'));
    }
    return _send(op, params);
  }

  /// Queue one request behind any that are still in progress.
  Future<Map<String, dynamic>> _send(
    String op, [
    Map<String, dynamic> params = const {},
  ]) {
    final result = _queue.then((_) => _exchange(op, params));
    // Keep the queue going whether this request succeeds or fails.
    _queue = result.then((_) {}, onError: (_) {});
    return result;
  }

  /// Write one request and wait for its response.
  Future<Map<String, dynamic>> _exchange(
    String op,
    Map<String, dynamic> params,
  ) async {
    final device = _device;
    final characteristic = _requestCharacteristic;
    if (device == null || characteristic == null || device.isDisconnected) {
      throw PiUnavailable('The Pi is not connected');
    }

    final id = _nextRequestId++;
    final message = utf8.encode(jsonEncode({
      'v': protocolVersion,
      'id': id,
      'op': op,
      ...params,
    }));

    final waiting = Completer<Map<String, dynamic>>();
    // If the link drops while we are still writing, the completer fails
    // before anyone awaits it. `ignore` stops Dart from reporting that as an
    // unhandled error; the `await` further down still receives it.
    waiting.future.ignore();
    _waiting = waiting;
    _waitingId = id;

    try {
      // A write can carry MTU - 3 bytes.
      var chunkLength = device.mtuNow - 3;
      if (chunkLength < minChunkLength) chunkLength = minChunkLength;
      if (chunkLength > maxChunkLength) chunkLength = maxChunkLength;
      for (final chunk in chunkMessage(message, chunkLength)) {
        // "With response": the Pi confirms each chunk, so order is kept.
        await characteristic.write(chunk);
      }
      // Transmitting an action takes the Pi about half a second; ten
      // seconds means something is wrong.
      final response =
          await waiting.future.timeout(const Duration(seconds: 10));

      if (response['ok'] != true) {
        final error = response['error'] as Map<String, dynamic>?;
        throw PiRejected(
          (error?['code'] as String?) ?? 'unknown',
          (error?['message'] as String?) ?? 'The Pi refused the request',
        );
      }
      return response;
    } on TimeoutException {
      throw PiUnavailable('The Pi did not answer');
    } on FlutterBluePlusException catch (error) {
      throw PiUnavailable('Bluetooth error: ${error.description}');
    } finally {
      if (identical(_waiting, waiting)) _waiting = null;
    }
  }

  void _onResponseChunk(List<int> chunk) {
    try {
      final message = _reassembler.push(chunk);
      if (message == null) return; // more chunks to come
      final response = jsonDecode(utf8.decode(message)) as Map<String, dynamic>;
      final waiting = _waiting;
      if (waiting != null &&
          !waiting.isCompleted &&
          response['id'] == _waitingId) {
        waiting.complete(response);
      }
    } catch (error) {
      debugPrint('Bad data from the Pi: $error');
    }
  }

  void _failWaiting(Object error) {
    final waiting = _waiting;
    if (waiting != null && !waiting.isCompleted) {
      waiting.completeError(error);
    }
  }

  void _setState(PiLinkState next) {
    if (_disposed) return;
    _state = next;
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _adapterSubscription?.cancel();
    _connectionSubscription?.cancel();
    _responseSubscription?.cancel();
    _device?.disconnect();
    super.dispose();
  }
}
