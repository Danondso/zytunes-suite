import 'dart:io';

import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'package:shared_preferences/shared_preferences.dart';

import 'app.dart';
import 'crossfade.dart';
import 'just_audio_playback.dart';
import 'media_kit_playback.dart';
import 'playback.dart';
import 'playback_keepalive.dart';
import 'session.dart';
import 'soloud_stem_mix.dart';
import 'stem_cache.dart';
import 'stem_playback.dart';
import 'storage_prefs.dart';

Playback createEngine() {
  // ExoPlayer (just_audio) cannot decode ALAC; mpv can. iOS AVPlayer can.
  if (Platform.isLinux || Platform.isAndroid) {
    return MediaKitPlayback();
  }
  return JustAudioPlayback();
}

Playback createPlayback() {
  return CrossfadePlayback(primary: createEngine(), secondary: createEngine());
}

StemMix createStemMix() => SoloudStemMix();

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final prefs = await SharedPreferences.getInstance();
  final httpClient = http.Client();
  final session = Session(
    httpClient: httpClient,
    playback: createPlayback(),
    store: PersistentCredentialsStore(prefs: prefs),
    settings: PrefsSettingsStore(prefs: prefs),
    stemCache: DiskStemCache(
      root: Directory('${Directory.systemTemp.path}/zytunes-stems'),
      httpClient: httpClient,
    ),
    createStemMix: createStemMix,
    keepalive: Platform.isAndroid
        ? ChannelPlaybackKeepalive()
        : const NoopPlaybackKeepalive(),
  );
  runApp(ZytunesApp(session: session));
  await session.restore();
}
