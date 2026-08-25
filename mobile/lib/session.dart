import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;

import 'api/client.dart';
import 'api/models.dart';
import 'playback.dart';
import 'playback_keepalive.dart';
import 'stem_cache.dart';
import 'stem_playback.dart';
import 'storage.dart';
import 'theme.dart';

enum SessionPhase { disconnected, connecting, connected }

enum StemPhase { off, separating, caching, active, failed }

const _restartThreshold = Duration(seconds: 3);

/// iTunes-style play threshold: 50% of the track or 4 minutes, whichever
/// first. Same formula as `zytunes::local_plays::play_threshold_ms`.
int playThresholdMs(int durationMs) {
  if (durationMs <= 0) return 0;
  final half = durationMs ~/ 2;
  return half < 240000 ? half : 240000;
}

class Session extends ChangeNotifier {
  Session({
    required http.Client httpClient,
    required this.playback,
    required this.store,
    SettingsStore? settings,
    StemCache? stemCache,
    PlaybackKeepalive? keepalive,
    this.createStemMix,
  }) : _http = httpClient,
       settings = settings ?? MemorySettingsStore(),
       stemCache = stemCache ?? MemoryStemCache(),
       keepalive = keepalive ?? const NoopPlaybackKeepalive() {
    playback.addListener(_onPlaybackTick);
    _completedSub = playback.completed.listen((_) {
      // Defer so ListenableBuilder is not already building from the
      // engine notification that ended the track.
      scheduleMicrotask(_onTrackCompleted);
    });
    _handedOffSub = playback.handedOff.listen((_) {
      scheduleMicrotask(_onHandoff);
    });
  }

  final http.Client _http;
  final Playback playback;
  final CredentialsStore store;
  final SettingsStore settings;
  final StemCache stemCache;
  final PlaybackKeepalive keepalive;
  final StemMix Function()? createStemMix;
  StreamSubscription<void>? _completedSub;
  StreamSubscription<void>? _handedOffSub;
  StreamSubscription<void>? _stemCompletedSub;
  StemPlayback? _stemPlayback;
  Timer? _stemPoll;
  var _stemGen = 0;
  var _searchGen = 0;
  var _connectGen = 0;
  String? _countedPlayId;

  SessionPhase phase = SessionPhase.disconnected;
  bool busy = false;
  String? error;
  SavedServer? saved;
  ZytunesClient? client;
  Duration crossfade = Duration.zero;
  String themeId = defaultThemeId;

  static const crossfadeSteps = [0, 4, 8, 12];

  List<String> artists = const [];
  String? selectedArtist;
  List<AlbumPair> albums = const [];
  AlbumPair? selectedAlbum;
  List<TrackSummary> tracks = const [];
  List<String> searchArtists = const [];
  List<AlbumPair> searchAlbums = const [];
  List<TrackSummary> searchHits = const [];
  TrackSummary? get nowPlaying {
    if (queueIndex < 0 || queueIndex >= queue.length) return null;
    return queue[queueIndex];
  }

  List<TrackSummary> queue = const [];
  int queueIndex = -1;
  int nowPlayingPlayCount = 0;
  String? nowPlayingPlayCountId;

  /// Count for the current track once loaded. Null while the id is stale.
  int? get displayedPlayCount {
    final id = nowPlaying?.id;
    if (id == null || nowPlayingPlayCountId != id) return null;
    return nowPlayingPlayCount;
  }

  StemPhase stemPhase = StemPhase.off;
  StemSetInfo? stemSet;
  List<bool> stemEnabled = const [];
  String? stemError;
  int? stemProgress;

  /// The engine the UI should read: stem mixer while active, otherwise
  /// the file/crossfade player.
  Playback get player {
    final stems = _stemPlayback;
    if (stemPhase == StemPhase.active && stems != null) return stems;
    return playback;
  }

  Duration get displayDuration {
    if (player.duration > Duration.zero) return player.duration;
    final ms = nowPlaying?.durationMs;
    if (ms != null) return Duration(milliseconds: ms);
    return Duration.zero;
  }

  bool get canSeek => player.duration > Duration.zero;

  Future<void> restore() async {
    crossfade = await settings.loadCrossfade();
    playback.crossfade = crossfade;
    themeId = resolveThemeId(await settings.loadThemeId());
    saved = await store.load();
    notifyListeners();
    final next = saved;
    if (next == null) return;
    await connect(host: next.host, port: next.port, token: next.token);
  }

  Future<void> connect({
    required String host,
    required int port,
    String? token,
  }) async {
    final gen = ++_connectGen;
    final stayConnected = phase == SessionPhase.connected;
    busy = true;
    if (!stayConnected) phase = SessionPhase.connecting;
    error = null;
    notifyListeners();

    final url = buildBaseUrl(host, port);
    final trimmedToken = token?.trim();
    final nextToken = (trimmedToken == null || trimmedToken.isEmpty)
        ? null
        : trimmedToken;
    await _remember(
      SavedServer(host: url.host, port: port, token: saved?.token),
    );
    if (gen != _connectGen) return;

    final next = ZytunesClient(
      baseUrl: url,
      token: nextToken,
      httpClient: _http,
    );

    try {
      await next.health();
      if (gen != _connectGen) return;
      final names = await next.artists();
      if (gen != _connectGen) return;
      if (stayConnected && _serverChanged(url, nextToken)) {
        await _dropPlayback();
      }
      if (gen != _connectGen) return;
      client = next;
      artists = names;
      phase = SessionPhase.connected;
      await _remember(
        SavedServer(host: url.host, port: port, token: next.token),
      );
    } on ZytunesAuthException {
      if (gen != _connectGen) return;
      _fail(
        'Wrong token — check Authorization on the server',
        keepClient: stayConnected,
      );
    } catch (_) {
      if (gen != _connectGen) return;
      _fail("Can't reach the server", keepClient: stayConnected);
    } finally {
      if (gen == _connectGen) {
        busy = false;
        notifyListeners();
      }
    }
  }

  /// Drop an in-flight connect so the form can be edited and retried.
  void cancelConnect() {
    if (!busy) return;
    _connectGen++;
    busy = false;
    if (phase == SessionPhase.connecting) {
      phase = SessionPhase.disconnected;
    }
    notifyListeners();
  }

  bool _serverChanged(Uri url, String? token) {
    final current = client;
    if (current == null) return false;
    return current.baseUrl.host != url.host ||
        current.baseUrl.port != url.port ||
        current.token != token;
  }

  Future<void> _remember(SavedServer server) async {
    saved = server;
    await store.save(server);
  }

  Future<void> _dropPlayback() async {
    await _resetStems();
    await playback.stop();
    queue = const [];
    queueIndex = -1;
    nowPlayingPlayCount = 0;
    nowPlayingPlayCountId = null;
    _countedPlayId = null;
  }

  void _fail(String message, {bool keepClient = false}) {
    error = message;
    if (keepClient) return;
    phase = SessionPhase.disconnected;
    client = null;
    artists = const [];
    searchArtists = const [];
    searchAlbums = const [];
    searchHits = const [];
  }

  Future<void> selectArtist(String artist) async {
    final api = client;
    if (api == null) return;
    try {
      final next = await api.albums(artist: artist);
      selectedArtist = artist;
      selectedAlbum = null;
      tracks = const [];
      albums = next;
      error = null;
      notifyListeners();
    } catch (e) {
      debugPrint('selectArtist failed: $e');
      error = "Can't load $artist";
      notifyListeners();
    }
  }

  /// Queues every album by [selectedArtist] (list order, tracks sorted
  /// within each album) and starts at the first track.
  Future<void> playArtist() async {
    final api = client;
    final artist = selectedArtist;
    if (api == null || artist == null || albums.isEmpty) return;
    late final List<TrackSummary> raw;
    try {
      raw = await api.tracks(artist: artist);
    } catch (e) {
      debugPrint('playArtist failed: $e');
      error = "Can't play $artist";
      notifyListeners();
      return;
    }
    final byAlbum = <String, List<TrackSummary>>{};
    for (final track in raw) {
      byAlbum.putIfAbsent(track.album, () => []).add(track);
    }
    final queue = <TrackSummary>[];
    for (final album in albums) {
      final albumTracks = byAlbum.remove(album.album);
      if (albumTracks != null) {
        queue.addAll(sortAlbumTracks(albumTracks));
      }
    }
    for (final leftover in byAlbum.values) {
      queue.addAll(sortAlbumTracks(leftover));
    }
    if (queue.isEmpty) return;
    await play(queue.first, queue: queue);
  }

  Uri? albumArtUri(AlbumPair album) {
    final path = album.artUrl;
    if (path == null || path.isEmpty) return null;
    return client?.resolve(path);
  }

  Future<void> selectAlbum(AlbumPair album) async {
    final api = client;
    if (api == null) return;
    try {
      final raw = await api.tracks(artist: album.artist, album: album.album);
      selectedAlbum = album;
      selectedArtist = album.artist;
      tracks = sortAlbumTracks(raw);
      error = null;
      notifyListeners();
    } catch (e) {
      debugPrint('selectAlbum failed: $e');
      error = "Can't load ${album.album}";
      notifyListeners();
    }
  }

  Future<void> search(String query) async {
    final api = client;
    if (api == null) return;
    final gen = ++_searchGen;
    try {
      final results = await api.search(query);
      if (gen != _searchGen) return;
      searchArtists = results.artists;
      searchAlbums = results.albums;
      searchHits = results.tracks;
      error = null;
      notifyListeners();
    } catch (e) {
      if (gen != _searchGen) return;
      debugPrint('search failed: $e');
      error = "Can't search";
      notifyListeners();
    }
  }

  Future<void> play(TrackSummary track, {List<TrackSummary>? queue}) async {
    final api = client;
    if (api == null) return;
    final next = List<TrackSummary>.from(queue ?? [track]);
    var index = next.indexOf(track);
    if (index < 0) {
      index = next.indexWhere((t) => t.id == track.id);
    }
    if (index < 0) {
      next.insert(0, track);
      index = 0;
    }
    this.queue = next;
    queueIndex = index;
    notifyListeners();
    await _playCurrent();
  }

  Future<void> playNext(TrackSummary track) async {
    if (client == null) return;
    if (queueIndex < 0 || queue.isEmpty) {
      await play(track);
      return;
    }
    final next = List<TrackSummary>.from(queue);
    next.insert(queueIndex + 1, track);
    queue = next;
    notifyListeners();
    await _syncUpcoming();
  }

  Future<void> addToQueue(TrackSummary track) async {
    if (client == null) return;
    if (queueIndex < 0 || queue.isEmpty) {
      await play(track);
      return;
    }
    queue = [...queue, track];
    notifyListeners();
    await _syncUpcoming();
  }

  Future<void> playAt(int index) async {
    if (client == null) return;
    if (index < 0 || index >= queue.length) return;
    queueIndex = index;
    notifyListeners();
    await _playCurrent();
  }

  Future<void> removeFromQueue(int index) async {
    if (index < 0 || index >= queue.length) return;
    final wasCurrent = index == queueIndex;
    final next = List<TrackSummary>.from(queue)..removeAt(index);
    if (index < queueIndex) {
      queueIndex--;
    }
    queue = next;
    if (queue.isEmpty) {
      queueIndex = -1;
      notifyListeners();
      await _resetStems();
      await playback.stop();
      return;
    }
    if (queueIndex >= queue.length) {
      queueIndex = queue.length - 1;
    }
    notifyListeners();
    if (wasCurrent) {
      await _playCurrent();
    } else {
      await _syncUpcoming();
    }
  }

  void moveInQueue(int from, int to) {
    if (from == to) return;
    if (from < 0 || to < 0 || from >= queue.length || to >= queue.length) {
      return;
    }
    final next = List<TrackSummary>.from(queue);
    final item = next.removeAt(from);
    next.insert(to, item);
    if (queueIndex == from) {
      queueIndex = to;
    } else if (from < queueIndex && to >= queueIndex) {
      queueIndex--;
    } else if (from > queueIndex && to <= queueIndex) {
      queueIndex++;
    }
    queue = next;
    notifyListeners();
    unawaited(_syncUpcoming());
  }

  Future<void> _playCurrent() async {
    final api = client;
    final track = nowPlaying;
    if (api == null || track == null) return;
    _countedPlayId = null;
    unawaited(_loadPlayCount(track.id));
    await _resetStems();
    try {
      await playback.play(
        track: track,
        streamUri: api.streamUri(track.id),
        headers: api.headers,
      );
      unawaited(_syncKeepalive());
      await _syncUpcoming();
    } catch (e, st) {
      debugPrint('playback failed: $e\n$st');
      error = "Can't play ${track.name}";
      notifyListeners();
    }
  }

  Future<void> togglePause() async {
    if (nowPlaying == null) return;
    if (player.playing) {
      await player.pause();
    } else {
      await player.resume();
    }
    notifyListeners();
  }

  Future<void> seek(Duration position) async {
    await player.seek(position);
  }

  void _onPlaybackTick() {
    unawaited(_syncKeepalive());
    final id = nowPlaying?.id;
    if (id == null || _countedPlayId == id) return;
    unawaited(_maybeRecordPlay());
  }

  Future<void> _syncKeepalive() {
    final track = nowPlaying;
    return keepalive.sync(
      playing: track != null && player.playing,
      title: track?.name,
      artist: track?.artist,
    );
  }

  void _onTrackCompleted() {
    final id = nowPlaying?.id;
    if (id != null) unawaited(_recordListenFor(id, force: true));
    unawaited(skipNext());
  }

  void _onHandoff() {
    if (queueIndex < 0 || queueIndex + 1 >= queue.length) return;
    final leaving = queue[queueIndex];
    unawaited(_recordListenFor(leaving.id, force: true));
    queueIndex++;
    _countedPlayId = null;
    notifyListeners();
    unawaited(_syncKeepalive());
    final incoming = nowPlaying;
    if (incoming != null) unawaited(_loadPlayCount(incoming.id));
    unawaited(_syncUpcoming());
  }

  Future<void> _loadPlayCount(String id) async {
    final api = client;
    if (api == null) return;
    try {
      final detail = await api.track(id);
      if (nowPlaying?.id != id) return;
      nowPlayingPlayCount = detail.playCount ?? 0;
      nowPlayingPlayCountId = id;
      notifyListeners();
    } catch (e) {
      debugPrint('play count load failed: $e');
    }
  }

  Future<void> _maybeRecordPlay() async {
    final track = nowPlaying;
    if (track == null) return;
    await _recordListenFor(track.id);
  }

  Future<void> _recordListenFor(String id, {bool force = false}) async {
    final api = client;
    if (api == null) return;
    if (_countedPlayId == id) return;
    if (!force) {
      final durationMs = displayDuration.inMilliseconds;
      if (durationMs <= 0) return;
      if (player.position.inMilliseconds < playThresholdMs(durationMs)) {
        return;
      }
    }
    _countedPlayId = id;
    try {
      final recorded = await api.recordPlay(id);
      if (nowPlaying?.id != id) return;
      nowPlayingPlayCount = recorded.playCount;
      nowPlayingPlayCountId = id;
      notifyListeners();
    } catch (e, st) {
      if (_countedPlayId == id) _countedPlayId = null;
      debugPrint('record play failed: $e\n$st');
    }
  }

  Future<void> _syncUpcoming() async {
    final api = client;
    if (api == null || queueIndex < 0 || queueIndex + 1 >= queue.length) {
      await playback.prepareNext(null);
      return;
    }
    final track = queue[queueIndex + 1];
    await playback.prepareNext(
      PreparedSource(
        track: track,
        streamUri: api.streamUri(track.id),
        headers: api.headers,
      ),
    );
  }

  Future<void> setCrossfade(Duration duration) async {
    crossfade = duration < Duration.zero ? Duration.zero : duration;
    playback.crossfade = crossfade;
    await settings.saveCrossfade(crossfade);
    notifyListeners();
  }

  Future<void> setTheme(String id) async {
    themeId = resolveThemeId(id);
    notifyListeners();
    await settings.saveThemeId(themeId);
  }

  Future<void> cycleCrossfade() async {
    final i = crossfadeSteps.indexOf(crossfade.inSeconds);
    final next = crossfadeSteps[(i < 0 ? 0 : i + 1) % crossfadeSteps.length];
    await setCrossfade(Duration(seconds: next));
  }

  Future<void> skipNext() async {
    if (queueIndex < 0 || queueIndex + 1 >= queue.length) return;
    await playAt(queueIndex + 1);
  }

  Future<void> skipPrevious() async {
    if (player.position > _restartThreshold) {
      await player.seek(Duration.zero);
      return;
    }
    if (queueIndex > 0) {
      await playAt(queueIndex - 1);
    } else {
      await player.seek(Duration.zero);
    }
  }

  Uri? artUriFor(TrackSummary track) => client?.artUri(track.id);

  Map<String, String> get authHeaders => client?.headers ?? const {};

  Future<void> toggleStems() async {
    switch (stemPhase) {
      case StemPhase.active:
        await _exitStems();
      case StemPhase.separating:
        await _cancelStemJob();
      case StemPhase.caching:
        await _resetStems();
      case StemPhase.off:
      case StemPhase.failed:
        await _enterStems();
    }
  }

  Future<void> toggleStemAt(int index) async {
    if (stemPhase != StemPhase.active) return;
    final mixer = _stemPlayback;
    if (mixer == null || index < 0 || index >= stemEnabled.length) return;
    await mixer.setEnabled(index, !stemEnabled[index]);
    stemEnabled = List<bool>.from(mixer.enabled);
    notifyListeners();
  }

  Future<void> _enterStems() async {
    final api = client;
    final track = nowPlaying;
    if (api == null || track == null) return;
    stemError = null;
    stemProgress = null;
    stemPhase = StemPhase.separating;
    notifyListeners();
    try {
      var info = await api.stems(track.id);
      if (info.status == StemJobStatus.missing) {
        info = await api.requestStems(track.id);
      }
      await _applyStemInfo(info, track, api);
    } catch (e, st) {
      debugPrint('stems failed: $e\n$st');
      stemPhase = StemPhase.failed;
      stemError = "Can't split ${track.name}";
      notifyListeners();
    }
  }

  Future<void> _applyStemInfo(
    StemSetInfo info,
    TrackSummary track,
    ZytunesClient api,
  ) async {
    stemSet = info;
    stemProgress = info.progress;
    switch (info.status) {
      case StemJobStatus.ready:
        await _startStemPlayback(info, track, api);
      case StemJobStatus.separating:
        stemPhase = StemPhase.separating;
        notifyListeners();
        if (_stemPoll == null || !_stemPoll!.isActive) {
          _pollStems(track, api);
        }
      case StemJobStatus.failed:
        stemPhase = StemPhase.failed;
        stemError = info.error ?? "Can't split ${track.name}";
        _stemPoll?.cancel();
        notifyListeners();
      case StemJobStatus.missing:
        stemPhase = StemPhase.failed;
        stemError = info.engineAvailable
            ? "Can't split ${track.name}"
            : 'Install the stem engine in zytunes-tui first (press M)';
        notifyListeners();
    }
  }

  void _pollStems(TrackSummary track, ZytunesClient api) {
    _stemPoll?.cancel();
    _stemPoll = Timer.periodic(const Duration(seconds: 1), (_) async {
      if (nowPlaying?.id != track.id || stemPhase != StemPhase.separating) {
        _stemPoll?.cancel();
        return;
      }
      try {
        final info = await api.stems(track.id);
        if (nowPlaying?.id != track.id || stemPhase != StemPhase.separating) {
          return;
        }
        await _applyStemInfo(info, track, api);
      } catch (e) {
        debugPrint('stem poll failed: $e');
      }
    });
  }

  Future<void> _startStemPlayback(
    StemSetInfo info,
    TrackSummary track,
    ZytunesClient api,
  ) async {
    _stemPoll?.cancel();
    final factory = createStemMix;
    if (factory == null || info.stems.isEmpty) {
      stemPhase = StemPhase.failed;
      stemError = "Can't mix stems on this player";
      notifyListeners();
      return;
    }
    final gen = ++_stemGen;
    stemSet = info;
    stemPhase = StemPhase.caching;
    stemProgress = 0;
    stemError = null;
    notifyListeners();
    List<Uri> uris;
    try {
      uris = await stemCache.ensure(
        trackId: track.id,
        recipe: info.recipe,
        stems: info.stems,
        resolve: api.resolve,
        headers: api.headers,
        onProgress: (p) {
          if (gen != _stemGen) return;
          stemProgress = p;
          notifyListeners();
        },
      );
    } catch (e, st) {
      if (gen != _stemGen) return;
      debugPrint('stem cache failed: $e\n$st');
      stemPhase = StemPhase.failed;
      stemError = "Can't download stems";
      notifyListeners();
      return;
    }
    if (gen != _stemGen) return;
    final pos = player.position;
    final wasPlaying = player.playing;
    await playback.pause();
    await _disposeStemPlayback();
    final mixer = StemPlayback(factory());
    mixer.addListener(_onPlaybackTick);
    mixer.addListener(notifyListeners);
    _stemCompletedSub = mixer.completed.listen((_) {
      scheduleMicrotask(_onTrackCompleted);
    });
    _stemPlayback = mixer;
    stemEnabled = List<bool>.filled(info.stems.length, true);
    stemSet = info;
    stemPhase = StemPhase.active;
    stemError = null;
    notifyListeners();
    await mixer.playStems(
      track: track,
      uris: uris,
      headers: const {},
      enabled: stemEnabled,
      position: pos,
    );
    if (!wasPlaying) {
      await mixer.pause();
    }
  }

  Future<void> _exitStems() async {
    final api = client;
    final track = nowPlaying;
    final pos = _stemPlayback?.position ?? Duration.zero;
    final wasPlaying = _stemPlayback?.playing ?? false;
    await _resetStems();
    if (api == null || track == null) return;
    await playback.play(
      track: track,
      streamUri: api.streamUri(track.id),
      headers: api.headers,
    );
    if (pos > Duration.zero) {
      await playback.seek(pos);
    }
    if (!wasPlaying) {
      await playback.pause();
    }
    await _syncUpcoming();
  }

  Future<void> _cancelStemJob() async {
    _stemGen++;
    _stemPoll?.cancel();
    final api = client;
    final track = nowPlaying;
    if (api != null && track != null) {
      try {
        await api.cancelStems(track.id);
      } catch (e) {
        debugPrint('cancel stems failed: $e');
      }
    }
    stemPhase = StemPhase.off;
    stemSet = null;
    stemProgress = null;
    stemError = null;
    notifyListeners();
  }

  Future<void> _resetStems() async {
    _stemGen++;
    _stemPoll?.cancel();
    await _disposeStemPlayback();
    final hadStems = stemPhase != StemPhase.off;
    stemPhase = StemPhase.off;
    stemSet = null;
    stemEnabled = const [];
    stemProgress = null;
    stemError = null;
    if (hadStems) notifyListeners();
  }

  Future<void> _disposeStemPlayback() async {
    _stemCompletedSub?.cancel();
    _stemCompletedSub = null;
    final mixer = _stemPlayback;
    _stemPlayback = null;
    if (mixer == null) return;
    mixer.removeListener(_onPlaybackTick);
    mixer.removeListener(notifyListeners);
    await mixer.stop();
    mixer.dispose();
  }

  @override
  void dispose() {
    _stemPoll?.cancel();
    _stemCompletedSub?.cancel();
    _completedSub?.cancel();
    _handedOffSub?.cancel();
    playback.removeListener(_onPlaybackTick);
    _stemPlayback?.dispose();
    unawaited(keepalive.sync(playing: false));
    super.dispose();
  }
}
