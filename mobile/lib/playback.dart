import 'dart:async';

import 'package:flutter/foundation.dart';

import 'api/models.dart';

class PreparedSource {
  const PreparedSource({
    required this.track,
    required this.streamUri,
    required this.headers,
  });

  final TrackSummary track;
  final Uri streamUri;
  final Map<String, String> headers;
}

abstract class Playback extends ChangeNotifier {
  bool get playing;
  Duration get position;
  Duration get duration;
  Stream<void> get completed;

  /// Fired when a crossfade finishes and the next track is now current.
  /// Session should bump [Session.queueIndex] without calling [play].
  Stream<void> get handedOff => const Stream<void>.empty();

  Duration get crossfade => Duration.zero;

  set crossfade(Duration value) {}

  double get volume => 1;

  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  });

  /// Load [source] without starting playback. Default is a no-op; [play]
  /// will load later.
  Future<void> prepare(PreparedSource source) async {}

  /// Completes when [prepare] or [play] has a seekable source. Engines
  /// that know duration after demux override this; the default is already
  /// complete so fakes and no-ops do not stall a mixer.
  Future<void> get whenReady => Future.value();

  Future<void> prepareNext(PreparedSource? source) async {}

  Future<void> setVolume(double volume) async {}

  Future<void> pause();

  Future<void> resume();

  Future<void> seek(Duration position);

  Future<void> stop();
}

class FakePlayback extends Playback {
  TrackSummary? lastTrack;
  Uri? lastUri;
  Map<String, String> lastHeaders = const {};
  PreparedSource? prepared;
  var playCount = 0;

  @override
  var playing = false;

  @override
  var position = Duration.zero;

  @override
  var duration = Duration.zero;

  @override
  var volume = 1.0;

  final _completed = StreamController<void>.broadcast(sync: true);
  final _handedOff = StreamController<void>.broadcast(sync: true);

  @override
  Stream<void> get completed => _completed.stream;

  @override
  Stream<void> get handedOff => _handedOff.stream;

  void emitCompleted() => _completed.add(null);

  void emitHandoff() => _handedOff.add(null);

  /// Test helper: move the playhead and notify listeners (does not go through
  /// [seek], so a wrapping [CrossfadePlayback] can treat it as natural time).
  void emulatePosition(Duration value) {
    position = value;
    notifyListeners();
  }

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    playCount++;
    lastTrack = track;
    lastUri = streamUri;
    lastHeaders = headers;
    prepared = null;
    playing = true;
    position = Duration.zero;
    duration = Duration(milliseconds: track.durationMs ?? 0);
    notifyListeners();
  }

  @override
  Future<void> prepare(PreparedSource source) async {
    prepared = source;
    lastTrack = source.track;
    lastUri = source.streamUri;
    lastHeaders = source.headers;
    duration = Duration(milliseconds: source.track.durationMs ?? 0);
    position = Duration.zero;
    playing = false;
  }

  @override
  Future<void> setVolume(double volume) async {
    final next = volume.clamp(0.0, 1.0);
    if (this.volume == next) return;
    this.volume = next;
    notifyListeners();
  }

  @override
  Future<void> pause() async {
    playing = false;
    notifyListeners();
  }

  @override
  Future<void> resume() async {
    playing = true;
    notifyListeners();
  }

  @override
  Future<void> seek(Duration position) async {
    this.position = position;
    notifyListeners();
  }

  @override
  Future<void> stop() async {
    playing = false;
    lastTrack = null;
    lastUri = null;
    lastHeaders = const {};
    prepared = null;
    position = Duration.zero;
    notifyListeners();
  }

  @override
  void dispose() {
    _completed.close();
    _handedOff.close();
    super.dispose();
  }
}
