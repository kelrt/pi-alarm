import 'dart:math';

/// Day names exactly as the protocol spells them, Monday first.
const List<String> dayNames = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];

/// Short labels for the day chips in the editor, same order as [dayNames].
const List<String> dayLabels = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

/// One thing an alarm does: run [action] at [offsetS] seconds relative to
/// the alarm time (negative = before).
class AlarmStep {
  const AlarmStep({required this.offsetS, required this.action});

  final int offsetS;
  final String action;

  factory AlarmStep.fromJson(Map<String, dynamic> json) => AlarmStep(
        offsetS: json['offset_s'] as int,
        action: json['action'] as String,
      );

  Map<String, dynamic> toJson() => {'offset_s': offsetS, 'action': action};

  AlarmStep copyWith({int? offsetS, String? action}) => AlarmStep(
        offsetS: offsetS ?? this.offsetS,
        action: action ?? this.action,
      );

  /// "10 min before", "at alarm time", "90 s after".
  String get offsetText {
    if (offsetS == 0) return 'at alarm time';
    final amount = offsetS.abs();
    final when = offsetS < 0 ? 'before' : 'after';
    if (amount % 60 == 0) return '${amount ~/ 60} min $when';
    return '$amount s $when';
  }
}

/// An alarm, in the same shape as the daemon's JSON (see PROTOCOL.md).
class Alarm {
  const Alarm({
    required this.id,
    required this.label,
    required this.hour,
    required this.minute,
    required this.days,
    required this.enabled,
    required this.steps,
  });

  final String id;
  final String label;
  final int hour;
  final int minute;

  /// Entries from [dayNames].
  final List<String> days;
  final bool enabled;
  final List<AlarmStep> steps;

  /// A fresh alarm with sensible defaults for the editor: 07:00 on weekdays,
  /// light ten minutes before, fan at the alarm time.
  factory Alarm.blank() => Alarm(
        id: newAlarmId(),
        label: '',
        hour: 7,
        minute: 0,
        days: const ['mon', 'tue', 'wed', 'thu', 'fri'],
        enabled: true,
        steps: const [
          AlarmStep(offsetS: -600, action: 'light_toggle'),
          AlarmStep(offsetS: 0, action: 'fan_3'),
        ],
      );

  factory Alarm.fromJson(Map<String, dynamic> json) => Alarm(
        id: json['id'] as String,
        label: (json['label'] as String?) ?? '',
        hour: json['hour'] as int,
        minute: json['minute'] as int,
        days: (json['days'] as List).cast<String>(),
        enabled: json['enabled'] as bool,
        steps: (json['steps'] as List)
            .map((step) => AlarmStep.fromJson(step as Map<String, dynamic>))
            .toList(),
      );

  Map<String, dynamic> toJson() => {
        'id': id,
        'label': label,
        'hour': hour,
        'minute': minute,
        'days': days,
        'enabled': enabled,
        'steps': steps.map((step) => step.toJson()).toList(),
      };

  Alarm copyWith({
    String? label,
    int? hour,
    int? minute,
    List<String>? days,
    bool? enabled,
    List<AlarmStep>? steps,
  }) =>
      Alarm(
        id: id,
        label: label ?? this.label,
        hour: hour ?? this.hour,
        minute: minute ?? this.minute,
        days: days ?? this.days,
        enabled: enabled ?? this.enabled,
        steps: steps ?? this.steps,
      );

  /// "06:30".
  String get timeText =>
      '${hour.toString().padLeft(2, '0')}:${minute.toString().padLeft(2, '0')}';

  /// "Every day", "Weekdays", "Weekends" or "Mon, Wed".
  String get daysText {
    final set = days.toSet();
    if (set.length == 7) return 'Every day';
    const weekdays = {'mon', 'tue', 'wed', 'thu', 'fri'};
    const weekend = {'sat', 'sun'};
    if (set.length == 5 && set.containsAll(weekdays)) return 'Weekdays';
    if (set.length == 2 && set.containsAll(weekend)) return 'Weekends';
    final labels = <String>[];
    for (var i = 0; i < dayNames.length; i++) {
      if (set.contains(dayNames[i])) labels.add(dayLabels[i]);
    }
    return labels.join(', ');
  }

  /// Why this alarm cannot be saved, or `null` if it is fine. Mirrors the
  /// daemon's rules so mistakes are caught before a round trip.
  String? get problem {
    if (days.isEmpty) return 'Pick at least one day';
    if (steps.isEmpty) return 'Add at least one step';
    if (steps.length > 8) return 'At most 8 steps';
    if (label.length > 40) return 'Label is too long (40 characters at most)';
    for (final step in steps) {
      if (step.offsetS.abs() > 6 * 3600) {
        return 'A step can be at most 6 hours from the alarm time';
      }
    }
    return null;
  }
}

/// A random 8-character id (a-f, 0-9), which the daemon's id rules allow.
String newAlarmId() {
  final random = Random.secure();
  const digits = '0123456789abcdef';
  return List.generate(8, (_) => digits[random.nextInt(16)]).join();
}
