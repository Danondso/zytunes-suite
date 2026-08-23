import 'package:flutter/material.dart';

import 'screens/connect_screen.dart';
import 'screens/library_screen.dart';
import 'session.dart';
import 'theme.dart';

class ZytunesApp extends StatelessWidget {
  const ZytunesApp({super.key, required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'zytunes',
      theme: bedfellowTheme(),
      home: ListenableBuilder(
        listenable: session,
        builder: (context, _) {
          if (session.phase == SessionPhase.connected) {
            return LibraryScreen(
              key: const ValueKey('library'),
              session: session,
            );
          }
          return ConnectScreen(session: session);
        },
      ),
    );
  }
}
