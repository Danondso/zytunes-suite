import 'package:flutter/services.dart';

/// Keeps the process alive while audio is playing (Android FGS).
abstract class PlaybackKeepalive {
  Future<void> sync({
    required bool playing,
    String? title,
    String? artist,
  });
}

class NoopPlaybackKeepalive implements PlaybackKeepalive {
  const NoopPlaybackKeepalive();

  @override
  Future<void> sync({
    required bool playing,
    String? title,
    String? artist,
  }) async {}
}

/// Method-channel keepalive. [invoke] is injectable for tests.
class ChannelPlaybackKeepalive implements PlaybackKeepalive {
  ChannelPlaybackKeepalive({
    Future<void> Function(String method, [dynamic arguments])? invoke,
  }) : _invoke =
           invoke ??
           ((method, [arguments]) async {
             await _channel.invokeMethod<void>(method, arguments);
           });

  static const channelName = 'zytunes/playback_keepalive';
  static const _channel = MethodChannel(channelName);

  final Future<void> Function(String method, [dynamic arguments]) _invoke;
  var _playing = false;
  String? _title;
  String? _artist;

  @override
  Future<void> sync({
    required bool playing,
    String? title,
    String? artist,
  }) async {
    if (!playing) {
      if (!_playing) return;
      _playing = false;
      _title = null;
      _artist = null;
      await _invoke('stop');
      return;
    }
    if (_playing && _title == title && _artist == artist) return;
    _playing = true;
    _title = title;
    _artist = artist;
    await _invoke('start', {'title': title ?? '', 'artist': artist ?? ''});
  }
}
