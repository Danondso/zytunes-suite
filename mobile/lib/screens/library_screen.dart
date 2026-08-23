import 'package:flutter/material.dart';

import '../api/models.dart';
import '../session.dart';
import 'connect_screen.dart';
import 'play_count_meter.dart';
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

class _ArtistListState extends State<_ArtistList> {
  final _search = TextEditingController();

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
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
                    : ListView.builder(
                        keyboardDismissBehavior:
                            ScrollViewKeyboardDismissBehavior.onDrag,
                        itemCount: session.artists.length,
                        itemBuilder: (context, index) {
                          final artist = session.artists[index];
                          return ListTile(
                            title: Text(artist),
                            onTap: () => _openArtist(context, session, artist),
                          );
                        },
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

class _NowPlayingBar extends StatelessWidget {
  const _NowPlayingBar({
    required this.session,
    required this.track,
    required this.onOpen,
  });

  final Session session;
  final TrackSummary track;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final textTheme = Theme.of(context).textTheme;
    return Material(
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
                onTap: onOpen,
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 8,
                    vertical: 4,
                  ),
                  child: Column(
                    children: [
                      Text(
                        track.name,
                        textAlign: TextAlign.center,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: textTheme.titleMedium,
                      ),
                      Text(
                        track.artist,
                        textAlign: TextAlign.center,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: textTheme.bodySmall,
                      ),
                      if (session.displayedPlayCount != null)
                        PlayCountMeter(
                          key: ValueKey(track.id),
                          count: session.displayedPlayCount!,
                          compact: true,
                        ),
                    ],
                  ),
                ),
              ),
              SeekBar(session: session, compact: true),
              PlayerControls(session: session, compact: true),
            ],
          ),
        ),
      ),
    );
  }
}
