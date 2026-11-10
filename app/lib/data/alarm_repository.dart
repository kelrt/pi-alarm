import 'package:flutter/foundation.dart';

import '../ble/pi_client.dart';
import '../models/alarm.dart';
import 'local_store.dart';

/// The app's list of alarms, and the queue that gets changes to the Pi.
///
/// How syncing works:
///
/// * Saving or deleting changes the list on the phone immediately and adds a
///   [PendingChange] to a queue that is stored on the phone.
/// * Whenever the Pi is connected, [sync] sends the queue in order. When the
///   queue is empty it downloads the Pi's list, which is the truth.
/// * If the Pi is out of range the queue simply waits. Nothing is lost when
///   the app is closed, and [isPending] lets the UI mark unsent alarms.
///
/// Sending a change twice is harmless (the protocol's `put` and `delete` are
/// idempotent), so a change stays queued until the Pi has answered.
class AlarmRepository extends ChangeNotifier {
  AlarmRepository(this._client, this._store) {
    _alarms = _store.loadAlarms();
    _pending = _store.loadPending();
    // Sync whenever the connection comes up.
    _client.addListener(_onClientChanged);
  }

  final PiClient _client;
  final LocalStore _store;

  List<Alarm> _alarms = [];
  List<PendingChange> _pending = [];
  bool _syncing = false;
  bool _wasConnected = false;

  /// The alarms as the user should see them, sorted by time.
  List<Alarm> get alarms {
    final sorted = List.of(_alarms);
    sorted.sort((a, b) =>
        (a.hour * 60 + a.minute).compareTo(b.hour * 60 + b.minute));
    return sorted;
  }

  /// How many changes are waiting for the Pi.
  int get pendingCount => _pending.length;

  bool get isSyncing => _syncing;

  /// The last change the Pi refused, for showing to the user once.
  String? lastRejection;

  /// True if [alarmId] has a change the Pi has not confirmed yet.
  bool isPending(String alarmId) =>
      _pending.any((change) => change.alarmId == alarmId);

  /// Create or update an alarm.
  Future<void> save(Alarm alarm) async {
    _alarms = [
      for (final other in _alarms)
        if (other.id != alarm.id) other,
      alarm,
    ];
    await _enqueue(PendingChange.put(alarm));
  }

  /// Delete an alarm.
  Future<void> delete(String alarmId) async {
    _alarms = [
      for (final other in _alarms)
        if (other.id != alarmId) other,
    ];
    await _enqueue(PendingChange.delete(alarmId));
  }

  Future<void> _enqueue(PendingChange change) async {
    // A newer change to the same alarm replaces an older unsent one.
    _pending = [
      for (final other in _pending)
        if (other.alarmId != change.alarmId) other,
      change,
    ];
    await _persist();
    notifyListeners();
    // Not awaited: saving returns at once, syncing carries on by itself.
    sync();
  }

  /// Send queued changes, then refresh from the Pi. Safe to call at any
  /// time; does nothing if the Pi is not connected or a sync is running.
  Future<void> sync() async {
    if (_syncing || !_client.isConnected) return;
    _syncing = true;
    notifyListeners();
    // Set to false when the Pi could not be reached, so we do not retry in
    // a tight loop.
    var reachedPi = true;
    try {
      while (_pending.isNotEmpty) {
        final change = _pending.first;
        try {
          if (change.isDelete) {
            await _client.request('delete', {'alarm_id': change.alarmId});
          } else {
            await _client.request('put', {'alarm': change.alarm!.toJson()});
          }
        } on PiRejected catch (error) {
          // The Pi will never accept this one; drop it and tell the user.
          lastRejection = error.message;
        }
        // Remove exactly the change we sent. If the user edited the same
        // alarm meanwhile, a newer change has taken its place and stays.
        _pending = [
          for (final other in _pending)
            if (!identical(other, change)) other,
        ];
        await _persist();
        notifyListeners();
      }

      // Everything is delivered: take the Pi's list as the truth.
      final response = await _client.request('list');
      final listed = (response['alarms'] as List)
          .map((item) => Alarm.fromJson(item as Map<String, dynamic>))
          .toList();
      // Unless the user changed something while we were downloading; then
      // keep the local list and let the next round sort it out.
      if (_pending.isEmpty) {
        _alarms = listed;
        await _persist();
      }
    } on PiUnavailable {
      // Out of range again. The queue is kept and sent on the next connect.
      reachedPi = false;
    } catch (error) {
      // Anything unexpected (for example a response we cannot parse) must
      // not crash the app; show it and try again on the next connect.
      reachedPi = false;
      lastRejection = 'Sync failed: $error';
    } finally {
      _syncing = false;
      notifyListeners();
    }
    // Changes made during the sync still need sending.
    if (reachedPi && _pending.isNotEmpty && _client.isConnected) {
      sync();
    }
  }

  void _onClientChanged() {
    final connected = _client.isConnected;
    if (connected && !_wasConnected) {
      sync();
    }
    _wasConnected = connected;
  }

  Future<void> _persist() async {
    await _store.saveAlarms(_alarms);
    await _store.savePending(_pending);
  }

  @override
  void dispose() {
    _client.removeListener(_onClientChanged);
    super.dispose();
  }
}
