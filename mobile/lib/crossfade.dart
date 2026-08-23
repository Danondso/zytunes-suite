import 'dart:async';

import 'package:flutter/foundation.dart';

import 'api/models.dart';
import 'playback.dart';

/// Overlaps the end of one engine with the start of another.
///
/// Skip and [play] cut immediately. Seek stays on the current engine and
/// does not itself start a fade.
class CrossfadePlayback extends Playback {
  CrossfadePlayback({
    required Playback primary,
    required Playback secondary,
    Duration crossfade = Duration.zero,
  }) : _primary = primary,
       _secondary = secondary,
       _crossfade = crossfade {
    _primary.addListener(_onEngine);
    _secondary.addListener(_onEngine);
    final first = _primary;
    final second = _secondary;
    _primaryCompleted = first.completed.listen(
      (_) => _onEngineCompleted(first),
    );
    _secondaryCompleted = second.completed.listen(
      (_) => _onEngineCompleted(second),
    );
  }

  Playback _primary;
  Playback _secondary;
  Duration _crossfade;
  PreparedSource? _upcoming;
  Future<void>? _prepareFuture;
  var _fading = false;
  var _startingFade = false;
  var _settingVolume = false;
  var _seeking = false;
  int _fadeTotalMs = 0;
  DateTime? _fadeStartedAt;
  var _fadeElapsed = Duration.zero;
  Timer? _fadeTimer;

  final _completed = StreamController<void>.broadcast(sync: true);
  final _handedOff = StreamController<void>.broadcast(sync: true);
  StreamSubscription<void>? _primaryCompleted;
  StreamSubscription<void>? _secondaryCompleted;

  @override
  var playing = false;

  @override
  var position = Duration.zero;

  @override
  var duration = Duration.zero;

  @override
  Stream<void> get completed => _completed.stream;

  @override
  Stream<void> get handedOff => _handedOff.stream;

  @override
  Duration get crossfade => _crossfade;

  @override
  set crossfade(Duration value) {
    _crossfade = value < Duration.zero ? Duration.zero : value;
  }

  @override
  double get volume => _primary.volume;

  void _onEngine() {
    playing = _primary.playing || (_fading && _secondary.playing);
    // Stay on the outgoing playhead until the fade ends so the scrubber
    // runs to the end of the current track instead of jumping to 0:00.
    position = _primary.position;
    duration = _primary.duration;
    notifyListeners();
    if (_settingVolume) return;
    if (!_fading) {
      unawaited(_maybeStartFade());
    }
  }

  void _onEngineCompleted(Playback source) {
    if (source == _primary && !_fading) {
      _completed.add(null);
    }
    // Outgoing completed during a fade: keep the timer running so the
    // incoming side still ramps up instead of snapping to full volume.
  }

  Future<void> _maybeStartFade() async {
    if (_fading || _startingFade || _seeking) return;
    if (_crossfade <= Duration.zero) return;
    final next = _upcoming;
    if (next == null) return;
    if (!_primary.playing) return;
    final dur = _primary.duration;
    if (dur <= _crossfade) return;
    final remaining = dur - _primary.position;
    if (remaining > _crossfade || remaining <= Duration.zero) return;

    _startingFade = true;
    try {
      await _startFade(next, remaining);
    } catch (e, st) {
      debugPrint('crossfade failed: $e\n$st');
      _fading = false;
    } finally {
      _startingFade = false;
    }
  }

  Future<void> _startFade(PreparedSource next, Duration remaining) async {
    _fading = true;
    _fadeTotalMs = remaining.inMilliseconds.clamp(
      50,
      _crossfade.inMilliseconds,
    );

    try {
      await _prepareFuture;
    } catch (e, st) {
      debugPrint('crossfade prepare failed: $e\n$st');
      _fading = false;
      return;
    }
    _upcoming = null;
    // Yield so this is not running inside a ChangeNotifier listener; the
    // session can then rebuild the now-playing UI on handoff.
    await Future<void>.delayed(Duration.zero);

    debugPrint(
      'crossfade start ${next.track.name} over ${remaining.inMilliseconds}ms',
    );
    _settingVolume = true;
    await _secondary.setVolume(0);
    _settingVolume = false;
    // The next track is already loaded on the idle engine. play() would
    // reload it and, with just_audio, await until that track itself ended.
    await _secondary.resume();
    _settingVolume = true;
    await _secondary.setVolume(0);
    _settingVolume = false;
    // Incoming play() can steal audio focus; put the current track back.
    await _primary.resume();

    // Keep primary as the outgoing track so position/duration (and the
    // now-playing title) stay on the song that's finishing.
    _fadeElapsed = Duration.zero;
    _fadeStartedAt = DateTime.now();
    _fadeTimer?.cancel();
    _fadeTimer = Timer.periodic(
      const Duration(milliseconds: 32),
      (_) => _tickFade(),
    );
    _onEngine();
    _tickFade();
  }

  void _keepOutgoingPlaying() {
    if (!_fading) return;
    if (_primary.playing) return;
    final dur = _primary.duration;
    if (dur > Duration.zero && _primary.position >= dur) return;
    unawaited(_primary.resume());
  }

  void _tickFade() {
    if (!_fading) return;
    final started = _fadeStartedAt;
    if (_fadeTotalMs <= 0) return;
    var elapsed = _fadeElapsed;
    if (started != null) elapsed += DateTime.now().difference(started);
    final t = (elapsed.inMilliseconds / _fadeTotalMs).clamp(0.0, 1.0);
    _keepOutgoingPlaying();
    _settingVolume = true;
    unawaited(_primary.setVolume(1 - t));
    unawaited(_secondary.setVolume(t));
    _settingVolume = false;
    if (t >= 1) {
      unawaited(_finishFade());
    }
  }

  void _stopFadeTimer() {
    _fadeTimer?.cancel();
    _fadeTimer = null;
    _fadeStartedAt = null;
    _fadeElapsed = Duration.zero;
  }

  Future<void> _finishFade() async {
    if (!_fading) return;
    _fading = false;
    _stopFadeTimer();
    final outgoing = _primary;
    _primary = _secondary;
    _secondary = outgoing;
    await _secondary.stop();
    _settingVolume = true;
    await _primary.setVolume(1);
    _settingVolume = false;
    _handedOff.add(null);
    final next = _upcoming;
    if (next != null) {
      _prepareFuture = _secondary.prepare(next);
      await _prepareFuture;
    }
    _onEngine();
  }

  Future<void> _abortFade() async {
    _startingFade = false;
    if (!_fading) return;
    _fading = false;
    _stopFadeTimer();
    await _secondary.stop();
    _settingVolume = true;
    await _primary.setVolume(1);
    _settingVolume = false;
  }

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    await _abortFade();
    _upcoming = null;
    _prepareFuture = null;
    await _secondary.stop();
    await _primary.setVolume(1);
    await _primary.play(track: track, streamUri: streamUri, headers: headers);
    _onEngine();
  }

  @override
  Future<void> prepareNext(PreparedSource? source) async {
    _upcoming = source;
    if (source == null) {
      _prepareFuture = null;
      if (!_fading) await _secondary.stop();
      return;
    }
    if (_fading) return;
    _prepareFuture = _secondary.prepare(source);
    await _prepareFuture;
  }

  @override
  Future<void> setVolume(double volume) => _primary.setVolume(volume);

  @override
  Future<void> pause() async {
    if (_fading && _fadeStartedAt != null) {
      _fadeElapsed += DateTime.now().difference(_fadeStartedAt!);
      _fadeStartedAt = null;
    }
    _fadeTimer?.cancel();
    await _primary.pause();
    if (_fading) await _secondary.pause();
  }

  @override
  Future<void> resume() async {
    await _primary.resume();
    if (_fading) {
      await _secondary.resume();
      _fadeStartedAt = DateTime.now();
      _fadeTimer?.cancel();
      _fadeTimer = Timer.periodic(
        const Duration(milliseconds: 32),
        (_) => _tickFade(),
      );
    }
  }

  @override
  Future<void> seek(Duration position) async {
    await _abortFade();
    _seeking = true;
    try {
      await _primary.seek(position);
    } finally {
      _seeking = false;
    }
  }

  @override
  Future<void> stop() async {
    await _abortFade();
    _upcoming = null;
    await _primary.stop();
    await _secondary.stop();
    _onEngine();
  }

  @override
  void dispose() {
    _fadeTimer?.cancel();
    _primaryCompleted?.cancel();
    _secondaryCompleted?.cancel();
    _primary.removeListener(_onEngine);
    _secondary.removeListener(_onEngine);
    _completed.close();
    _handedOff.close();
    _primary.dispose();
    _secondary.dispose();
    super.dispose();
  }
}
