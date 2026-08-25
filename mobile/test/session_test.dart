import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/api/models.dart';
import 'package:zytunes_mobile/playback.dart';
import 'package:zytunes_mobile/playback_keepalive.dart';
import 'package:zytunes_mobile/session.dart';
import 'package:zytunes_mobile/stem_playback.dart';
import 'package:zytunes_mobile/storage.dart';

class _RecordingKeepalive implements PlaybackKeepalive {
  var playing = false;
  String? title;

  @override
  Future<void> sync({
    required bool playing,
    String? title,
    String? artist,
  }) async {
    this.playing = playing;
    this.title = title;
  }
}

void main() {
  late List<http.Request> requests;
  late MemoryCredentialsStore store;
  late FakePlayback playback;

  http.Response jsonOk(Object body) => http.Response(
    jsonEncode(body),
    200,
    headers: {'content-type': 'application/json'},
  );

  Session sessionWith(
    http.Response Function(http.Request) handler, {
    StemMix Function()? createStemMix,
    int playFailures = 0,
    PlaybackKeepalive? keepalive,
  }) {
    requests = [];
    store = MemoryCredentialsStore();
    playback = FakePlayback();
    var remainingPlayFailures = playFailures;
    return Session(
      httpClient: MockClient((req) async {
        requests.add(req);
        if (req.method == 'POST' && req.url.path.endsWith('/play')) {
          if (remainingPlayFailures > 0) {
            remainingPlayFailures--;
            return http.Response('', 500);
          }
          return jsonOk({'play_count': 5, 'last_played_at_ms': 1700000000000});
        }
        final parts = req.url.path.split('/');
        if (req.method == 'GET' && parts.length == 3 && parts[1] == 'tracks') {
          return jsonOk({
            'id': parts[2],
            'name': '',
            'artist': '',
            'album': '',
            'stream_url': '${req.url.path}/stream',
            'file_url': '${req.url.path}/file',
            'art_url': '${req.url.path}/art',
            'play_count': 4,
          });
        }
        return handler(req);
      }),
      playback: playback,
      store: store,
      createStemMix: createStemMix,
      keepalive: keepalive,
    );
  }

  Map<String, dynamic> summary({
    int id = 42,
    String name = 'Karma Police',
    String artist = 'Radiohead',
    String album = 'OK Computer',
    int? trackNumber = 1,
    int? discNumber,
    int? durationMs,
  }) {
    return {
      'id': id,
      'name': name,
      'artist': artist,
      'album': album,
      'track_number': ?trackNumber,
      'disc_number': ?discNumber,
      'duration_ms': ?durationMs,
    };
  }

  group('connect', () {
    test('probes /health, loads artists, and persists credentials', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') {
          return jsonOk(['NIN', 'Radiohead']);
        }
        fail('unexpected ${req.url}');
      });

      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');

      expect(session.phase, SessionPhase.connected);
      expect(session.artists, ['NIN', 'Radiohead']);
      expect(session.error, isNull);
      expect(requests.first.url.path, '/health');
      expect(requests.first.headers['authorization'], 'Bearer secret');
      expect(store.value, isNotNull);
      expect(store.value!.host, '10.0.0.8');
      expect(store.value!.port, 9847);
      expect(store.value!.token, 'secret');
    });

    test(
      '401 stays disconnected and keeps the host without the token',
      () async {
        final session = sessionWith((_) => http.Response('', 401));

        await session.connect(host: '10.0.0.8', port: 9847, token: 'nope');

        expect(session.phase, SessionPhase.disconnected);
        expect(session.error, contains('token'));
        expect(store.value!.host, '10.0.0.8');
        expect(store.value!.port, 9847);
        expect(store.value!.token, isNull);
      },
    );

    test('network failure stays disconnected and keeps the host', () async {
      final session = sessionWith(
        (_) => throw http.ClientException('Connection refused'),
      );

      await session.connect(host: '10.0.0.8', port: 9847);

      expect(session.phase, SessionPhase.disconnected);
      expect(session.error, contains('reach'));
      expect(store.value!.host, '10.0.0.8');
      expect(store.value!.token, isNull);
    });

    test(
      'failed reconnect keeps the live client and last good token',
      () async {
        var healthy = true;
        final session = sessionWith((req) {
          if (!healthy) throw http.ClientException('Connection refused');
          if (req.url.path == '/health') return jsonOk({'ok': true});
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          fail('unexpected ${req.url}');
        });

        await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
        healthy = false;
        await session.connect(host: '10.0.0.9', port: 9847, token: 'secret');

        expect(session.phase, SessionPhase.connected);
        expect(session.client!.baseUrl.host, '10.0.0.8');
        expect(session.error, contains('reach'));
        expect(store.value!.host, '10.0.0.9');
        expect(store.value!.token, 'secret');
      },
    );

    test('reconnect to a new host swaps the client', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') {
          return jsonOk([req.url.host == '10.0.0.9' ? 'NIN' : 'Radiohead']);
        }
        fail('unexpected ${req.url}');
      });

      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.connect(host: '10.0.0.9', port: 9847, token: 'secret');

      expect(session.phase, SessionPhase.connected);
      expect(session.artists, ['NIN']);
      expect(session.client!.baseUrl.host, '10.0.0.9');
      expect(store.value!.host, '10.0.0.9');
    });

    test('restore reconnects from saved credentials', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      store.value = const SavedServer(
        host: '10.0.0.8',
        port: 9847,
        token: 'secret',
      );

      await session.restore();

      expect(session.phase, SessionPhase.connected);
      expect(session.artists, ['Radiohead']);
      expect(requests.first.headers['authorization'], 'Bearer secret');
    });

    test('restore is a no-op when nothing is saved', () async {
      final session = sessionWith((_) => fail('should not hit the network'));
      await session.restore();
      expect(session.phase, SessionPhase.disconnected);
    });

    test('cancelConnect unsticks a hung probe and ignores a late success', () async {
      final delayed = Completer<http.Response>();
      final session = Session(
        httpClient: MockClient((req) async {
          if (req.url.path == '/health') return delayed.future;
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          fail('unexpected ${req.url}');
        }),
        playback: FakePlayback(),
        store: MemoryCredentialsStore(),
      );
      final connecting = session.connect(host: '10.0.0.8', port: 9847);
      await Future<void>.delayed(Duration.zero);
      expect(session.busy, isTrue);
      expect(session.phase, SessionPhase.connecting);

      session.cancelConnect();
      expect(session.busy, isFalse);
      expect(session.phase, SessionPhase.disconnected);

      delayed.complete(jsonOk({'ok': true}));
      await connecting;
      expect(session.phase, SessionPhase.disconnected);
      expect(session.client, isNull);
      expect(session.busy, isFalse);
    });
  });

  group('browse', () {
    Session connected() {
      return sessionWith((req) {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/albums':
            expect(req.url.queryParameters['artist'], 'Radiohead');
            return jsonOk([
              {'artist': 'Radiohead', 'album': 'OK Computer'},
            ]);
          case '/tracks':
            expect(req.url.queryParameters['artist'], 'Radiohead');
            expect(req.url.queryParameters['album'], 'OK Computer');
            return jsonOk([
              summary(id: 2, name: 'Let Down', trackNumber: 5),
              summary(id: 1, name: 'Airbag', trackNumber: 1),
            ]);
          case '/search':
            expect(req.url.queryParameters['q'], 'karma');
            return jsonOk({
              'artists': ['Karma Collective'],
              'albums': [
                {'artist': 'Band', 'album': 'Karma Sessions', 'track_count': 1},
              ],
              'tracks': [summary()],
            });
          default:
            fail('unexpected ${req.url}');
        }
      });
    }

    test('selectArtist loads that artist\'s albums', () async {
      final session = connected();
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.selectArtist('Radiohead');
      expect(session.selectedArtist, 'Radiohead');
      expect(session.albums.single.album, 'OK Computer');
    });

    test('playArtist queues albums in list order', () async {
      final session = sessionWith((req) {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/albums':
            return jsonOk([
              {'artist': 'Radiohead', 'album': 'OK Computer', 'track_count': 2},
              {'artist': 'Radiohead', 'album': 'Pablo Honey', 'track_count': 1},
            ]);
          case '/tracks':
            expect(req.url.queryParameters['artist'], 'Radiohead');
            expect(req.url.queryParameters.containsKey('album'), isFalse);
            return jsonOk([
              summary(id: 3, name: 'You', album: 'Pablo Honey', trackNumber: 1),
              summary(id: 2, name: 'Let Down', trackNumber: 5),
              summary(id: 1, name: 'Airbag', trackNumber: 1),
            ]);
          default:
            fail('unexpected ${req.url}');
        }
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.selectArtist('Radiohead');
      await session.playArtist();
      expect(session.queue.map((t) => t.name).toList(), [
        'Airbag',
        'Let Down',
        'You',
      ]);
      expect(session.nowPlaying!.name, 'Airbag');
    });

    test(
      'selectAlbum loads tracks filtered by artist and album, sorted',
      () async {
        final session = connected();
        await session.connect(host: '10.0.0.8', port: 9847);
        await session.selectArtist('Radiohead');
        await session.selectAlbum(
          const AlbumPair(artist: 'Radiohead', album: 'OK Computer'),
        );
        expect(session.tracks.map((t) => t.name).toList(), [
          'Airbag',
          'Let Down',
        ]);
        expect(
          requests.where((r) => r.url.path == '/tracks'),
          everyElement(
            predicate<http.Request>(
              (r) =>
                  r.url.queryParameters['artist'] != null &&
                  r.url.queryParameters['album'] != null,
            ),
          ),
        );
      },
    );

    test('search stores ranked hits', () async {
      final session = connected();
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.search('karma');
      expect(session.searchHits.single.name, 'Karma Police');
      expect(session.searchArtists, ['Karma Collective']);
      expect(session.searchAlbums.single.album, 'Karma Sessions');
    });

    test('selectArtist maps HTTP errors onto Session.error', () async {
      final session = sessionWith((req) {
        switch (req.url.path) {
          case '/health':
            return jsonOk({'ok': true});
          case '/artists':
            return jsonOk(['Radiohead']);
          case '/albums':
            return http.Response('', 500);
          default:
            fail('unexpected ${req.url}');
        }
      });
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.selectArtist('Radiohead');
      expect(session.error, "Can't load Radiohead");
      expect(session.albums, isEmpty);
    });

    test('search keeps the latest query when responses arrive late', () async {
      requests = [];
      final delayed = Completer<http.Response>();
      final session = Session(
        httpClient: MockClient((req) async {
          requests.add(req);
          if (req.url.path == '/health') return jsonOk({'ok': true});
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          if (req.url.path == '/search') {
            if (req.url.queryParameters['q'] == 'r') {
              return delayed.future;
            }
            return jsonOk({
              'artists': <String>[],
              'albums': <Map<String, Object>>[],
              'tracks': [summary(name: 'Radiohead')],
            });
          }
          fail('unexpected ${req.url}');
        }),
        playback: FakePlayback(),
        store: MemoryCredentialsStore(),
      );
      await session.connect(host: '10.0.0.8', port: 9847);
      final first = session.search('r');
      await session.search('radio');
      delayed.complete(
        jsonOk({
          'artists': <String>[],
          'albums': <Map<String, Object>>[],
          'tracks': [summary(name: 'Rumble')],
        }),
      );
      await first;
      expect(session.searchHits.single.name, 'Radiohead');
      expect(session.error, isNull);
    });
  });

  group('play', () {
    test(
      'starts playback at the resolved stream URL with auth headers',
      () async {
        final session = sessionWith((req) {
          if (req.url.path == '/health') return jsonOk({'ok': true});
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          fail('unexpected ${req.url}');
        });
        await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');

        final track = TrackSummary.fromJson(summary());
        final queue = [
          track,
          TrackSummary.fromJson(
            summary(id: 43, name: 'Let Down', trackNumber: 5),
          ),
        ];
        await session.play(track, queue: queue);

        expect(session.nowPlaying, same(track));
        expect(session.queue, queue);
        expect(
          playback.lastUri,
          Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
        );
        expect(playback.lastHeaders['Authorization'], 'Bearer secret');
        expect(playback.lastTrack!.id, '42');
      },
    );

    test('play of the current track restarts from the beginning', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final track = TrackSummary.fromJson(summary());
      await session.play(track);
      playback.emulatePosition(const Duration(seconds: 30));
      expect(playback.playCount, 1);

      await session.play(track);
      expect(playback.playCount, 2);
      expect(playback.position, Duration.zero);
      expect(playback.playing, isTrue);
    });

    test('keepalive starts on play and stops on pause', () async {
      final keep = _RecordingKeepalive();
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      }, keepalive: keep);
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.play(TrackSummary.fromJson(summary()));
      expect(keep.playing, isTrue);
      expect(keep.title, 'Karma Police');

      await session.togglePause();
      expect(keep.playing, isFalse);
    });

    test(
      'pause and resume toggle playback without changing the track',
      () async {
        final session = sessionWith((req) {
          if (req.url.path == '/health') return jsonOk({'ok': true});
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          fail('unexpected ${req.url}');
        });
        await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
        final track = TrackSummary.fromJson(summary());
        await session.play(track);

        await session.togglePause();
        expect(playback.playing, isFalse);
        expect(session.nowPlaying!.id, '42');

        await session.togglePause();
        expect(playback.playing, isTrue);
        expect(
          playback.lastUri,
          Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
        );
      },
    );

    test('skipNext plays the following queue item', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final airbag = TrackSummary.fromJson(summary(id: 1, name: 'Airbag'));
      final karma = TrackSummary.fromJson(
        summary(id: 42, name: 'Karma Police'),
      );
      await session.play(airbag, queue: [airbag, karma]);

      await session.skipNext();

      expect(session.nowPlaying!.id, '42');
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
      );
    });

    test('skipNext at end of queue leaves the current track', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final track = TrackSummary.fromJson(summary());
      await session.play(track, queue: [track]);

      await session.skipNext();

      expect(session.nowPlaying!.id, '42');
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
      );
    });

    test('skipPrevious seeks to start when more than 3 seconds in', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final airbag = TrackSummary.fromJson(summary(id: 1, name: 'Airbag'));
      final karma = TrackSummary.fromJson(
        summary(id: 42, name: 'Karma Police'),
      );
      await session.play(karma, queue: [airbag, karma]);
      playback.position = const Duration(seconds: 10);

      await session.skipPrevious();

      expect(session.nowPlaying!.id, '42');
      expect(playback.position, Duration.zero);
    });

    test('skipPrevious near the start plays the previous queue item', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final airbag = TrackSummary.fromJson(summary(id: 1, name: 'Airbag'));
      final karma = TrackSummary.fromJson(
        summary(id: 42, name: 'Karma Police'),
      );
      await session.play(karma, queue: [airbag, karma]);
      playback.position = const Duration(seconds: 1);

      await session.skipPrevious();

      expect(session.nowPlaying!.id, '1');
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/1/stream'),
      );
    });

    test('seek forwards to playback', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.play(TrackSummary.fromJson(summary()));

      await session.seek(const Duration(seconds: 30));

      expect(playback.position, const Duration(seconds: 30));
    });

    test('track completion auto-advances to the next queue item', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      final airbag = TrackSummary.fromJson(summary(id: 1, name: 'Airbag'));
      final karma = TrackSummary.fromJson(
        summary(id: 42, name: 'Karma Police'),
      );
      await session.play(airbag, queue: [airbag, karma]);

      playback.emitCompleted();
      await Future<void>.delayed(Duration.zero);

      expect(session.nowPlaying!.id, '42');
    });

    test('cycleCrossfade walks 0/4/8/12 and persists', () async {
      final settings = MemorySettingsStore();
      final session = Session(
        httpClient: MockClient((req) async {
          if (req.url.path == '/health') return jsonOk({'ok': true});
          if (req.url.path == '/artists') return jsonOk(['Radiohead']);
          fail('unexpected ${req.url}');
        }),
        playback: FakePlayback(),
        store: MemoryCredentialsStore(),
        settings: settings,
      );

      expect(session.crossfade, Duration.zero);
      await session.cycleCrossfade();
      expect(session.crossfade, const Duration(seconds: 4));
      expect(settings.crossfade, const Duration(seconds: 4));
      await session.cycleCrossfade();
      expect(session.crossfade.inSeconds, 8);
      await session.cycleCrossfade();
      expect(session.crossfade.inSeconds, 12);
      await session.cycleCrossfade();
      expect(session.crossfade, Duration.zero);
    });

    test('restore loads the persisted theme id', () async {
      final settings = MemorySettingsStore()..themeId = 'tokyo-night';
      final session = Session(
        httpClient: MockClient((_) async => http.Response('', 500)),
        playback: FakePlayback(),
        store: MemoryCredentialsStore(),
        settings: settings,
      );

      await session.restore();
      expect(session.themeId, 'tokyo-night');
    });

    test('setTheme persists a catalog id and rejects unknown ids', () async {
      final settings = MemorySettingsStore();
      final session = Session(
        httpClient: MockClient((_) async => http.Response('', 500)),
        playback: FakePlayback(),
        store: MemoryCredentialsStore(),
        settings: settings,
      );

      await session.setTheme('zune-original');
      expect(session.themeId, 'zune-original');
      expect(settings.themeId, 'zune-original');

      await session.setTheme('not-a-theme');
      expect(session.themeId, 'bedfellow-light');
      expect(settings.themeId, 'bedfellow-light');
    });
  });

  group('playThresholdMs', () {
    test('uses half duration for short tracks', () {
      expect(playThresholdMs(180000), 90000);
    });

    test('caps at four minutes', () {
      expect(playThresholdMs(600000), 240000);
      expect(playThresholdMs(480000), 240000);
    });

    test('zero duration is zero', () {
      expect(playThresholdMs(0), 0);
    });
  });

  group('play count', () {
    Future<Session> playing({
      int durationMs = 240000,
      int playFailures = 0,
    }) async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      }, playFailures: playFailures);
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.play(
        TrackSummary.fromJson(summary(durationMs: durationMs)),
      );
      await Future<void>.delayed(Duration.zero);
      requests.removeWhere(
        (r) => r.method == 'POST' && r.url.path.endsWith('/play'),
      );
      return session;
    }

    List<http.Request> playPosts() => requests
        .where((r) => r.method == 'POST' && r.url.path.endsWith('/play'))
        .toList();

    test('play loads the sidecar count', () async {
      final session = await playing();
      expect(session.displayedPlayCount, 4);
    });

    test('recording a play updates the displayed count', () async {
      final session = await playing();
      expect(session.displayedPlayCount, 4);
      playback.emulatePosition(const Duration(milliseconds: 120000));
      await Future<void>.delayed(Duration.zero);
      expect(session.displayedPlayCount, 5);
    });

    test('a failed /play posts again on the next tick', () async {
      final session = await playing(playFailures: 1);
      expect(session.displayedPlayCount, 4);
      playback.emulatePosition(const Duration(milliseconds: 120000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
      expect(session.displayedPlayCount, 4);

      playback.emulatePosition(const Duration(milliseconds: 120001));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(2));
      expect(session.displayedPlayCount, 5);
    });

    test('position at 50% posts /play once', () async {
      await playing();
      playback.emulatePosition(const Duration(milliseconds: 119999));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), isEmpty);

      playback.emulatePosition(const Duration(milliseconds: 120000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
      expect(playPosts().single.url.path, '/tracks/42/play');
      expect(playPosts().single.headers['authorization'], 'Bearer secret');

      playback.emulatePosition(const Duration(milliseconds: 200000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
    });

    test('long tracks post at four minutes, not 50%', () async {
      await playing(durationMs: 1200000);
      playback.emulatePosition(const Duration(milliseconds: 239999));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), isEmpty);

      playback.emulatePosition(const Duration(milliseconds: 240000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
    });

    test('skip before the threshold does not post', () async {
      final session = await playing();
      final next = TrackSummary.fromJson(
        summary(id: 43, name: 'Let Down', trackNumber: 5, durationMs: 240000),
      );
      await session.play(
        TrackSummary.fromJson(summary(durationMs: 240000)),
        queue: [TrackSummary.fromJson(summary(durationMs: 240000)), next],
      );
      requests.removeWhere(
        (r) => r.method == 'POST' && r.url.path.endsWith('/play'),
      );
      playback.emulatePosition(const Duration(seconds: 30));
      await Future<void>.delayed(Duration.zero);
      await session.skipNext();
      expect(playPosts(), isEmpty);
    });

    test('skip after the threshold does not double-count', () async {
      final session = await playing();
      final current = TrackSummary.fromJson(summary(durationMs: 240000));
      final next = TrackSummary.fromJson(
        summary(id: 43, name: 'Let Down', trackNumber: 5, durationMs: 240000),
      );
      await session.play(current, queue: [current, next]);
      requests.removeWhere(
        (r) => r.method == 'POST' && r.url.path.endsWith('/play'),
      );
      playback.emulatePosition(const Duration(milliseconds: 130000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
      await session.skipNext();
      expect(playPosts(), hasLength(1));
    });

    test('completion records a play for short clips', () async {
      await playing(durationMs: 1500);
      playback.emitCompleted();
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
      expect(playPosts().single.url.path, '/tracks/42/play');
    });

    test('completion after a counted play does not post again', () async {
      await playing();
      playback.emulatePosition(const Duration(milliseconds: 130000));
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
      playback.emitCompleted();
      await Future<void>.delayed(Duration.zero);
      expect(playPosts(), hasLength(1));
    });

    test('crossfade handoff records the outgoing track', () async {
      final session = await playing();
      final current = TrackSummary.fromJson(summary(durationMs: 240000));
      final next = TrackSummary.fromJson(
        summary(id: 43, name: 'Let Down', trackNumber: 5, durationMs: 240000),
      );
      await session.play(current, queue: [current, next]);
      requests.removeWhere(
        (r) => r.method == 'POST' && r.url.path.endsWith('/play'),
      );

      playback.emitHandoff();
      await Future<void>.delayed(Duration.zero);

      expect(playPosts(), hasLength(1));
      expect(playPosts().single.url.path, '/tracks/42/play');
      expect(session.nowPlaying!.id, '43');
    });
  });

  group('queue', () {
    TrackSummary track({required int id, required String name}) =>
        TrackSummary.fromJson(summary(id: id, name: name));

    Future<Session> connected() async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        fail('unexpected ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      return session;
    }

    test(
      'playNext inserts after the current track without interrupting',
      () async {
        final session = await connected();
        final airbag = track(id: 1, name: 'Airbag');
        final karma = track(id: 42, name: 'Karma Police');
        final letDown = track(id: 43, name: 'Let Down');
        await session.play(airbag, queue: [airbag, karma]);

        await session.playNext(letDown);

        expect(session.nowPlaying!.id, '1');
        expect(session.queue.map((t) => t.id), ['1', '43', '42']);
        expect(
          playback.lastUri,
          Uri.parse('http://10.0.0.8:9847/tracks/1/stream'),
        );
      },
    );

    test('playNext when idle starts playback', () async {
      final session = await connected();
      final karma = track(id: 42, name: 'Karma Police');

      await session.playNext(karma);

      expect(session.nowPlaying!.id, '42');
      expect(session.queue.single.id, '42');
      expect(playback.playing, isTrue);
    });

    test('addToQueue appends without interrupting', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      final karma = track(id: 42, name: 'Karma Police');
      final letDown = track(id: 43, name: 'Let Down');
      await session.play(airbag, queue: [airbag, karma]);

      await session.addToQueue(letDown);

      expect(session.nowPlaying!.id, '1');
      expect(session.queue.map((t) => t.id), ['1', '42', '43']);
    });

    test('addToQueue when idle starts playback', () async {
      final session = await connected();
      final karma = track(id: 42, name: 'Karma Police');

      await session.addToQueue(karma);

      expect(session.nowPlaying!.id, '42');
      expect(playback.playing, isTrue);
    });

    test(
      'skipNext follows an inserted duplicate by position, not id',
      () async {
        final session = await connected();
        final airbag = track(id: 1, name: 'Airbag');
        final karma = track(id: 42, name: 'Karma Police');
        await session.play(airbag, queue: [airbag, karma]);
        await session.playNext(karma);

        await session.skipNext();

        expect(session.queueIndex, 1);
        expect(session.nowPlaying!.id, '42');
        await session.skipNext();
        expect(session.queueIndex, 2);
        expect(session.nowPlaying!.id, '42');
      },
    );

    test('removeFromQueue drops an upcoming track', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      final karma = track(id: 42, name: 'Karma Police');
      final letDown = track(id: 43, name: 'Let Down');
      await session.play(airbag, queue: [airbag, karma, letDown]);

      await session.removeFromQueue(1);

      expect(session.nowPlaying!.id, '1');
      expect(session.queue.map((t) => t.id), ['1', '43']);
    });

    test('removeFromQueue on the current track plays the next', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      final karma = track(id: 42, name: 'Karma Police');
      await session.play(airbag, queue: [airbag, karma]);

      await session.removeFromQueue(0);

      expect(session.nowPlaying!.id, '42');
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
      );
    });

    test('removeFromQueue on the last remaining track stops', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      await session.play(airbag, queue: [airbag]);

      await session.removeFromQueue(0);

      expect(session.nowPlaying, isNull);
      expect(session.queue, isEmpty);
      expect(playback.playing, isFalse);
    });

    test('moveInQueue reorders and keeps the playing item', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      final karma = track(id: 42, name: 'Karma Police');
      final letDown = track(id: 43, name: 'Let Down');
      await session.play(karma, queue: [airbag, karma, letDown]);

      session.moveInQueue(2, 0);

      expect(session.queue.map((t) => t.id), ['43', '1', '42']);
      expect(session.nowPlaying!.id, '42');
      expect(session.queueIndex, 2);
    });

    test('playAt jumps to a queue index without replacing the queue', () async {
      final session = await connected();
      final airbag = track(id: 1, name: 'Airbag');
      final karma = track(id: 42, name: 'Karma Police');
      await session.play(airbag, queue: [airbag, karma]);

      await session.playAt(1);

      expect(session.nowPlaying!.id, '42');
      expect(session.queue.map((t) => t.id), ['1', '42']);
    });
  });

  group('stems', () {
    Map<String, dynamic> sixStems({
      String id = '42',
      String status = 'ready',
      String? error,
    }) {
      const kinds = ['vocals', 'drums', 'bass', 'guitar', 'piano', 'other'];
      return {
        'status': status,
        'recipe': 'demucs',
        'layout': kinds,
        'stems': [
          for (final k in kinds)
            {
              'kind': k,
              'label': k,
              'short_label': k.substring(0, 3),
              'url': '/tracks/$id/stems/$k',
            },
        ],
        'engine_available': error == null,
        'error': ?error,
      };
    }

    test('toggleStems swaps to local stem files', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        if (req.url.path == '/tracks/42/stems') {
          expect(req.method, 'GET');
          return jsonOk(sixStems());
        }
        fail('unexpected ${req.method} ${req.url}');
      }, createStemMix: FakeStemMix.new);
      await session.connect(host: '10.0.0.8', port: 9847, token: 'secret');
      await session.play(TrackSummary.fromJson(summary()));

      await session.toggleStems();

      expect(session.stemPhase, StemPhase.active);
      expect(session.player, isNot(playback));
      final mix = (session.player as StemPlayback).mix as FakeStemMix;
      expect(mix.uris, hasLength(6));
      expect(mix.uris.first, Uri.parse('file:///stems/42/demucs/vocals.flac'));
      expect(playback.playing, isFalse);
    });

    test('toggleStemAt mutes that stem player', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        if (req.url.path == '/tracks/42/stems') {
          return jsonOk(sixStems());
        }
        fail('unexpected ${req.url}');
      }, createStemMix: FakeStemMix.new);
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.play(TrackSummary.fromJson(summary()));
      await session.toggleStems();

      await session.toggleStemAt(0);

      expect(session.stemEnabled[0], isFalse);
      final mix = (session.player as StemPlayback).mix as FakeStemMix;
      expect(mix.volumes[0], 0);
      expect(mix.volumes[1], 1);
    });

    test('second toggleStems returns to the file stream', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        if (req.url.path == '/tracks/42/stems') {
          return jsonOk(sixStems());
        }
        fail('unexpected ${req.url}');
      }, createStemMix: FakeStemMix.new);
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.play(TrackSummary.fromJson(summary()));
      await session.toggleStems();
      await session.toggleStems();

      expect(session.stemPhase, StemPhase.off);
      expect(session.player, playback);
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/42/stream'),
      );
    });

    test('skipNext while stems are on returns to file playback', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        if (req.url.path == '/tracks/42/stems') {
          return jsonOk(sixStems());
        }
        fail('unexpected ${req.url}');
      }, createStemMix: FakeStemMix.new);
      await session.connect(host: '10.0.0.8', port: 9847);
      final a = TrackSummary.fromJson(summary());
      final b = TrackSummary.fromJson(
        summary(id: 43, name: 'Let Down', trackNumber: 5),
      );
      await session.play(a, queue: [a, b]);
      await session.toggleStems();
      await session.skipNext();

      expect(session.stemPhase, StemPhase.off);
      expect(session.nowPlaying!.id, '43');
      expect(
        playback.lastUri,
        Uri.parse('http://10.0.0.8:9847/tracks/43/stream'),
      );
    });

    test('missing engine surfaces the TUI provision message', () async {
      final session = sessionWith((req) {
        if (req.url.path == '/health') return jsonOk({'ok': true});
        if (req.url.path == '/artists') return jsonOk(['Radiohead']);
        if (req.url.path == '/tracks/42/stems') {
          if (req.method == 'POST') {
            return jsonOk(
              sixStems(
                status: 'failed',
                error: 'stem engine not installed — press M in zytunes-tui once to provision',
              ),
            );
          }
          return jsonOk(sixStems(status: 'missing'));
        }
        fail('unexpected ${req.method} ${req.url}');
      });
      await session.connect(host: '10.0.0.8', port: 9847);
      await session.play(TrackSummary.fromJson(summary()));
      await session.toggleStems();

      expect(session.stemPhase, StemPhase.failed);
      expect(session.stemError, contains('zytunes-tui'));
    });
  });
}
