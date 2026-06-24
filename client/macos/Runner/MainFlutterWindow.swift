import AVFoundation
import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    // A DAW-style timeline needs width; give the window a sensible default.
    self.setContentSize(NSSize(width: 1440, height: 900))
    self.minSize = NSSize(width: 960, height: 600)
    self.center()

    // No white flash on launch: keep the window fully transparent until Flutter
    // has painted its first frame, then reveal it. Dart calls "show" on the
    // window channel from a post-first-frame callback; a fallback timer reveals
    // it anyway if that signal never arrives. The dark background + dark
    // appearance also guarantee the frame is never white at any point.
    self.alphaValue = 0
    self.backgroundColor = NSColor(
      srgbRed: 0x13 / 255.0, green: 0x13 / 255.0, blue: 0x16 / 255.0, alpha: 1.0)
    self.isOpaque = true
    self.appearance = NSAppearance(named: .darkAqua)
    self.contentView?.wantsLayer = true
    self.contentView?.layer?.backgroundColor = self.backgroundColor.cgColor

    RegisterGeneratedPlugins(registry: flutterViewController)

    let windowChannel = FlutterMethodChannel(
      name: "vocara/window",
      binaryMessenger: flutterViewController.engine.binaryMessenger)
    windowChannel.setMethodCallHandler { [weak self] call, result in
      if call.method == "show" {
        self?.reveal()
        result(nil)
      } else {
        result(FlutterMethodNotImplemented)
      }
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
      self?.reveal()
    }

    // Microphone permission. The native engine captures via cpal/CoreAudio (raw HAL), which is
    // fed SILENCE when the app lacks mic permission and — unlike AVFoundation — never triggers
    // the macOS prompt or adds the app to Privacy → Microphone. So request access through
    // AVFoundation here, invoked from a foreground user action (toggling monitor / recording).
    let micChannel = FlutterMethodChannel(
      name: "vocara/mic",
      binaryMessenger: flutterViewController.engine.binaryMessenger)
    micChannel.setMethodCallHandler { call, result in
      switch call.method {
      case "request":
        AVCaptureDevice.requestAccess(for: .audio) { granted in
          DispatchQueue.main.async { result(granted) }
        }
      case "status":
        result(AVCaptureDevice.authorizationStatus(for: .audio) == .authorized)
      default:
        result(FlutterMethodNotImplemented)
      }
    }

    super.awakeFromNib()
  }

  /// Fades the window in once (idempotent): used both by the first-frame signal
  /// from Dart and by the fallback timer.
  private func reveal() {
    guard self.alphaValue < 1 else { return }
    self.alphaValue = 1
    self.makeKeyAndOrderFront(nil)
    // Bring the app to the foreground too: if the user clicked away during the
    // pre-first-frame window, makeKeyAndOrderFront alone would leave us behind
    // the now-active app.
    NSApp.activate(ignoringOtherApps: true)
  }
}
