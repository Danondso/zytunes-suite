class SavedServer {
  const SavedServer({required this.host, required this.port, this.token});

  final String host;
  final int port;
  final String? token;
}

abstract class CredentialsStore {
  Future<void> save(SavedServer server);
  Future<SavedServer?> load();
  Future<void> clear();
}

class MemoryCredentialsStore implements CredentialsStore {
  SavedServer? value;

  @override
  Future<void> save(SavedServer server) async {
    value = server;
  }

  @override
  Future<SavedServer?> load() async => value;

  @override
  Future<void> clear() async {
    value = null;
  }
}

abstract class SettingsStore {
  Future<Duration> loadCrossfade();
  Future<void> saveCrossfade(Duration duration);
  Future<String> loadThemeId();
  Future<void> saveThemeId(String id);
}

class MemorySettingsStore implements SettingsStore {
  Duration crossfade = Duration.zero;
  String themeId = 'bedfellow-light';

  @override
  Future<Duration> loadCrossfade() async => crossfade;

  @override
  Future<void> saveCrossfade(Duration duration) async {
    crossfade = duration;
  }

  @override
  Future<String> loadThemeId() async => themeId;

  @override
  Future<void> saveThemeId(String id) async {
    themeId = id;
  }
}
