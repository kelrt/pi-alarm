import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:pi_alarm/models/alarm.dart';

void main() {
  // The exact JSON the daemon produces for its sample alarm
  // (see `json_layout_is_stable` in src/alarm.rs).
  const daemonJson =
      '{"id":"a1","label":"Work","hour":6,"minute":30,"days":["mon","tue","wed","thu","fri"],"enabled":true,"steps":[{"offset_s":-600,"action":"light_toggle"},{"offset_s":0,"action":"fan_3"}]}';

  test('parses the daemon\'s JSON and writes the same JSON back', () {
    final alarm = Alarm.fromJson(jsonDecode(daemonJson) as Map<String, dynamic>);
    expect(alarm.id, 'a1');
    expect(alarm.timeText, '06:30');
    expect(alarm.daysText, 'Weekdays');
    expect(alarm.steps[0].offsetS, -600);
    expect(alarm.steps[0].offsetText, '10 min before');
    expect(alarm.steps[1].offsetText, 'at alarm time');
    expect(jsonEncode(alarm.toJson()), daemonJson);
  });

  test('a missing label becomes empty', () {
    final alarm = Alarm.fromJson({
      'id': 'x',
      'hour': 7,
      'minute': 5,
      'days': ['sat', 'sun'],
      'enabled': false,
      'steps': <dynamic>[],
    });
    expect(alarm.label, '');
    expect(alarm.daysText, 'Weekends');
    expect(alarm.timeText, '07:05');
  });

  test('day summaries', () {
    final base = Alarm.blank();
    expect(base.copyWith(days: List.of(dayNames)).daysText, 'Every day');
    expect(base.copyWith(days: ['mon', 'wed']).daysText, 'Mon, Wed');
  });

  test('offset text', () {
    expect(const AlarmStep(offsetS: 300, action: 'x').offsetText, '5 min after');
    expect(const AlarmStep(offsetS: 90, action: 'x').offsetText, '90 s after');
    expect(const AlarmStep(offsetS: -45, action: 'x').offsetText, '45 s before');
  });

  test('problems are caught before sending', () {
    final good = Alarm.blank();
    expect(good.problem, isNull);
    expect(good.copyWith(days: []).problem, isNotNull);
    expect(good.copyWith(steps: []).problem, isNotNull);
    expect(
      good.copyWith(steps: [
        const AlarmStep(offsetS: 7 * 3600, action: 'fan_3'),
      ]).problem,
      isNotNull,
    );
  });

  test('new ids fit the daemon\'s rules', () {
    final id = newAlarmId();
    expect(RegExp(r'^[0-9a-f]{8}$').hasMatch(id), isTrue);
    expect(newAlarmId(), isNot(id));
  });
}
