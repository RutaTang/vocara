#
# Builds + embeds the vocara_engine Rust framework into the macOS Runner. Rides the existing
# `use_frameworks!` embed/re-sign phase — no .pbxproj surgery. `prepare_command` builds the
# framework at `pod install`; the `before_compile` script phase rebuilds it each build so
# Rust edits are picked up.
#
Pod::Spec.new do |s|
  s.name             = 'vocara_engine'
  s.version          = '0.1.0'
  s.summary          = 'Vocara real-time DAG audio engine (Rust).'
  s.description      = <<-DESC
    Native real-time audio graph engine: EQ / compressor / Freeverb, a shared vocal reverb
    send bus, a sample-counter transport, cpal CoreAudio output and symphonia decode.
  DESC
  s.homepage         = 'https://vocara.app'
  s.license          = { :type => 'MIT' }
  s.author           = { 'Vocara' => 'dev@vocara.app' }
  s.source           = { :path => '.' }
  s.platform         = :osx, '10.15'

  s.vendored_frameworks = 'Frameworks/vocara_engine.framework'
  s.prepare_command  = 'bash build_rust.sh'
  s.script_phase = {
    :name => 'Build vocara_engine (Rust)',
    :script => 'bash "${PODS_TARGET_SRCROOT}/build_rust.sh"',
    :execution_position => :before_compile,
  }
end
