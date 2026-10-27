import AVFoundation
import Cocoa
import FlutterMacOS

@main
class AppDelegate: FlutterAppDelegate {
  override func applicationDidFinishLaunching(_ notification: Notification) {
    // The native Rust engine captures the mic via CoreAudio (cpal). Unlike AVFoundation,
    // CoreAudio access doesn't reliably trigger the macOS microphone permission prompt, so a
    // permission-denied input silently returns zeros. Request access here so the prompt appears
    // (and an existing grant is honored) before any capture starts.
    AVCaptureDevice.requestAccess(for: .audio) { _ in }
    super.applicationDidFinishLaunching(notification)
  }

  /// When a quit began waiting on the Dart side's exit guard, which finishes any in-flight save
  /// before it answers. Nil when none has.
  private var quitRequestedAt: Date?
  /// Set by the Dart side ("exitApproved" on vocara/window) once its exit guard is done: the
  /// terminate the engine then asks for goes through.
  var exitApproved = false
  /// Set while the engine handles a quit: before the Dart side takes quit requests the engine
  /// terminates at once, calling back in here, and that must go through.
  private var askingEngine = false

  /// FlutterAppDelegate asks the Dart side before quitting, but marks the quit approved the
  /// moment it asks: a second Cmd+Q or Dock Quit while the exit guard was still finishing a save
  /// quit at once and cut the save off. A repeat request now waits for the guard's answer,
  /// for at most a little longer than the guard's own longest wait (60 s), so a guard that never
  /// answers cannot keep the app from quitting.
  override func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    if exitApproved || askingEngine {
      return .terminateNow
    }
    if let since = quitRequestedAt, Date().timeIntervalSince(since) < 65 {
      return .terminateCancel
    }
    quitRequestedAt = Date()
    askingEngine = true
    defer { askingEngine = false }
    return super.applicationShouldTerminate(sender)
  }

  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
