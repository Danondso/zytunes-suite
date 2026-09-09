import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import '../api/models.dart';
import '../session.dart';
import '../speed_scroll.dart';
import 'connect_screen.dart';
import 'player_screen.dart';

class LibraryScreen extends StatefulWidget {
  const LibraryScreen({super.key, required this.session});

  final Session session;

  @override
  State<LibraryScreen> createState() => _LibraryScreenState();
}

class _LibraryScreenState extends State<LibraryScreen> {
  final _navKey = GlobalKey<NavigatorState>();
  var _playerOpen = false;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Stack(
        children: [
          Navigator(
            key: _navKey,
            onGenerateRoute: (settings) {
              return MaterialPageRoute<void>(
                builder: (_) => _ArtistList(session: widget.session),
                settings: settings,
              );
            },
          ),
          SafeArea(
            child: Align(
              alignment: Alignment.topRight,
              child: IconButton(
                key: const Key('settingsButton'),
                tooltip: 'Server',
                onPressed: () => showServerSettings(context, widget.session),
                icon: const Icon(Icons.settings),
              ),
            ),
          ),
        ],
      ),
      bottomNavigationBar: ListenableBuilder(
        listenable: Listenable.merge([widget.session, widget.session.player]),
        builder: (context, _) {
          final track = widget.session.nowPlaying;
          if (track == null || _playerOpen) return const SizedBox.shrink();
          return _NowPlayingBar(
            session: widget.session,
            track: track,
            onOpen: () async {
              setState(() => _playerOpen = true);
              await showModalBottomSheet<void>(
                context: context,
                isScrollControlled: true,
                builder: (context) => SizedBox(
                  height: MediaQuery.sizeOf(context).height * 0.92,
                  child: PlayerScreen(session: widget.session),
                ),
              );
              if (mounted) setState(() => _playerOpen = false);
            },
          );
        },
      ),
    );
  }
}

class _ArtistList extends StatefulWidget {
  const _ArtistList({required this.session});

  final Session session;

  @override
  State<_ArtistList> createState() => _ArtistListState();
}

class _ArtistListState extends State<_ArtistList>
    with SingleTickerProviderStateMixin {
  final _search = TextEditingController();
  final _scroll = ScrollController();
  final _speed = SpeedScroll();
  Ticker? _coastTicker;
  var _ignoreJump = false;
  String? _letter;

  Duration get _now => SchedulerBinding.instance.currentSystemFrameTimeStamp;

  @override
  void dispose() {
    _coastTicker?.dispose();
    _search.dispose();
    _scroll.dispose();
    super.dispose();
  }

  bool _onArtistScroll(ScrollNotification notification) {
    if (_ignoreJump) return false;
    final names = widget.session.artists;
    if (notification is ScrollStartNotification) {
      _coastTicker?.stop();
      _coastTicker?.dispose();
      _coastTicker = null;
      _speed.begin();
      if (_letter != null) setState(() => _letter = null);
      return false;
    }
    if (notification is ScrollEndNotification) {
      _releaseScrub();
      return false;
    }
    if (notification is ScrollUpdateNotification && names.isNotEmpty) {
      final delta = notification.scrollDelta ?? 0;
      _speed.ensureBegan();
      final currentIndex = (notification.metrics.pixels / SpeedScroll.rowExtent)
          .floor();
      final tick = _speed.addDelta(delta, names, currentIndex, _now);
      if (tick != null) {
        HapticFeedback.selectionClick();
        final idx = tick.clamp(0, names.length - 1);
        final letter = letterOf(names[idx]);
        if (_letter != letter) setState(() => _letter = letter);
        _holdAt(idx);
      } else if (_speed.jumping) {
        // Stay parked on the last click until the next haptic tick.
        _absorb(delta);
      }
    }
    return false;
  }

  void _releaseScrub() {
    if (_speed.coasting) return;
    if (_speed.release(_now)) {
      _cancelBallistic();
      _startCoast();
    } else {
      _stopScrub();
    }
  }

  void _startCoast() {
    _coastTicker?.dispose();
    _coastTicker = createTicker(_onCoastTick)..start();
  }

  void _onCoastTick(Duration _) {
    final names = widget.session.artists;
    if (names.isEmpty) {
      _stopScrub();
      return;
    }
    final tick = _speed.advance(names, _now);
    if (tick != null) {
      HapticFeedback.selectionClick();
      final idx = tick.clamp(0, names.length - 1);
      final letter = letterOf(names[idx]);
      if (_letter != letter) setState(() => _letter = letter);
      _holdAt(idx);
    } else if (!_speed.coasting) {
      _stopScrub();
    }
  }

  void _cancelBallistic() {
    if (!_scroll.hasClients) return;
    _ignoreJump = true;
    _scroll.jumpTo(_scroll.offset);
    _ignoreJump = false;
  }

  void _stopScrub() {
    _coastTicker?.stop();
    _coastTicker?.dispose();
    _coastTicker = null;
    _speed.end();
    if (_letter != null) setState(() => _letter = null);
  }

  void _absorb(double delta) {
    if (!_scroll.hasClients || delta.abs() < 0.5) return;
    _ignoreJump = true;
    _scroll.position.correctBy(-delta);
    _ignoreJump = false;
  }

  void _holdAt(int index) {
    if (!_scroll.hasClients) return;
    final pos = _scroll.position;
    final target = (index * SpeedScroll.rowExtent).clamp(
      0.0,
      pos.maxScrollExtent,
    );
    final correction = target - pos.pixels;
    if (correction.abs() < 0.5) return;
    // jumpTo kills the active drag. correctBy keeps the gesture; we only
    // snap here on a haptic tick, not on every pointer move.
    _ignoreJump = true;
    pos.correctBy(correction);
    pos.notifyListeners();
    _ignoreJump = false;
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.session,
      builder: (context, _) {
        final session = widget.session;
        final querying = _search.text.trim().isNotEmpty;
        return Scaffold(
          appBar: AppBar(
            title: const Text('Artists'),
            actions: const [SizedBox(width: 48)],
          ),
          body: Column(
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
                child: TextField(
                  key: const Key('searchField'),
                  controller: _search,
                  decoration: const InputDecoration(
                    prefixIcon: Icon(Icons.search),
                    hintText: 'Search tracks, artists, albums',
                  ),
                  onChanged: session.search,
                ),
              ),
              if (session.error != null)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
                  child: Text(
                    session.error!,
                    key: const Key('libraryError'),
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                ),
              Expanded(
                child: querying
                    ? ListView(
                        keyboardDismissBehavior:
                            ScrollViewKeyboardDismissBehavior.onDrag,
                        children: [
                          if (session.searchArtists.isNotEmpty) ...[
                            const _SearchSection(title: 'Artists'),
                            for (final artist in session.searchArtists)
                              ListTile(
                                key: Key('searchArtist-$artist'),
                                leading: const Icon(Icons.person_outline),
                                title: Text(artist),
                                onTap: () =>
                                    _openArtist(context, session, artist),
                              ),
                          ],
                          if (session.searchAlbums.isNotEmpty) ...[
                            const _SearchSection(title: 'Albums'),
                            for (final album in session.searchAlbums)
                              ListTile(
                                key: Key(
                                  'searchAlbum-${album.artist}-${album.album}',
                                ),
                                leading: _AlbumCover(
                                  session: session,
                                  album: album,
                                ),
                                title: Text(album.album),
                                subtitle: Text(_albumSubtitle(album)),
                                onTap: () =>
                                    _openAlbum(context, session, album),
                              ),
                          ],
                          if (session.searchHits.isNotEmpty) ...[
                            const _SearchSection(title: 'Tracks'),
                            for (final track in session.searchHits)
                              ListTile(
                                key: Key('searchTrack-${track.id}'),
                                title: Text(track.name),
                                subtitle: Text(
                                  '${track.artist} · ${track.album}',
                                ),
                                onTap: () => session.play(
                                  track,
                                  queue: session.searchHits,
                                ),
                                trailing: _TrackQueueMenu(
                                  session: session,
                                  track: track,
                                  onGoToAlbum: () => _openAlbum(
                                    context,
                                    session,
                                    AlbumPair(
                                      artist: track.artist,
                                      album: track.album,
                                      artUrl: '/tracks/${track.id}/art',
                                    ),
                                  ),
                                  onGoToArtist: () => _openArtist(
                                    context,
                                    session,
                                    track.artist,
                                  ),
                                ),
                              ),
                          ],
                        ],
                      )
                    : Stack(
                        children: [
                          Listener(
                            onPointerUp: (_) => _releaseScrub(),
                            onPointerCancel: (_) => _stopScrub(),
                            child: NotificationListener<ScrollNotification>(
                              onNotification: _onArtistScroll,
                              child: ListView.builder(
                                key: const Key('artistList'),
                                controller: _scroll,
                                itemExtent: SpeedScroll.rowExtent,
                                keyboardDismissBehavior:
                                    ScrollViewKeyboardDismissBehavior.onDrag,
                                itemCount: session.artists.length,
                                itemBuilder: (context, index) {
                                  final artist = session.artists[index];
                                  return ListTile(
                                    title: Text(artist),
                                    onTap: () =>
                                        _openArtist(context, session, artist),
                                  );
                                },
                              ),
                            ),
                          ),
                          if (_letter != null)
                            IgnorePointer(
                              child: Center(
                                child: Material(
                                  key: const Key('artistSpeedScrollLetter'),
                                  color: Theme.of(context)
                                      .colorScheme
                                      .surfaceContainerHigh,
                                  elevation: 8,
                                  borderRadius: BorderRadius.circular(12),
                                  child: Padding(
                                    padding: const EdgeInsets.symmetric(
                                      horizontal: 28,
                                      vertical: 12,
                                    ),
                                    child: Text(
                                      _letter!,
                                      style: Theme.of(context)
                                          .textTheme
                                          .displayLarge,
                                    ),
                                  ),
                                ),
                              ),
                            ),
                        ],
                      ),
              ),
            ],
          ),
        );
      },
    );
  }
}

Future<void> _openArtist(
  BuildContext context,
  Session session,
  String artist,
) async {
  await session.selectArtist(artist);
  if (!context.mounted) return;
  await Navigator.of(
    context,
  ).push(MaterialPageRoute<void>(builder: (_) => _AlbumList(session: session)));
}

Future<void> _openAlbum(
  BuildContext context,
  Session session,
  AlbumPair album,
) async {
  await session.selectAlbum(album);
  if (!context.mounted) return;
  await Navigator.of(
    context,
  ).push(MaterialPageRoute<void>(builder: (_) => _TrackList(session: session)));
}

class _SearchSection extends StatelessWidget {
  const _SearchSection({required this.title});

  final String title;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
      child: Text(
        title,
        key: Key('searchSection-$title'),
        style: Theme.of(context).textTheme.titleSmall,
      ),
    );
  }
}

class _AlbumList extends StatelessWidget {
  const _AlbumList({required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    final artist = session.selectedArtist ?? 'Albums';
    final count = session.albums.length;
    final countLabel = count == 1 ? '1 album' : '$count albums';
    return Scaffold(
      key: const Key('artistScreen'),
      appBar: AppBar(title: Text(artist), actions: const [SizedBox(width: 48)]),
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(artist, style: Theme.of(context).textTheme.headlineSmall),
                const SizedBox(height: 4),
                Text(
                  countLabel,
                  key: const Key('artistAlbumCount'),
                  style: Theme.of(context).textTheme.bodyMedium,
                ),
                const SizedBox(height: 12),
                FilledButton.icon(
                  key: const Key('playArtistButton'),
                  onPressed: session.playArtist,
                  icon: const Icon(Icons.play_arrow),
                  label: const Text('Play'),
                ),
              ],
            ),
          ),
          Expanded(
            child: ListView.builder(
              itemCount: session.albums.length,
              itemBuilder: (context, index) {
                final album = session.albums[index];
                return ListTile(
                  key: Key('albumTile-${album.album}'),
                  leading: _AlbumCover(session: session, album: album),
                  title: Text(album.album),
                  subtitle: Text(_albumSubtitle(album)),
                  onTap: () => _openAlbum(context, session, album),
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

String _albumSubtitle(AlbumPair album) {
  final parts = <String>[];
  if (album.year != null) parts.add('${album.year}');
  if (album.trackCount != null) {
    parts.add(album.trackCount == 1 ? '1 track' : '${album.trackCount} tracks');
  }
  return parts.isEmpty ? album.artist : parts.join(' · ');
}

class _AlbumCover extends StatelessWidget {
  const _AlbumCover({
    super.key,
    required this.session,
    required this.album,
    this.size = 56,
  });

  final Session session;
  final AlbumPair album;
  final double size;

  @override
  Widget build(BuildContext context) {
    final uri = session.albumArtUri(album);
    final fallback = ColoredBox(
      color: Theme.of(context).colorScheme.surfaceContainerHighest,
      child: Icon(Icons.album, size: size * 0.45),
    );
    return ClipRRect(
      borderRadius: BorderRadius.circular(size >= 120 ? 12 : 8),
      child: SizedBox(
        width: size,
        height: size,
        child: uri == null
            ? fallback
            : Image.network(
                uri.toString(),
                headers: session.authHeaders,
                fit: BoxFit.cover,
                errorBuilder: (context, error, stack) => fallback,
              ),
      ),
    );
  }
}

class _TrackList extends StatelessWidget {
  const _TrackList({required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    final album = session.selectedAlbum;
    final coverSize = (MediaQuery.sizeOf(context).width - 48).clamp(
      160.0,
      240.0,
    );
    return Scaffold(
      appBar: AppBar(
        title: Text(album?.album ?? 'Tracks'),
        actions: const [SizedBox(width: 48)],
      ),
      body: CustomScrollView(
        slivers: [
          if (album != null)
            SliverToBoxAdapter(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(24, 16, 24, 8),
                child: Center(
                  child: _AlbumCover(
                    key: const Key('albumCover'),
                    session: session,
                    album: album,
                    size: coverSize,
                  ),
                ),
              ),
            ),
          SliverList(
            delegate: SliverChildBuilderDelegate((context, index) {
              final track = session.tracks[index];
              return ListTile(
                leading: Text('${track.trackNumber ?? index + 1}'),
                title: Text(track.name),
                subtitle: Text(track.artist),
                onTap: () => session.play(track, queue: session.tracks),
                trailing: _TrackQueueMenu(session: session, track: track),
              );
            }, childCount: session.tracks.length),
          ),
        ],
      ),
    );
  }
}

class _TrackQueueMenu extends StatelessWidget {
  const _TrackQueueMenu({
    required this.session,
    required this.track,
    this.onGoToAlbum,
    this.onGoToArtist,
  });

  final Session session;
  final TrackSummary track;
  final Future<void> Function()? onGoToAlbum;
  final Future<void> Function()? onGoToArtist;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<String>(
      key: Key('trackMenu-${track.id}'),
      onSelected: (value) {
        switch (value) {
          case 'next':
            session.playNext(track);
          case 'queue':
            session.addToQueue(track);
          case 'album':
            onGoToAlbum?.call();
          case 'artist':
            onGoToArtist?.call();
        }
      },
      itemBuilder: (context) => [
        const PopupMenuItem(value: 'next', child: Text('Play next')),
        const PopupMenuItem(value: 'queue', child: Text('Add to queue')),
        if (onGoToAlbum != null)
          const PopupMenuItem(value: 'album', child: Text('Go to album')),
        if (onGoToArtist != null)
          const PopupMenuItem(value: 'artist', child: Text('Go to artist')),
      ],
    );
  }
}

class _NowPlayingBar extends StatefulWidget {
  const _NowPlayingBar({
    required this.session,
    required this.track,
    required this.onOpen,
  });

  final Session session;
  final TrackSummary track;
  final VoidCallback onOpen;

  @override
  State<_NowPlayingBar> createState() => _NowPlayingBarState();
}

class _NowPlayingBarState extends State<_NowPlayingBar> {
  double _dragDy = 0;

  void _onDragEnd(DragEndDetails details) {
    final flickedUp = (details.primaryVelocity ?? 0) < -200;
    final swipedUp = _dragDy < -48;
    _dragDy = 0;
    if (flickedUp || swipedUp) widget.onOpen();
  }

  @override
  Widget build(BuildContext context) {
    final textTheme = Theme.of(context).textTheme;
    return GestureDetector(
      onVerticalDragStart: (_) => _dragDy = 0,
      onVerticalDragUpdate: (details) => _dragDy += details.delta.dy,
      onVerticalDragEnd: _onDragEnd,
      onVerticalDragCancel: () => _dragDy = 0,
      child: Material(
        key: const Key('nowPlaying'),
        color: Theme.of(context).colorScheme.surfaceContainerHigh,
        child: SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(8, 8, 8, 4),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                InkWell(
                  key: const Key('nowPlayingTrack'),
                  onTap: widget.onOpen,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 8,
                      vertical: 4,
                    ),
                    child: Column(
                      children: [
                        Text(
                          widget.track.album,
                          textAlign: TextAlign.center,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: textTheme.titleMedium,
                        ),
                        Text(
                          widget.track.artist,
                          textAlign: TextAlign.center,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: textTheme.bodySmall,
                        ),
                      ],
                    ),
                  ),
                ),
                SeekBar(session: widget.session, compact: true),
                PlayerControls(session: widget.session, compact: true),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
