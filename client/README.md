# vocara

Vocara — a macOS app for singing practice notes and analysis

## Environments (build flavors)

The macOS app ships in three environments, each a **distinct bundle id** so they
coexist on one machine with separate sandbox stores (drift db + assets):

| Flavor | Bundle id                  | Purpose                          |
| ------ | -------------------------- | -------------------------------- |
| `dev`  | `com.rutatang.vocara.dev`  | Local development (default)      |
| `test` | `com.rutatang.vocara.test` | Automated e2e / integration runs |
| `prod` | `com.rutatang.vocara`      | Shipping build                   |

Builds are **flavor-only** — always pass `--flavor`:

```sh
flutter run   --flavor dev  -d macos     # or: just dev  /  just run dev
flutter build macos --flavor prod        # release a prod build
```

At runtime, dev/test show a coral `DEV` / `TEST` pill in the window's top bar and
suffix the window title (`Vocara — DEV`); prod shows neither. The active flavor
is read from Flutter's `appFlavor` in `lib/config/environment.dart`.

The e2e suite (`just e2e`) runs on the `test` flavor so it never touches dev/prod
data.

## Getting Started

This project is a starting point for a Flutter application.

A few resources to get you started if this is your first Flutter project:

- [Lab: Write your first Flutter app](https://docs.flutter.dev/get-started/codelab)
- [Cookbook: Useful Flutter samples](https://docs.flutter.dev/cookbook)

For help getting started with Flutter development, view the
[online documentation](https://docs.flutter.dev/), which offers tutorials,
samples, guidance on mobile development, and a full API reference.
