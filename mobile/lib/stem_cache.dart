import 'dart:io';

import 'package:http/http.dart' as http;

import 'api/models.dart';

/// Local copies of server stem FLACs. The mixer needs files on one
/// clock; N HTTP streams (or N independent players) drift.
abstract class StemCache {
  Future<List<Uri>> ensure({
    required String trackId,
    required String recipe,
    required List<StemFile> stems,
    required Uri Function(String url) resolve,
    required Map<String, String> headers,
    void Function(int percent)? onProgress,
  });
}

/// Test double: file URIs, no I/O.
class MemoryStemCache implements StemCache {
  @override
  Future<List<Uri>> ensure({
    required String trackId,
    required String recipe,
    required List<StemFile> stems,
    required Uri Function(String url) resolve,
    required Map<String, String> headers,
    void Function(int percent)? onProgress,
  }) async {
    onProgress?.call(100);
    return [
      for (final s in stems)
        Uri(scheme: 'file', path: '/stems/$trackId/$recipe/${s.kind}.flac'),
    ];
  }
}

class DiskStemCache implements StemCache {
  DiskStemCache({required this.root, required http.Client httpClient})
    : _http = httpClient;

  final Directory root;
  final http.Client _http;

  @override
  Future<List<Uri>> ensure({
    required String trackId,
    required String recipe,
    required List<StemFile> stems,
    required Uri Function(String url) resolve,
    required Map<String, String> headers,
    void Function(int percent)? onProgress,
  }) async {
    if (stems.isEmpty) return const [];
    final dir = Directory('${root.path}/${_safe(trackId)}/${_safe(recipe)}');
    await dir.create(recursive: true);
    final out = List<Uri?>.filled(stems.length, null);
    var done = 0;
    await Future.wait([
      for (var i = 0; i < stems.length; i++)
        () async {
          final dest = File('${dir.path}/${_safe(stems[i].kind)}.flac');
          if (await dest.exists() && await dest.length() > 0) {
            out[i] = dest.uri;
          } else {
            await _download(resolve(stems[i].url), headers, dest);
            out[i] = dest.uri;
          }
          done++;
          onProgress?.call((done * 100 / stems.length).round());
        }(),
    ]);
    return [for (final uri in out) uri!];
  }

  Future<void> _download(
    Uri url,
    Map<String, String> headers,
    File dest,
  ) async {
    final tmp = File('${dest.path}.part');
    if (await tmp.exists()) await tmp.delete();
    final request = http.Request('GET', url);
    request.headers.addAll(headers);
    final response = await _http.send(request);
    if (response.statusCode != 200 && response.statusCode != 206) {
      throw HttpException(
        'stem download HTTP ${response.statusCode}',
        uri: url,
      );
    }
    try {
      await response.stream.pipe(tmp.openWrite());
    } catch (_) {
      if (await tmp.exists()) await tmp.delete();
      rethrow;
    }
    await tmp.rename(dest.path);
  }

  static String _safe(String s) {
    final out = s.replaceAll(RegExp(r'[^A-Za-z0-9._-]'), '_');
    return out.isEmpty ? '_' : out;
  }
}
