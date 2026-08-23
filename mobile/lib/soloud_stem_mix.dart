import 'dart:async';

import 'package:flutter_soloud/flutter_soloud.dart';

import 'stem_playback.dart';

/// Gain ramp matching the TUI mixer's `RAMP_SECONDS` (10 ms).
const _ramp = Duration(milliseconds: 10);

class SoloudStemMix extends StemMix {
  final _completed = StreamController<void>.broadcast(sync: true);
  final _sources = <AudioSource>[];
  final _handles = <SoundHandle>[];
  final _lengths = <Duration>[];
  StreamSubscription<void>? _endedSub;
  Timer? _clock;
  var _master = 1.0;
  var _longest = 0;

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

  Future<void> _ensureEngine() async {
    final s = SoLoud.instance;
    if (s.isInitialized) return;
    await s.init(
      automaticCleanup: false,
      lowLatency: false,
      sampleRate: 44100,
      bufferSize: 2048,
      androidAAudioAttributes: AndroidAAudioAttributes.mediaMusic,
    );
    s.setMaxActiveVoiceCount(16);
  }

  Future<void> _releaseVoices() async {
    _endedSub?.cancel();
    _endedSub = null;
    _clock?.cancel();
    _clock = null;
    final s = SoLoud.instance;
    if (s.isInitialized) {
      for (final h in _handles) {
        if (s.getIsValidVoiceHandle(h)) {
          await s.stop(h);
        }
      }
      for (final src in _sources) {
        await s.disposeSource(src);
      }
    }
    _handles.clear();
    _sources.clear();
    _lengths.clear();
  }

  void _startClock() {
    _clock?.cancel();
    _clock = Timer.periodic(const Duration(milliseconds: 50), (_) {
      if (!isMixAlive) return;
      final s = SoLoud.instance;
      if (!s.isInitialized || _handles.isEmpty) return;
      final h = _handles[_longest];
      if (!s.getIsValidVoiceHandle(h)) return;
      position = s.getPosition(h);
      notifyIfAlive();
    });
  }

  void _applyVolumes({required bool fade}) {
    final s = SoLoud.instance;
    if (!s.isInitialized) return;
    final vols = volumes;
    for (var i = 0; i < _handles.length; i++) {
      final h = _handles[i];
      if (!s.getIsValidVoiceHandle(h)) continue;
      if (fade) {
        s.fadeVolume(h, vols[i], _ramp);
      } else {
        s.setVolume(h, vols[i]);
      }
    }
  }

  @override
  Future<void> load({
    required List<Uri> uris,
    required List<bool> enabled,
    required Duration position,
  }) async {
    if (!isMixAlive) return;
    if (uris.isEmpty || uris.length > maxStems) {
      throw ArgumentError(
        'stem mix needs 1..=$maxStems sources, got ${uris.length}',
      );
    }
    await _releaseVoices();
    await _ensureEngine();
    final s = SoLoud.instance;
    this.uris = List<Uri>.from(uris);
    this.enabled = List<bool>.from(enabled);

    for (final uri in uris) {
      if (uri.scheme != 'file') {
        throw ArgumentError('stem mix needs local files, got $uri');
      }
      // Memory load so seek is cheap, matching the TUI's seek-then-mix.
      final source = await s.loadFile(uri.toFilePath(), mode: LoadMode.memory);
      _sources.add(source);
      _lengths.add(s.getLength(source));
    }

    duration = _lengths.fold(Duration.zero, (a, b) => a > b ? a : b);
    _longest = 0;
    for (var i = 1; i < _lengths.length; i++) {
      if (_lengths[i] > _lengths[_longest]) _longest = i;
    }

    final vols = volumes;
    for (var i = 0; i < _sources.length; i++) {
      final h = s.play(_sources[i], volume: vols[i], paused: true);
      _handles.add(h);
      if (position > Duration.zero) {
        try {
          s.seek(h, position);
        } on SoLoudException {
          // Past this stem's end: leave it paused (silence pad).
        }
      }
    }
    this.position = position;

    _endedSub = _sources[_longest].allInstancesFinished.listen((_) {
      if (!isMixAlive || _completed.isClosed) return;
      playing = false;
      _clock?.cancel();
      _completed.add(null);
      notifyIfAlive();
    });

    for (final h in _handles) {
      if (s.getIsValidVoiceHandle(h)) s.setPause(h, false);
    }
    playing = true;
    _startClock();
    notifyIfAlive();
  }

  @override
  Future<void> setEnabled(int index, bool on) async {
    if (index < 0 || index >= enabled.length) return;
    if (enabled[index] == on) return;
    enabled = [...enabled]..[index] = on;
    _applyVolumes(fade: true);
    notifyIfAlive();
  }

  @override
  Future<void> setMasterVolume(double volume) async {
    _master = volume.clamp(0.0, 1.0);
    _applyVolumes(fade: false);
    notifyIfAlive();
  }

  @override
  Future<void> pause() async {
    final s = SoLoud.instance;
    if (s.isInitialized) {
      for (final h in _handles) {
        if (s.getIsValidVoiceHandle(h)) s.setPause(h, true);
      }
    }
    playing = false;
    _clock?.cancel();
    notifyIfAlive();
  }

  @override
  Future<void> resume() async {
    final s = SoLoud.instance;
    if (s.isInitialized) {
      for (final h in _handles) {
        if (s.getIsValidVoiceHandle(h)) s.setPause(h, false);
      }
    }
    playing = true;
    _startClock();
    notifyIfAlive();
  }

  @override
  Future<void> seek(Duration position) async {
    final s = SoLoud.instance;
    if (!s.isInitialized || _sources.isEmpty) return;
    final wasPlaying = playing;
    final vols = volumes;
    for (var i = 0; i < _sources.length; i++) {
      var h = i < _handles.length ? _handles[i] : null;
      if (h == null || !s.getIsValidVoiceHandle(h)) {
        h = s.play(_sources[i], volume: vols[i], paused: true);
        if (i < _handles.length) {
          _handles[i] = h;
        } else {
          _handles.add(h);
        }
      } else {
        s.setPause(h, true);
      }
      try {
        s.seek(h, position);
      } on SoLoudException {
        await s.stop(h);
      }
    }
    this.position = position;
    if (wasPlaying) {
      for (final h in _handles) {
        if (s.getIsValidVoiceHandle(h)) s.setPause(h, false);
      }
    }
    notifyIfAlive();
  }

  Future<void> _shutdown() async {
    await _releaseVoices();
    if (SoLoud.instance.isInitialized) {
      await SoLoud.instance.deinitAsync();
    }
  }

  @override
  Future<void> stop() async {
    if (!isMixAlive) return;
    playing = false;
    position = Duration.zero;
    await _shutdown();
    notifyIfAlive();
  }

  @override
  void dispose() {
    unawaited(_shutdown());
    if (!_completed.isClosed) _completed.close();
    super.dispose();
  }
}
