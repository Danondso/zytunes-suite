import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/api/client.dart';
import 'package:zytunes_mobile/api/models.dart';

void main() {
  const base = 'http://10.0.0.8:9847';

  ZytunesClient clientFor(
    Future<http.Response> Function(http.Request) handler, {
    String? token,
    Duration? timeout,
  }) {
    return ZytunesClient(
      baseUrl: Uri.parse(base),
      token: token,
      timeout: timeout,
      httpClient: MockClient((request) async => handler(request)),
    );
  }

  group('auth', () {
    test('omits Authorization when no token is configured', () async {
      http.Request? seen;
      final client = clientFor((req) async {
        seen = req;
        return http.Response('{"ok":true}', 200);
      });
      await client.health();
      expect(seen!.headers['authorization'], isNull);
    });

    test('sends Bearer token on every request when configured', () async {
      http.Request? seen;
      final client = clientFor((req) async {
        seen = req;
        return http.Response('{"ok":true}', 200);
      }, token: 'secret');
      await client.health();
      expect(seen!.headers['authorization'], 'Bearer secret');
    });

    test('401 becomes ZytunesAuthException', () async {
      final client = clientFor((_) async => http.Response('', 401));
      expect(client.health(), throwsA(isA<ZytunesAuthException>()));
    });

    test('a hung request times out', () async {
      final client = clientFor((_) async {
        await Future<void>.delayed(const Duration(milliseconds: 50));
        return http.Response('{"ok":true}', 200);
      }, timeout: const Duration(milliseconds: 10));
      expect(client.health(), throwsA(isA<TimeoutException>()));
    });
  });

  group('catalog', () {
    test('GET /artists returns name strings', () async {
      final client = clientFor((req) async {
        expect(req.url.path, '/artists');
        return http.Response(jsonEncode(['Radiohead', 'NIN']), 200);
      });
      expect(await client.artists(), ['Radiohead', 'NIN']);
    });

    test('GET /albums?artist= encodes the filter', () async {
      final client = clientFor((req) async {
        expect(req.url.path, '/albums');
        expect(req.url.queryParameters['artist'], 'Radiohead');
        return http.Response(
          jsonEncode([
            {'artist': 'Radiohead', 'album': 'OK Computer', 'track_count': 12},
          ]),
          200,
        );
      });
      final albums = await client.albums(artist: 'Radiohead');
      expect(albums, hasLength(1));
      expect(albums.first.album, 'OK Computer');
      expect(albums.first.trackCount, 12);
    });

    test('GET /tracks?artist=&album= returns summaries', () async {
      final client = clientFor((req) async {
        expect(req.url.path, '/tracks');
        expect(req.url.queryParameters['artist'], 'Radiohead');
        expect(req.url.queryParameters['album'], 'OK Computer');
        return http.Response(
          jsonEncode([
            {
              'id': 42,
              'name': 'Karma Police',
              'artist': 'Radiohead',
              'album': 'OK Computer',
              'track_number': 1,
              'kind': 'FLAC',
            },
          ]),
          200,
        );
      });
      final tracks = await client.tracks(
        artist: 'Radiohead',
        album: 'OK Computer',
      );
      expect(tracks.single.id, '42');
      expect(tracks.single.kind, 'FLAC');
    });

    test('GET /tracks/{id} returns detail with relative URLs', () async {
      final client = clientFor((req) async {
        expect(req.url.path, '/tracks/42');
        return http.Response(
          jsonEncode({
            'id': 42,
            'name': 'Karma Police',
            'artist': 'Radiohead',
            'album': 'OK Computer',
            'sample_rate': 44100,
            'stream_url': '/tracks/42/stream',
            'file_url': '/tracks/42/file',
            'art_url': '/tracks/42/art',
          }),
          200,
        );
      });
      final detail = await client.track('42');
      expect(detail.sampleRate, 44100);
      expect(detail.streamUrl, '/tracks/42/stream');
    });

    test('GET /search?q= returns ranked hits', () async {
      final client = clientFor((req) async {
        expect(req.url.path, '/search');
        expect(req.url.queryParameters['q'], 'karma');
        return http.Response(
          jsonEncode({
            'artists': ['Radiohead'],
            'albums': [
              {
                'artist': 'Radiohead',
                'album': 'OK Computer',
                'track_count': 12,
              },
            ],
            'tracks': [
              {
                'id': 42,
                'name': 'Karma Police',
                'artist': 'Radiohead',
                'album': 'OK Computer',
              },
            ],
          }),
          200,
        );
      });
      final hits = await client.search('karma');
      expect(hits.tracks.single.name, 'Karma Police');
      expect(hits.artists, ['Radiohead']);
      expect(hits.albums.single.album, 'OK Computer');
    });

    test('empty or whitespace search does not hit the network', () async {
      var calls = 0;
      final client = clientFor((_) async {
        calls++;
        return http.Response('[]', 200);
      });
      final empty = await client.search('');
      final blank = await client.search('   ');
      expect(empty.artists, isEmpty);
      expect(empty.albums, isEmpty);
      expect(empty.tracks, isEmpty);
      expect(blank.tracks, isEmpty);
      expect(calls, 0);
    });

    test('POST /tracks/{id}/play returns the sidecar counts', () async {
      final client = clientFor((req) async {
        expect(req.method, 'POST');
        expect(req.url.path, '/tracks/42/play');
        expect(req.headers['authorization'], 'Bearer secret');
        return http.Response(
          jsonEncode({'play_count': 3, 'last_played_at_ms': 1700000000000}),
          200,
        );
      }, token: 'secret');
      final recorded = await client.recordPlay('42');
      expect(recorded.playCount, 3);
      expect(recorded.lastPlayedAtMs, 1700000000000);
    });

    test('POST /tracks/{id}/play 404 becomes ZytunesNotFoundException', () async {
      final client = clientFor((_) async => http.Response('', 404));
      expect(client.recordPlay('999'), throwsA(isA<ZytunesNotFoundException>()));
    });

    test('404 becomes ZytunesNotFoundException', () async {
      final client = clientFor((_) async => http.Response('', 404));
      expect(client.track('999'), throwsA(isA<ZytunesNotFoundException>()));
    });
  });

  group('urls', () {
    test('resolve joins a relative path onto the base URL', () {
      final client = ZytunesClient(baseUrl: Uri.parse(base));
      expect(
        client.resolve('/tracks/42/stream'),
        Uri.parse('$base/tracks/42/stream'),
      );
    });

    test('streamUri and artUri are derived from the track id', () {
      final client = ZytunesClient(baseUrl: Uri.parse(base));
      expect(client.streamUri('42'), Uri.parse('$base/tracks/42/stream'));
      expect(client.artUri('42'), Uri.parse('$base/tracks/42/art'));
      expect(
        client.streamUri('18446744073709551615'),
        Uri.parse('$base/tracks/18446744073709551615/stream'),
      );
    });
  });

  group('stems', () {
    test('GET /tracks/{id}/stems parses the job payload', () async {
      final client = clientFor((req) async {
        expect(req.method, 'GET');
        expect(req.url.path, '/tracks/42/stems');
        return http.Response(
          jsonEncode({
            'status': 'separating',
            'recipe': 'demucs',
            'layout': ['vocals'],
            'stems': [
              {
                'kind': 'vocals',
                'label': 'Vocals',
                'short_label': 'Voc',
                'url': '/tracks/42/stems/vocals',
              },
            ],
            'progress': 43,
            'engine_available': true,
          }),
          200,
        );
      });
      final info = await client.stems('42');
      expect(info.status, StemJobStatus.separating);
      expect(info.progress, 43);
    });

    test('POST and DELETE hit /tracks/{id}/stems', () async {
      final methods = <String>[];
      final client = clientFor((req) async {
        methods.add(req.method);
        expect(req.url.path, '/tracks/42/stems');
        return http.Response(
          jsonEncode({'status': 'missing', 'recipe': 'demucs'}),
          200,
        );
      });
      await client.requestStems('42');
      await client.cancelStems('42');
      expect(methods, ['POST', 'DELETE']);
    });
  });

  group('buildBaseUrl', () {
    test('builds http://host:port from a bare host', () {
      expect(buildBaseUrl('10.0.0.8', 9847), Uri.parse('http://10.0.0.8:9847'));
    });

    test('strips a pasted scheme and ignores an embedded port', () {
      expect(
        buildBaseUrl('http://10.0.0.8:1234', 9847),
        Uri.parse('http://10.0.0.8:9847'),
      );
    });

    test('rewrites 0.0.0.0 bind address to loopback', () {
      expect(buildBaseUrl('0.0.0.0', 9847), Uri.parse('http://127.0.0.1:9847'));
    });

    test('on the Android emulator, loopback becomes the host alias', () {
      expect(
        buildBaseUrl('127.0.0.1', 9847, emulator: true),
        Uri.parse('http://10.0.2.2:9847'),
      );
      expect(
        buildBaseUrl('localhost', 9847, emulator: true),
        Uri.parse('http://10.0.2.2:9847'),
      );
      expect(
        buildBaseUrl('0.0.0.0', 9847, emulator: true),
        Uri.parse('http://10.0.2.2:9847'),
      );
    });
  });
}
