import 'dart:convert';

import 'package:http/http.dart' as http;

import 'models.dart';

class ZytunesApiException implements Exception {
  ZytunesApiException(this.message, {this.statusCode});

  final String message;
  final int? statusCode;

  @override
  String toString() => message;
}

class ZytunesAuthException extends ZytunesApiException {
  ZytunesAuthException() : super('Unauthorized', statusCode: 401);
}

class ZytunesNotFoundException extends ZytunesApiException {
  ZytunesNotFoundException() : super('Not found', statusCode: 404);
}

/// Build the LAN base URL. The port field always wins over a port pasted
/// into [host]; scheme is always `http`.
///
/// Pass [emulator] on the Android emulator only: loopback becomes
/// `10.0.2.2` so the guest can reach `zytunes-serve` on the host. A
/// physical device must use the machine's LAN IP.
Uri buildBaseUrl(String host, int port, {bool emulator = false}) {
  var trimmed = host.trim();
  if (trimmed.contains('://')) {
    final parsed = Uri.parse(trimmed);
    if (parsed.host.isNotEmpty) {
      trimmed = parsed.host;
    }
  }
  if (trimmed == '0.0.0.0' || trimmed == '::' || trimmed == '[::]') {
    trimmed = '127.0.0.1';
  }
  if (emulator && (trimmed == '127.0.0.1' || trimmed == 'localhost')) {
    trimmed = '10.0.2.2';
  }
  return Uri(scheme: 'http', host: trimmed, port: port);
}

String defaultConnectHostHint({bool android = false}) =>
    android ? '192.168.x.x' : '127.0.0.1';

class ZytunesClient {
  ZytunesClient({
    required this.baseUrl,
    this.token,
    http.Client? httpClient,
    Duration? timeout,
  }) : _http = httpClient ?? http.Client(),
       timeout = timeout ?? defaultTimeout;

  static const defaultTimeout = Duration(seconds: 8);

  final Uri baseUrl;
  final String? token;
  final http.Client _http;
  final Duration timeout;

  Map<String, String> get headers {
    final token = this.token;
    if (token == null || token.isEmpty) return const {};
    return {'Authorization': 'Bearer $token'};
  }

  Uri resolve(String relativePath) => baseUrl.resolve(relativePath);

  Uri streamUri(String id) => resolve('/tracks/$id/stream');

  Uri artUri(String id) => resolve('/tracks/$id/art');

  Uri fileUri(String id) => resolve('/tracks/$id/file');

  Future<void> health() async {
    await _getJson('/health');
  }

  Future<List<String>> artists() async {
    final body = await _getJson('/artists');
    return (body as List<dynamic>).cast<String>();
  }

  Future<List<AlbumPair>> albums({String? artist}) async {
    final query = <String, String>{};
    if (artist != null) {
      query['artist'] = artist;
    }
    final body = await _getJson('/albums', query: query);
    return (body as List<dynamic>)
        .map((e) => AlbumPair.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<List<TrackSummary>> tracks({String? artist, String? album}) async {
    final query = <String, String>{};
    if (artist != null) {
      query['artist'] = artist;
    }
    if (album != null) {
      query['album'] = album;
    }
    final body = await _getJson('/tracks', query: query);
    return (body as List<dynamic>)
        .map((e) => TrackSummary.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<TrackDetail> track(String id) async {
    final body = await _getJson('/tracks/$id');
    return TrackDetail.fromJson(body as Map<String, dynamic>);
  }

  Future<SearchResults> search(String query) async {
    final q = query.trim();
    if (q.isEmpty) return const SearchResults();
    final body = await _getJson('/search', query: {'q': q});
    return SearchResults.fromJson(body);
  }

  Future<PlayRecord> recordPlay(String id) async {
    final url = baseUrl.replace(path: '/tracks/$id/play');
    final response = await _timed(_http.post(url, headers: headers));
    _throwIfBad(response);
    if (response.body.isEmpty) {
      return const PlayRecord(playCount: 0, lastPlayedAtMs: 0);
    }
    return PlayRecord.fromJson(
      jsonDecode(response.body) as Map<String, dynamic>,
    );
  }

  Future<StemSetInfo> stems(String id) async {
    final body = await _getJson('/tracks/$id/stems');
    return StemSetInfo.fromJson(body as Map<String, dynamic>);
  }

  Future<StemSetInfo> requestStems(String id) async {
    return _stemAction(id, 'POST');
  }

  Future<StemSetInfo> cancelStems(String id) async {
    return _stemAction(id, 'DELETE');
  }

  Future<StemSetInfo> _stemAction(String id, String method) async {
    final url = baseUrl.replace(path: '/tracks/$id/stems');
    final response = method == 'DELETE'
        ? await _timed(_http.delete(url, headers: headers))
        : await _timed(_http.post(url, headers: headers));
    _throwIfBad(response);
    return StemSetInfo.fromJson(
      jsonDecode(response.body) as Map<String, dynamic>,
    );
  }

  Future<Object?> _getJson(String path, {Map<String, String>? query}) async {
    final url = baseUrl.replace(
      path: path,
      queryParameters: (query == null || query.isEmpty) ? null : query,
    );
    final response = await _timed(_http.get(url, headers: headers));
    _throwIfBad(response);
    if (response.body.isEmpty) return null;
    return jsonDecode(response.body);
  }

  Future<http.Response> _timed(Future<http.Response> request) {
    return request.timeout(timeout);
  }

  void _throwIfBad(http.Response response) {
    switch (response.statusCode) {
      case 200:
      case 206:
        return;
      case 401:
        throw ZytunesAuthException();
      case 404:
        throw ZytunesNotFoundException();
      default:
        throw ZytunesApiException(
          'HTTP ${response.statusCode}',
          statusCode: response.statusCode,
        );
    }
  }
}
