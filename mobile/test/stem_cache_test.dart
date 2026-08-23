import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:zytunes_mobile/api/models.dart';
import 'package:zytunes_mobile/stem_cache.dart';

void main() {
  StemFile stem(String kind) => StemFile(
    kind: kind,
    label: kind,
    shortLabel: kind.substring(0, 3),
    url: '/tracks/1/stems/$kind',
  );

  test('downloads missing stems then reuses the files', () async {
    final dir = await Directory.systemTemp.createTemp('zytunes-stem-cache');
    addTearDown(() => dir.delete(recursive: true));
    var gets = 0;
    final cache = DiskStemCache(
      root: dir,
      httpClient: MockClient((req) async {
        gets++;
        return http.Response('flac-${req.url.path}', 200);
      }),
    );
    final stems = [stem('vocals'), stem('drums')];

    final first = await cache.ensure(
      trackId: '1',
      recipe: 'hq-harmony',
      stems: stems,
      resolve: (p) => Uri.parse('http://x$p'),
      headers: const {'Authorization': 'Bearer t'},
    );
    expect(gets, 2);
    expect(
      File.fromUri(first[0]).readAsStringSync(),
      'flac-/tracks/1/stems/vocals',
    );
    expect(
      File.fromUri(first[1]).readAsStringSync(),
      'flac-/tracks/1/stems/drums',
    );

    final second = await cache.ensure(
      trackId: '1',
      recipe: 'hq-harmony',
      stems: stems,
      resolve: (p) => Uri.parse('http://x$p'),
      headers: const {},
    );
    expect(gets, 2);
    expect(second, first);
  });

  test('recipe change does not reuse another recipe\'s files', () async {
    final dir = await Directory.systemTemp.createTemp('zytunes-stem-cache');
    addTearDown(() => dir.delete(recursive: true));
    var gets = 0;
    final cache = DiskStemCache(
      root: dir,
      httpClient: MockClient((req) async {
        gets++;
        return http.Response('x', 200);
      }),
    );
    final stems = [stem('vocals')];

    await cache.ensure(
      trackId: '1',
      recipe: 'demucs',
      stems: stems,
      resolve: (p) => Uri.parse('http://x$p'),
      headers: const {},
    );
    await cache.ensure(
      trackId: '1',
      recipe: 'hq',
      stems: stems,
      resolve: (p) => Uri.parse('http://x$p'),
      headers: const {},
    );
    expect(gets, 2);
  });
}
