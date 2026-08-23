import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:just_audio/just_audio.dart';

import 'api/models.dart';
import 'music_session.dart';
import 'playback.dart';

AudioPlayer _createPlayer() => AudioPlayer(
  // ExoPlayer can send Authorization itself; the localhost header proxy
  // has been a source of silent-but-playing streams on Android.
  useProxyForRequestHeaders: false,
  androidAudioOffloadPreferences: const AndroidAudioOffloadPreferences(
    audioOffloadMode: AndroidAudioOffloadMode.disabled,
  ),
  // Two engines must mix during a crossfade. Exclusive audio focus on the
  // incoming player would pause the outgoing one (a hard cut).
  handleInterruptions: false,
  handleAudioSessionActivation: false,
  // Re-applying session attributes to a playing ExoPlayer glitches output
  // when the second engine prepares the next track.
  androidApplyAudioAttributes: false,
);

class JustAudioPlayback extends Playback {
  JustAudioPlayback({AudioPlayer? player})
    : _player = player ?? _createPlayer() {
    _subs.add(
      _player.playerStateStream.listen((state) {
        playing = state.playing;
        notifyListeners();
      }),
    );
    _subs.add(
      _player.positionStream.listen((value) {
        position = value;
        notifyListeners();
      }),
    );
    _subs.add(
      _player.durationStream.listen((value) {
        if (value != null) duration = value;
        notifyListeners();
      }),
    );
    _subs.add(
      _player.processingStateStream.listen((state) {
        if (state == ProcessingState.completed) {
          _completed.add(null);
        }
      }),
    );
    _subs.add(
      _player.playbackEventStream.listen((event) {
        if (event.errorCode != null) {
          debugPrint(
            'just_audio error ${event.errorCode}: ${event.errorMessage}',
          );
        }
      }),
    );
  }

  final AudioPlayer _player;
  final _subs = <StreamSubscription<dynamic>>[];
  final _completed = StreamController<void>.broadcast();
  Uri? _loadedUri;

  @override
  var playing = false;

  @override
  var position = Duration.zero;

  @override
  var duration = Duration.zero;

  @override
  Stream<void> get completed => _completed.stream;

  Future<void> _ensureMusicSession() => ensureMusicAudioSession();

  Future<void> _load(Uri streamUri, Map<String, String> headers) async {
    if (_loadedUri == streamUri &&
        _player.processingState != ProcessingState.idle) {
      await _player.seek(Duration.zero);
      return;
    }
    await _player.setAudioSource(
      AudioSource.uri(streamUri, headers: headers.isEmpty ? null : headers),
    );
    _loadedUri = streamUri;
  }

  /// [AudioPlayer.play] completes only when playback pauses, stops, or ends.
  /// Kick it and return; completion is observed via [completed].
  void _startPlaying() {
    unawaited(() async {
      try {
        await _player.play();
      } catch (e, st) {
        debugPrint('just_audio play failed: $e\n$st');
      }
    }());
  }

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    await _ensureMusicSession();
    debugPrint('just_audio play $streamUri headers=${headers.keys.toList()}');
    await _load(streamUri, headers);
    _startPlaying();
    debugPrint(
      'just_audio playing=${_player.playing} '
      'state=${_player.processingState} '
      'duration=${_player.duration}',
    );
  }

  @override
  Future<void> prepare(PreparedSource source) async {
    await _ensureMusicSession();
    await _load(source.streamUri, source.headers);
  }

  @override
  double get volume => _player.volume;

  @override
  Future<void> setVolume(double volume) =>
      _player.setVolume(volume.clamp(0.0, 1.0));

  @override
  Future<void> pause() => _player.pause();

  @override
  Future<void> resume() async => _startPlaying();

  @override
  Future<void> seek(Duration position) => _player.seek(position);

  @override
  Future<void> stop() {
    _loadedUri = null;
    return _player.stop();
  }

  @override
  void dispose() {
    for (final sub in _subs) {
      sub.cancel();
    }
    _completed.close();
    _player.dispose();
    super.dispose();
  }
}
