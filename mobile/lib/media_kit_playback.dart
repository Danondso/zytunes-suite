import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:media_kit/media_kit.dart';

import 'api/models.dart';
import 'music_session.dart';
import 'playback.dart';

/// Linux and Android player. just_audio/ExoPlayer has no ALAC decoder
/// (M4A duration/seek work, audio is silent). libmpv does.
class MediaKitPlayback extends Playback {
  MediaKitPlayback() {
    MediaKit.ensureInitialized();
    _player = Player(
      configuration: const PlayerConfiguration(
        vo: 'null',
        title: 'zytunes',
        logLevel: MPVLogLevel.warn,
      ),
    );
    _subs.add(
      _player.stream.playing.listen((value) {
        playing = value;
        notifyListeners();
      }),
    );
    _subs.add(
      _player.stream.position.listen((value) {
        position = value;
        notifyListeners();
      }),
    );
    _subs.add(
      _player.stream.duration.listen((value) {
        duration = value;
        if (value > Duration.zero) _signalReady();
        notifyListeners();
      }),
    );
    _subs.add(
      _player.stream.completed.listen((done) {
        if (done) _completed.add(null);
      }),
    );
    _subs.add(
      _player.stream.log.listen((log) {
        debugPrint('mpv ${log.level} ${log.prefix}: ${log.text}');
      }),
    );
    _subs.add(
      _player.stream.audioDevice.listen((device) {
        debugPrint('mpv audio device: ${device.name} (${device.description})');
      }),
    );
  }

  late final Player _player;
  final _subs = <StreamSubscription<dynamic>>[];
  final _completed = StreamController<void>.broadcast();
  Uri? _loadedUri;
  Future<void>? _aoReady;
  var _ready = Completer<void>()..complete();

  @override
  Future<void> get whenReady => _ready.future;

  void _resetReady() {
    if (_ready.isCompleted) {
      _ready = Completer<void>();
    }
  }

  void _signalReady() {
    if (!_ready.isCompleted) _ready.complete();
  }

  @override
  var playing = false;

  @override
  var position = Duration.zero;

  @override
  var duration = Duration.zero;

  @override
  Stream<void> get completed => _completed.stream;

  NativePlayer? get _native {
    final platform = _player.platform;
    return platform is NativePlayer ? platform : null;
  }

  Future<void> _ensureAo() {
    return _aoReady ??= () async {
      final native = _native;
      if (Platform.isLinux) {
        // PipeWire exposes a PulseAudio socket; libmpv's pulse ao hits it.
        await native?.setProperty('ao', 'pulse');
      } else if (Platform.isAndroid) {
        // media_kit defaults to OpenSL ES, which is exclusive — starting
        // the incoming player stops the outgoing one (a cut, not a fade).
        await native?.setProperty('ao', 'audiotrack');
      }
      await native?.setProperty('audio-exclusive', 'no');
      await native?.setProperty('gapless-audio', 'no');
      await native?.setProperty('audio-display', 'no');
      await native?.setProperty('ytdl', 'no');
      // Accurate seeks so N stem players land on the same sample after
      // a shared seek; keyframe-only seeks are what made stems flam.
      await native?.setProperty('hr-seek', 'yes');
    }();
  }

  Future<void> _configureNative(Map<String, String> headers) async {
    await _ensureAo();
    final native = _native;
    if (headers.isNotEmpty) {
      final fields = headers.entries
          .map((e) => '${e.key}: ${e.value}')
          .join(',');
      await native?.setProperty('http-header-fields', fields);
    }
  }

  Future<void> _open(
    Uri streamUri,
    Map<String, String> headers, {
    required bool play,
  }) async {
    if (_loadedUri == streamUri) {
      await _player.seek(Duration.zero);
      if (play) await _player.play();
      return;
    }
    _resetReady();
    await _player.open(
      Media(
        streamUri.toString(),
        httpHeaders: headers.isEmpty ? null : headers,
      ),
      play: play,
    );
    _loadedUri = streamUri;
    if (_player.state.duration > Duration.zero) _signalReady();
  }

  @override
  Future<void> play({
    required TrackSummary track,
    required Uri streamUri,
    required Map<String, String> headers,
  }) async {
    debugPrint('play $streamUri headers=${headers.keys.toList()}');
    await ensureMusicAudioSession();
    await _configureNative(headers);
    await _open(streamUri, headers, play: true);
    debugPrint(
      'mpv opened playing=${_player.state.playing} '
      'duration=${_player.state.duration} '
      'error wait for logs',
    );
  }

  @override
  Future<void> prepare(PreparedSource source) async {
    await ensureMusicAudioSession();
    await _configureNative(source.headers);
    await _open(source.streamUri, source.headers, play: false);
  }

  @override
  double get volume => _player.state.volume / 100;

  @override
  Future<void> setVolume(double volume) =>
      _player.setVolume((volume * 100).clamp(0, 100));

  @override
  Future<void> pause() => _player.pause();

  @override
  Future<void> resume() => _player.play();

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
