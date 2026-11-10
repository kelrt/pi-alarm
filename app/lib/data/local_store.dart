import 'dart:convert';

import 'package:shared_preferences/shared_preferences.dart';

import '../models/alarm.dart';

/// A change made on the phone that the Pi has not confirmed yet.
class PendingChange {
  PendingChange.put(Alarm this.alarm) : alarmId = alarm.id;
  PendingChange.delete(this.alarmId) : alarm = null;

  final String alarmId;

  /// The alarm to save, or `null` if this change is a delete.
  final Alarm? alarm;

  bool get isDelete => alarm == null;

  factory PendingChange.fromJson(Map<String, dynamic> json) {
    final alarm = json['alarm'];
    if (alarm == null) return PendingChange.delete(json['alarm_id'] as String);
    return PendingChange.put(Alarm.fromJson(alarm as Map<String, dynamic>));
  }

  Map<String, dynamic> toJson() => {
        'alarm_id': alarmId,
        if (alarm != null) 'alarm': alarm!.toJson(),
      };
}

/// Everything the app keeps on the phone between launches.
///
/// The interface exists so tests can use [MemoryStore] instead of real
/// SharedPreferences.
abstract class LocalStore {
  /// Bluetooth address of the Pi we paired with, if any.
  String? get piRemoteId;
  Future<void> setPiRemoteId(String? id);

  /// The alarm list as last seen, so the app shows something offline.
  List<Alarm> loadAlarms();
  Future<void> saveAlarms(List<Alarm> alarms);

  /// Changes waiting to be sent to the Pi, oldest first.
  List<PendingChange> loadPending();
  Future<void> savePending(List<PendingChange> pending);
}

/// [LocalStore] backed by SharedPreferences (a small key-value file).
class PrefsStore implements LocalStore {
  PrefsStore(this._prefs);

  static const _keyRemoteId = 'pi_remote_id';
  static const _keyAlarms = 'alarms_cache';
  static const _keyPending = 'pending_changes';

  final SharedPreferences _prefs;

  static Future<PrefsStore> open() async =>
      PrefsStore(await SharedPreferences.getInstance());

  @override
  String? get piRemoteId => _prefs.getString(_keyRemoteId);

  @override
  Future<void> setPiRemoteId(String? id) async {
    if (id == null) {
      await _prefs.remove(_keyRemoteId);
    } else {
      await _prefs.setString(_keyRemoteId, id);
    }
  }

  @override
  List<Alarm> loadAlarms() => _loadList(_keyAlarms, Alarm.fromJson);

  @override
  Future<void> saveAlarms(List<Alarm> alarms) => _prefs.setString(
      _keyAlarms, jsonEncode(alarms.map((alarm) => alarm.toJson()).toList()));

  @override
  List<PendingChange> loadPending() =>
      _loadList(_keyPending, PendingChange.fromJson);

  @override
  Future<void> savePending(List<PendingChange> pending) => _prefs.setString(
      _keyPending,
      jsonEncode(pending.map((change) => change.toJson()).toList()));

  /// Read a JSON list. Anything unreadable counts as empty: a damaged cache
  /// must never stop the app from starting.
  List<T> _loadList<T>(String key, T Function(Map<String, dynamic>) fromJson) {
    final text = _prefs.getString(key);
    if (text == null) return [];
    try {
      return (jsonDecode(text) as List)
          .map((item) => fromJson(item as Map<String, dynamic>))
          .toList();
    } catch (_) {
      return [];
    }
  }
}

/// [LocalStore] that only lives in memory. For tests.
class MemoryStore implements LocalStore {
  String? _remoteId;
  List<Alarm> _alarms = [];
  List<PendingChange> _pending = [];

  @override
  String? get piRemoteId => _remoteId;

  @override
  Future<void> setPiRemoteId(String? id) async => _remoteId = id;

  @override
  List<Alarm> loadAlarms() => List.of(_alarms);

  @override
  Future<void> saveAlarms(List<Alarm> alarms) async =>
      _alarms = List.of(alarms);

  @override
  List<PendingChange> loadPending() => List.of(_pending);

  @override
  Future<void> savePending(List<PendingChange> pending) async =>
      _pending = List.of(pending);
}
