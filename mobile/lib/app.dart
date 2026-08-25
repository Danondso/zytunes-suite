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
    return ListenableBuilder(
      listenable: session,
      builder: (context, _) {
        return MaterialApp(
          title: 'zytunes',
          theme: themeDataFor(session.themeId),
          home: session.phase == SessionPhase.connected
              ? LibraryScreen(
                  key: const ValueKey('library'),
                  session: session,
                )
              : ConnectScreen(session: session),
        );
      },
    );
  }
}
