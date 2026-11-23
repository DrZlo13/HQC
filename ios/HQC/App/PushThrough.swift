import SwiftUI

/// How a full screen leaves when the app takes over (the launch splash): instead of a
/// cross-fade, which reads as a cut, it pushes *through* the viewer, scaling up and blurring
/// out as it goes transparent, while the app settles up to full size underneath (the caller
/// scales it up from 0.94). The departing view must stay on top (`zIndex`), or the ZStack
/// draws the app over it and the animation plays where nobody sees it.
extension AnyTransition {
    static var pushThrough: AnyTransition {
        .modifier(active: PushThrough(progress: 1), identity: PushThrough(progress: 0))
    }
}

/// `Animatable`, so SwiftUI drives `progress` frame by frame and the opacity curve below is
/// applied, rather than animating each effect straight between its two ends.
private struct PushThrough: ViewModifier, Animatable {
    var progress: Double

    var animatableData: Double {
        get { progress }
        set { progress = newValue }
    }

    func body(content: Content) -> some View {
        content
            .scaleEffect(1 + 0.18 * progress)
            .blur(radius: 12 * progress)
            // Squared: a straight fade is gone before the scale reads; this keeps the screen
            // legible through the first half and lets it drop away at the end.
            .opacity(1 - progress * progress)
    }
}
