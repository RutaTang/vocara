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

  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
