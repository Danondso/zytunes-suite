import 'package:audio_session/audio_session.dart';
import 'package:flutter/foundation.dart';

Future<void>? _musicSession;

/// Puts Android/iOS on the music stream so volume keys and output routing
/// work. Safe to call from both playback engines; runs once.
Future<void> ensureMusicAudioSession() {
  return _musicSession ??= () async {
    final session = await AudioSession.instance;
    await session.configure(const AudioSessionConfiguration.music());
    await session.setActive(true);
    try {
      final outs = await session.getDevices(includeInputs: false);
      debugPrint(
        'audio outputs: ${outs.map((d) => '${d.name}(${d.type})').join(', ')}',
      );
    } catch (e) {
      debugPrint('audio outputs unavailable: $e');
    }
  }();
}
