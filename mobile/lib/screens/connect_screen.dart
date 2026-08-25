import 'dart:io' show Platform;

import 'package:flutter/material.dart';

import '../api/client.dart';
import '../session.dart';
import '../storage.dart';
import '../theme.dart';

class ConnectScreen extends StatefulWidget {
  const ConnectScreen({super.key, required this.session});

  final Session session;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  final _host = TextEditingController(
    text: const String.fromEnvironment('ZYTUNES_HOST'),
  );
  final _port = TextEditingController(text: '9847');
  final _token = TextEditingController();
  var _prefilled = false;

  @override
  void initState() {
    super.initState();
    widget.session.addListener(_onSession);
    _maybePrefill(widget.session.saved);
  }

  @override
  void dispose() {
    widget.session.removeListener(_onSession);
    _host.dispose();
    _port.dispose();
    _token.dispose();
    super.dispose();
  }

  void _onSession() => _maybePrefill(widget.session.saved);

  void _maybePrefill(SavedServer? saved) {
    if (saved == null || _prefilled) return;
    _prefilled = true;
    _host.text = saved.host;
    _port.text = '${saved.port}';
    _token.text = saved.token ?? '';
  }

  Future<void> _connect() async {
    final port = int.tryParse(_port.text.trim()) ?? 9847;
    await widget.session.connect(
      host: _host.text,
      port: port,
      token: _token.text,
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Connect')),
      body: ListenableBuilder(
        listenable: widget.session,
        builder: (context, _) {
          final busy = widget.session.busy;
          return ListView(
            padding: const EdgeInsets.all(24),
            children: [
              ThemePicker(session: widget.session),
              const SizedBox(height: 24),
              ServerCredentialFields(
                host: _host,
                port: _port,
                token: _token,
              ),
              const SizedBox(height: 24),
              FilledButton(
                key: const Key('connectButton'),
                onPressed: busy
                    ? widget.session.cancelConnect
                    : _connect,
                child: Text(busy ? 'Cancel' : 'Connect'),
              ),
              if (widget.session.error != null) ...[
                const SizedBox(height: 16),
                Text(
                  widget.session.error!,
                  style: TextStyle(color: Theme.of(context).colorScheme.error),
                ),
              ],
            ],
          );
        },
      ),
    );
  }
}

class ThemePicker extends StatelessWidget {
  const ThemePicker({super.key, required this.session});

  final Session session;

  @override
  Widget build(BuildContext context) {
    return DropdownButtonFormField<String>(
      key: const Key('themePicker'),
      initialValue: resolveThemeId(session.themeId),
      decoration: const InputDecoration(labelText: 'Theme'),
      items: [
        for (final theme in appThemes)
          DropdownMenuItem(value: theme.id, child: Text(theme.name)),
      ],
      onChanged: (id) {
        if (id != null) session.setTheme(id);
      },
    );
  }
}

class ServerCredentialFields extends StatelessWidget {
  const ServerCredentialFields({
    super.key,
    required this.host,
    required this.port,
    required this.token,
    this.enabled = true,
  });

  final TextEditingController host;
  final TextEditingController port;
  final TextEditingController token;
  final bool enabled;

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        TextField(
          key: const Key('hostField'),
          controller: host,
          enabled: enabled,
          decoration: InputDecoration(
            labelText: 'Host',
            hintText: defaultConnectHostHint(android: Platform.isAndroid),
          ),
          keyboardType: TextInputType.url,
          autocorrect: false,
        ),
        const SizedBox(height: 12),
        TextField(
          key: const Key('portField'),
          controller: port,
          enabled: enabled,
          decoration: const InputDecoration(labelText: 'Port'),
          keyboardType: TextInputType.number,
        ),
        const SizedBox(height: 12),
        TextField(
          key: const Key('tokenField'),
          controller: token,
          enabled: enabled,
          obscureText: true,
          decoration: const InputDecoration(labelText: 'Token (optional)'),
        ),
      ],
    );
  }
}

Future<void> showServerSettings(BuildContext context, Session session) {
  return showDialog<void>(
    context: context,
    builder: (context) => ServerSettingsDialog(session: session),
  );
}

class ServerSettingsDialog extends StatefulWidget {
  const ServerSettingsDialog({super.key, required this.session});

  final Session session;

  @override
  State<ServerSettingsDialog> createState() => _ServerSettingsDialogState();
}

class _ServerSettingsDialogState extends State<ServerSettingsDialog> {
  late final TextEditingController _host;
  late final TextEditingController _port;
  late final TextEditingController _token;

  @override
  void initState() {
    super.initState();
    final saved = widget.session.saved;
    final client = widget.session.client;
    _host = TextEditingController(
      text: saved?.host ?? client?.baseUrl.host ?? '',
    );
    _port = TextEditingController(
      text: '${saved?.port ?? client?.baseUrl.port ?? 9847}',
    );
    _token = TextEditingController(text: saved?.token ?? client?.token ?? '');
  }

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _token.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    final port = int.tryParse(_port.text.trim()) ?? 9847;
    await widget.session.connect(
      host: _host.text,
      port: port,
      token: _token.text,
    );
    if (!mounted) return;
    if (widget.session.error == null) {
      Navigator.of(context).pop();
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.session,
      builder: (context, _) {
        final busy = widget.session.busy;
        return AlertDialog(
          title: const Text('Server'),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                ThemePicker(session: widget.session),
                const SizedBox(height: 16),
                ServerCredentialFields(
                  host: _host,
                  port: _port,
                  token: _token,
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(),
              child: const Text('Close'),
            ),
            FilledButton(
              key: const Key('settingsSaveButton'),
              onPressed: busy ? widget.session.cancelConnect : _save,
              child: Text(busy ? 'Cancel' : 'Save'),
            ),
          ],
        );
      },
    );
  }
}
