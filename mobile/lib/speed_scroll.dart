import 'dart:math' as math;

/// iPod-style click-wheel acceleration for a sorted name list.
///
/// The first [thresholdItems] rows of a gesture stay item-by-item. Past that,
/// each further [letterStep] of travel is one letter tick — the UI fires a
/// haptic and jumps once per tick. A flick stores velocity so ticks keep
/// firing after lift-off, slowing down until the wheel runs out of steam.
class SpeedScroll {
  static const thresholdItems = 20;
  static const rowExtent = 56.0;

  /// Finger travel for the next letter click. A full row so ticks stay
  /// deliberate without a physical click-wheel detent.
  static const letterStep = rowExtent;

  /// Fastest gap between letter ticks (~10 clicks/s).
  static const tickHold = Duration(milliseconds: 100);

  static const thresholdPx = thresholdItems * rowExtent;

  /// Exponential velocity decay while coasting, per second.
  static const decay = 1.15;

  /// Cap so a violent flick cannot scream through the alphabet.
  static const maxVelocity = letterStep * 8;

  /// Lift-off slower than this is a stop, not a spin.
  static const minCoastVelocity = letterStep * 2.5;

  /// Coast ends once a tick would wait longer than about half a second.
  static const stopVelocity = letterStep * 1.8;

  double _travel = 0;
  double _sinceTick = 0;
  double _velocity = 0;
  var _started = false;
  var coasting = false;
  int _dir = 1;
  Duration? _tickedAt;
  Duration? _lastNow;
  int? _pin;
  bool jumping = false;

  bool get active => _started || coasting;

  /// Letter-group start to hold while [jumping], or `null`.
  int? get pin => _pin;

  void begin() {
    _travel = 0;
    _sinceTick = 0;
    _velocity = 0;
    _started = true;
    coasting = false;
    jumping = false;
    _pin = null;
    _tickedAt = null;
    _lastNow = null;
  }

  void ensureBegan() {
    if (!_started) begin();
  }

  void end() {
    _started = false;
    coasting = false;
    jumping = false;
    _pin = null;
    _tickedAt = null;
    _lastNow = null;
    _sinceTick = 0;
    _velocity = 0;
  }

  /// Finger up. Starts a coast when the flick was fast enough.
  bool release([Duration now = Duration.zero]) {
    _noteVelocity(0, now);
    _started = false;
    _sinceTick = 0;
    if (jumping && _velocity.abs() >= minCoastVelocity) {
      coasting = true;
      return true;
    }
    coasting = false;
    jumping = false;
    _pin = null;
    _tickedAt = null;
    return false;
  }

  /// Consume a user-scroll [delta]. [now] is the frame timestamp so holds are
  /// testable. Returns a new pin when a tick fires, otherwise `null`.
  int? addDelta(
    double delta,
    List<String> names,
    int currentIndex, [
    Duration now = Duration.zero,
  ]) {
    if (names.isEmpty || !_started) return null;
    _noteVelocity(delta, now);
    if (delta == 0) return null;

    if (!jumping) {
      _travel += delta;
      if (_travel.abs() < thresholdPx) return null;
      jumping = true;
      _dir = _travel >= 0 ? 1 : -1;
      // Land on the current letter first — jumping straight to the *next*
      // group was the old free-run. The next [letterStep] clicks forward.
      final start = currentIndex.clamp(0, names.length - 1);
      return _tickTo(start, now);
    }

    return _maybeTick(delta, names, currentIndex, now);
  }

  /// Integrate coasting velocity. Returns a pin when a tick fires.
  int? advance(List<String> names, Duration now) {
    if (!coasting || names.isEmpty) return null;
    final last = _lastNow ?? now;
    final dt = (now - last).inMicroseconds / 1e6;
    _lastNow = now;
    if (dt > 0) {
      _velocity *= math.exp(-decay * dt);
      _sinceTick += _velocity * dt;
    }
    if (_velocity.abs() < stopVelocity) {
      coasting = false;
      return null;
    }
    final from = _pin ?? 0;
    return _maybeTick(0, names, from, now, useStoredTravel: true);
  }

  void _noteVelocity(double delta, Duration now) {
    final last = _lastNow;
    _lastNow = now;
    if (last == null) return;
    final dt = (now - last).inMicroseconds / 1e6;
    if (dt <= 0) return;
    if (dt > 0.12) {
      _velocity = 0;
      return;
    }
    final instant = delta / dt;
    _velocity = (_velocity * 0.55) + (instant * 0.45);
    if (_velocity.abs() > maxVelocity) {
      _velocity = maxVelocity * _velocity.sign;
    }
  }

  int? _maybeTick(
    double delta,
    List<String> names,
    int currentIndex,
    Duration now, {
    bool useStoredTravel = false,
  }) {
    if (_holding(now)) return null;
    if (!useStoredTravel) {
      _sinceTick += delta;
    }
    if (_sinceTick.abs() < letterStep) return null;
    _dir = _sinceTick >= 0 ? 1 : -1;
    final from = _pin ?? currentIndex.clamp(0, names.length - 1);
    final target = _next(names, from);
    if (target == null) {
      coasting = false;
      _sinceTick = 0;
      return null;
    }
    return _tickTo(target, now);
  }

  bool _holding(Duration now) =>
      _tickedAt != null && now - _tickedAt! < tickHold;

  int? _tickTo(int? target, Duration now) {
    _sinceTick = 0;
    if (target == null || target == _pin) return null;
    _pin = target;
    _tickedAt = now;
    return target;
  }

  int? _next(List<String> names, int from) =>
      _dir > 0 ? nextLetterIndex(names, from) : prevLetterIndex(names, from);
}

String letterOf(String name) {
  if (name.isEmpty) return ' ';
  return String.fromCharCode(name.runes.first).toUpperCase();
}

int? nextLetterIndex(List<String> names, int selected) {
  if (selected < 0 || selected >= names.length) return null;
  final current = letterOf(names[selected]);
  for (var i = selected + 1; i < names.length; i++) {
    if (letterOf(names[i]) != current) return i;
  }
  return null;
}

int? prevLetterIndex(List<String> names, int selected) {
  if (selected <= 0 || names.isEmpty) return null;
  final current = letterOf(names[selected]);
  var i = selected;
  while (i > 0 && letterOf(names[i - 1]) == current) {
    i--;
  }
  if (i == 0) return null;
  final prev = letterOf(names[i - 1]);
  while (i > 0 && letterOf(names[i - 1]) == prev) {
    i--;
  }
  return i;
}
