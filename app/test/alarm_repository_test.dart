// Tests for the offline queue, using a fake Pi instead of Bluetooth.
import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pi_alarm/ble/pi_client.dart';
import 'package:pi_alarm/data/alarm_repository.dart';
import 'package:pi_alarm/data/local_store.dart';
import 'package:pi_alarm/models/alarm.dart';

/// A pretend Pi: keeps alarms in a map and can be switched "out of range".
class FakePi extends ChangeNotifier implements PiClient {
  bool _connected = false;
  final Map<String, Alarm> alarms = {};
  final List<String> log = [];

  /// Alarm ids the fake refuses, to simulate `invalid_alarm`.
  final Set<String> refuse = {};

  @override
  bool get isConnected => _connected;

  void setConnected(bool connected) {
    _connected = connected;
    notifyListeners();
  }

  @override
  Future<Map<String, dynamic>> request(
    String op, [
    Map<String, dynamic> params = const {},
  ]) async {
    if (!_connected) throw PiUnavailable('out of range');
    log.add(op);
    switch (op) {
      case 'put':
        final alarm = Alarm.fromJson(params['alarm'] as Map<String, dynamic>);
        if (refuse.contains(alarm.id)) {
          throw PiRejected('invalid_alarm', 'refused by the fake');
        }
        alarms[alarm.id] = alarm;
        return {'ok': true};
      case 'delete':
        alarms.remove(params['alarm_id']);
        return {'ok': true};
      case 'list':
        return {
          'ok': true,
          'alarms': alarms.values.map((a) => a.toJson()).toList(),
        };
    }
    throw PiRejected('unknown_op', op);
  }
}

Alarm alarm(String id, {int hour = 7}) =>
    Alarm.blank().copyWith(hour: hour).withId(id);

extension on Alarm {
  /// `copyWith` cannot change the id, so build a new alarm for tests.
  Alarm withId(String id) => Alarm(
        id: id,
        label: label,
        hour: hour,
        minute: minute,
        days: days,
        enabled: enabled,
        steps: steps,
      );
}

/// Let queued futures run.
Future<void> settle() => Future<void>.delayed(Duration.zero);

void main() {
  test('a save while connected reaches the Pi and is no longer pending',
      () async {
    final pi = FakePi()..setConnected(true);
    final repository = AlarmRepository(pi, MemoryStore());

    await repository.save(alarm('a'));
    await settle();

    expect(pi.alarms.keys, ['a']);
    expect(repository.isPending('a'), isFalse);
    expect(repository.pendingCount, 0);
    expect(repository.alarms.single.id, 'a');
  });

  test('a save while out of range is queued, shown, and sent on connect',
      () async {
    final pi = FakePi();
    final store = MemoryStore();
    final repository = AlarmRepository(pi, store);

    await repository.save(alarm('a'));
    await settle();

    // Visible locally and clearly marked as not synced.
    expect(repository.alarms.single.id, 'a');
    expect(repository.isPending('a'), isTrue);
    expect(pi.alarms, isEmpty);
    // The queue survives an app restart.
    expect(store.loadPending().single.alarmId, 'a');

    pi.setConnected(true);
    await settle();

    expect(pi.alarms.keys, ['a']);
    expect(repository.isPending('a'), isFalse);
    expect(store.loadPending(), isEmpty);
  });

  test('a restarted app sends what the previous run queued', () async {
    final pi = FakePi();
    final store = MemoryStore();
    await AlarmRepository(pi, store).save(alarm('a'));

    // "Restart": a new repository over the same store.
    final repository = AlarmRepository(pi, store);
    expect(repository.isPending('a'), isTrue);
    pi.setConnected(true);
    await settle();
    expect(pi.alarms.keys, ['a']);
  });

  test('several edits to one alarm are sent as one change', () async {
    final pi = FakePi();
    final repository = AlarmRepository(pi, MemoryStore());

    await repository.save(alarm('a', hour: 6));
    await repository.save(alarm('a', hour: 8));
    expect(repository.pendingCount, 1);

    pi.setConnected(true);
    await settle();
    expect(pi.log.where((op) => op == 'put').length, 1);
    expect(pi.alarms['a']!.hour, 8);
  });

  test('a delete while out of range is queued too', () async {
    final pi = FakePi()..setConnected(true);
    final repository = AlarmRepository(pi, MemoryStore());
    await repository.save(alarm('a'));
    await settle();

    pi.setConnected(false);
    await repository.delete('a');
    expect(repository.alarms, isEmpty);
    expect(repository.isPending('a'), isTrue);
    expect(pi.alarms.keys, ['a'], reason: 'the Pi has not heard yet');

    pi.setConnected(true);
    await settle();
    expect(pi.alarms, isEmpty);
    expect(repository.pendingCount, 0);
  });

  test('a change the Pi refuses is dropped and reported', () async {
    final pi = FakePi()
      ..setConnected(true)
      ..refuse.add('bad');
    final repository = AlarmRepository(pi, MemoryStore());

    await repository.save(alarm('bad'));
    await repository.save(alarm('good'));
    await settle();

    expect(repository.lastRejection, 'refused by the fake');
    expect(repository.pendingCount, 0, reason: 'must not block the queue');
    // After the refresh the list is what the Pi really has.
    expect(repository.alarms.map((a) => a.id), ['good']);
  });

  test('after syncing, the Pi\'s list replaces the local one', () async {
    final pi = FakePi();
    // An alarm created elsewhere (say with the CLI on the Pi).
    pi.alarms['from-cli'] = alarm('from-cli', hour: 5);
    final repository = AlarmRepository(pi, MemoryStore());

    pi.setConnected(true);
    await settle();
    expect(repository.alarms.map((a) => a.id), ['from-cli']);
  });

  test('alarms are listed in time order', () async {
    final pi = FakePi()..setConnected(true);
    final repository = AlarmRepository(pi, MemoryStore());
    await repository.save(alarm('late', hour: 9));
    await repository.save(alarm('early', hour: 5));
    await settle();
    expect(repository.alarms.map((a) => a.id), ['early', 'late']);
  });
}
