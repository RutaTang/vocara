import 'package:flutter/widgets.dart';

import '../../core/widgets/confirm_dialog.dart';

/// Shows a modal to name a new song practice. Returns the trimmed title, or
/// null if the user cancelled or left it empty. Dark studio styling (matches
/// the rest of the app's dialogs).
Future<String?> showNewPracticeDialog(BuildContext context) {
  return showStudioPrompt(
    context,
    title: 'New song practice',
    label: 'Title',
    hint: 'e.g. 海阔天空',
    confirmLabel: 'Create',
  );
}
