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
}

class MemorySettingsStore implements SettingsStore {
  Duration crossfade = Duration.zero;

  @override
  Future<Duration> loadCrossfade() async => crossfade;

  @override
  Future<void> saveCrossfade(Duration duration) async {
    crossfade = duration;
  }
}
