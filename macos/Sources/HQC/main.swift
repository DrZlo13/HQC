import AppKit

// Application entry point. We build everything programmatically (no storyboard / xib),
// so we create the shared NSApplication, install our delegate, and start the run loop.
let app = NSApplication.shared
app.setActivationPolicy(.regular)

let delegate = AppDelegate()
app.delegate = delegate

app.run()
