import 'dart:async';

import 'package:flutter/foundation.dart';

import 'api/models.dart';
import 'playback.dart';

/// Same cap as the TUI mixer (`MAX_STEMS`).
const maxStems = 8;

/// One-clock stem mix. Matches the TUI `StemMixerSource`:
/// short stems pad silence, the mix lasts as long as the longest
/// source, and mute is gain rather than stopping that stem.
abstract class StemMix extends ChangeNotifier {
  var _alive = true;

  @protected
  bool get isMixAlive => _alive;

  @protected
  void notifyIfAlive() {
    if (!_alive) return;
    notifyListeners();
  }

  bool get playing;
  Duration get position;
  Duration get duration;
  List<Uri> get uris;
  List<bool> get enabled;
  List<double> get volumes;
  Stream<void> get completed;

  Future<void> load({
    required List<Uri> uris,
    required List<bool> enabled,
    required Duration position,
  });

  Future<void> setEnabled(int index, bool on);

  Future<void> setMasterVolume(double volume);

  Future<void> pause();

  Future<void> resume();

  Future<void> seek(Duration position);

  Future<void> stop();

  @override
  @mustCallSuper
  void dispose() {
    _alive = false;
    super.dispose();
  }
}

/// In-memory mixer for tests. One playhead; ending a short stem does
/// not complete the mix.
class FakeStemMix extends StemMix {
  FakeStemMix({this.lengths = const []});

  /// Per-stem durations used on the next [load]. Empty means 1 minute each.
  List<Duration> lengths;

  var _master = 1.0;
  final _ended = <bool>[];
  final _completed = StreamController<void>.broadcast(sync: true);

  @override
  var playing = false;

  @override
  var position = Duration.zero;

  @override
  var duration = Duration.zero;

  @override
  var uris = const <Uri>[];

  @override
  var enabled = const <bool>[];

  @override
  List<double> get volumes => [
    for (var i = 0; i < enabled.length; i++) enabled[i] ? _master : 0.0,
  ];

  @override
  Stream<void> get completed => _completed.stream;

  @override
  Future<void> load({
    required List<Uri> uris,
    required List<bool> enabled,
    required Duration position,
  }) async {
    if (uris.isEmpty || uris.length > maxStems) {
      throw ArgumentError(
        'stem mix needs 1..=$maxStems sources, got ${uris.length}',
      );
    }
    this.uris = List<Uri>.from(uris);
    this.enabled = List<bool>.from(enabled);
    final lens = lengths.isEmpty
        ? List<Duration>.filled(uris.length, const Duration(minutes: 1))
        : List<Duration>.from(lengths);
    if (lens.length != uris.length) {
      throw ArgumentError('stem lengths ${lens.length} != uris ${uris.length}');
    }
    duration = lens.fold(Duration.zero, (a, b) => a > b ? a : b);
    this.position = position;
    _ended
      ..clear()
      ..addAll(List<bool>.filled(uris.length, false));
    playing = true;
    notifyIfAlive();
  }

  /// Mark stem [index] exhausted. The mix completes only when every stem
  /// has ended — a short stem never truncates the track.
  void endStem(int index) {
    if (index < 0 || index >= _ended.length || _ended[index]) return;
    _ended[index] = true;
    if (_ended.every((e) => e)) {
      playing = false;
      _completed.add(null);
      notifyIfAlive();
    }
  }

  @override
  Future<void> setEnabled(int index, bool on) async {
    if (index < 0 || index >= enabled.length) return;
    if (enabled[index] == on) return;
    enabled = [...enabled]..[index] = on;
    notifyIfAlive();
  }

  @override
  Future<void> setMasterVolume(double volume) async {
    _master = volume.clamp(0.0, 1.0);
    notifyIfAlive();
  }

  @override
  Future<void> pause() async {
    playing = false;
    notifyIfAlive();
  }

  @override
  Future<void> resume() async {
    playing = true;
    notifyIfAlive();
  }

  @override
  Future<void> seek(Duration position) async {
    this.position = position;
    notifyIfAlive();
  }

  @override
  Future<void> stop() async {
    if (!isMixAlive) return;
    playing = false;
    position = Duration.zero;
    notifyIfAlive();
  }

  @override
  void dispose() {
    _completed.close();
    super.dispose();
  }
}

/// [Playback] facade over a [StemMix]. Production uses SoLoud; tests use
/// [FakeStemMix].
class StemPlayback extends Playback {
  StemPlayback(this.mix) {
    mix.addListener(_onMix);
    _sub = mix.completed.listen((_) => _completed.add(null));
  }

  final StemMix mix;
  StreamSubscription<void>? _sub;
  final _completed = StreamController<void>.broadcast(sync: true);

  List<bool> get enabled => mix.enabled;

  @override
  bool get playing => mix.playing;

  @override
  Duration get position => mix.position;

  @override
  Duration get duration => mix.duration;

  @override
  Stream<void> get completed => _completed.stream;

  @override
  double get volume =>
      mix.volumes.isEmpty ? 1 : mix.volumes.reduce((a, b) => a > b ? a : b);

  void _onMix() => notifyListeners();

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    throw StateError('StemPlayback.playStems — not play');
  }

  Future<void> playStems({
    required TrackSummary track,
    required List<Uri> uris,
    required Map<String, String> headers,
    required List<bool> enabled,
    Duration position = Duration.zero,
  }) {
    return mix.load(uris: uris, enabled: enabled, position: position);
  }

  Future<void> setEnabled(int index, bool on) => mix.setEnabled(index, on);

  @override
  Future<void> setVolume(double volume) => mix.setMasterVolume(volume);

  @override
  Future<void> pause() => mix.pause();

  @override
  Future<void> resume() => mix.resume();

  @override
  Future<void> seek(Duration position) => mix.seek(position);

  @override
  Future<void> stop() => mix.stop();

  @override
  void dispose() {
    _sub?.cancel();
    mix.removeListener(_onMix);
    mix.dispose();
    _completed.close();
    super.dispose();
  }
}
