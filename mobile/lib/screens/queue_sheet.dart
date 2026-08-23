import 'package:flutter/material.dart';

import '../session.dart';

Future<void> showQueueSheet(BuildContext context, Session session) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    builder: (context) => SizedBox(
      height: MediaQuery.sizeOf(context).height * 0.65,
      child: QueueSheet(session: session),
    ),
  );
}

class QueueSheet extends StatelessWidget {
  const QueueSheet({super.key, required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: session,
      builder: (context, _) {
        return Column(
          key: const Key('queueSheet'),
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 16, 16, 8),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(
                  'Queue',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
            ),
            Expanded(
              child: ReorderableListView.builder(
                itemCount: session.queue.length,
                onReorder: (from, to) {
                  if (to > from) to--;
                  session.moveInQueue(from, to);
                },
                itemBuilder: (context, index) {
                  final track = session.queue[index];
                  final current = index == session.queueIndex;
                  return ListTile(
                    key: Key('queueTile-$index'),
                    leading: current
                        ? const Icon(Icons.play_arrow)
                        : Text('${index + 1}'),
                    title: Text(track.name),
                    subtitle: Text(track.artist),
                    selected: current,
                    onTap: () => session.playAt(index),
                    trailing: IconButton(
                      key: Key('queueRemove-$index'),
                      icon: const Icon(Icons.close),
                      onPressed: () => session.removeFromQueue(index),
                    ),
                  );
                },
              ),
            ),
          ],
        );
      },
    );
  }
}
