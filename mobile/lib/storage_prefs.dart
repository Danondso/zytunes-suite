import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'storage.dart';

class PersistentCredentialsStore implements CredentialsStore {
  PersistentCredentialsStore({
    required SharedPreferences prefs,
    FlutterSecureStorage? secure,
  }) : _prefs = prefs,
       _secure = secure ?? const FlutterSecureStorage();

  static const _hostKey = 'server_host';
  static const _portKey = 'server_port';
  static const _tokenKey = 'server_token';

  final SharedPreferences _prefs;
  final FlutterSecureStorage _secure;

  @override
  Future<void> save(SavedServer server) async {
    await _prefs.setString(_hostKey, server.host);
    await _prefs.setInt(_portKey, server.port);
    if (server.token == null || server.token!.isEmpty) {
      await _secure.delete(key: _tokenKey);
    } else {
      await _secure.write(key: _tokenKey, value: server.token);
    }
  }

  @override
  Future<SavedServer?> load() async {
    final host = _prefs.getString(_hostKey);
    final port = _prefs.getInt(_portKey);
    if (host == null || port == null) return null;
    final token = await _secure.read(key: _tokenKey);
    return SavedServer(host: host, port: port, token: token);
  }

  @override
  Future<void> clear() async {
    await _prefs.remove(_hostKey);
    await _prefs.remove(_portKey);
    await _secure.delete(key: _tokenKey);
  }
}

class PrefsSettingsStore implements SettingsStore {
  PrefsSettingsStore({required SharedPreferences prefs}) : _prefs = prefs;

  static const _crossfadeKey = 'crossfade_ms';
  static const defaultCrossfade = Duration(seconds: 4);

  final SharedPreferences _prefs;

  @override
  Future<Duration> loadCrossfade() async {
    final ms = _prefs.getInt(_crossfadeKey);
    if (ms == null) return defaultCrossfade;
    return Duration(milliseconds: ms < 0 ? 0 : ms);
  }

  @override
  Future<void> saveCrossfade(Duration duration) async {
    await _prefs.setInt(_crossfadeKey, duration.inMilliseconds.clamp(0, 60000));
  }
}
