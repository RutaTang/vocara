import 'package:flutter/widgets.dart';
import 'package:go_router/go_router.dart';

/// Leaves the current page: pops it when a page lies beneath, otherwise goes
/// to [fallback].
///
/// Pop is the normal case, and the one that matters. A screen that pushed
/// this page awaits the push (Home and the technique library reload their
/// lists and release their double-open guards when it completes), and
/// go_router completes a push future ONLY on pop — `go` drops the pushed page
/// without ever completing it, which left Home's open guard set for good.
///
/// [fallback] covers a page that is the root of the stack (a dev
/// `--dart-define=START=` deep link), where a pop throws "There is nothing to
/// pop" and the back affordance would do nothing at all.
void popOrGo(BuildContext context, String fallback) {
  if (context.canPop()) {
    context.pop();
  } else {
    context.go(fallback);
  }
}
